//! Turning a document's mesh *recipes* into triangles.
//!
//! Model documents store recipes ("extrude doc_logo:#mark, depth 12, bevel 1.5"), not vertex
//! soup, so a mesh regenerates whenever its source path changes. Baked geometry — the result
//! of welding, decimating or importing — lives in the asset store as a `DPM1` blob and comes
//! back through [`MeshSource::Buffer`].

use crate::extrude::{loft_sections, polylines_of, revolve_profile, rings_of};
use crate::geom::{trs_matrix, MeshData, IDENTITY4};
use crate::prim::primitive;
use dpaint_core::doc::common::FillRule;
use dpaint_core::doc::model::{MeshSource, ModelDoc, PathRef};
use dpaint_core::kurbo::BezPath;
use dpaint_core::{AssetRef, AssetStore, DocId, Error, MaterialId, MeshId, NodeId, Project, Result};

/// Magic for the baked-mesh blob: `positions`, optional `normals`/`uvs`/`tangents`, `u32`
/// indices, little-endian throughout, so a bake is byte-identical across runs and machines.
const BLOB_MAGIC: &[u8; 4] = b"DPM1";
const FLAG_NORMALS: u32 = 1;
const FLAG_UVS: u32 = 2;
const FLAG_TANGENTS: u32 = 4;

pub fn encode_blob(mesh: &MeshData, tangents: Option<&[[f32; 4]]>) -> Vec<u8> {
    let v = mesh.positions.len();
    let mut flags = 0;
    if mesh.normals.len() == v {
        flags |= FLAG_NORMALS;
    }
    if mesh.uvs.len() == v {
        flags |= FLAG_UVS;
    }
    let tangents = tangents.filter(|t| t.len() == v);
    if tangents.is_some() {
        flags |= FLAG_TANGENTS;
    }
    let mut out = Vec::with_capacity(16 + v * 32 + mesh.indices.len() * 4);
    out.extend_from_slice(BLOB_MAGIC);
    out.extend_from_slice(&flags.to_le_bytes());
    out.extend_from_slice(&(v as u32).to_le_bytes());
    out.extend_from_slice(&(mesh.indices.len() as u32).to_le_bytes());
    for p in &mesh.positions {
        for c in p {
            out.extend_from_slice(&c.to_le_bytes());
        }
    }
    if flags & FLAG_NORMALS != 0 {
        for n in &mesh.normals {
            for c in n {
                out.extend_from_slice(&c.to_le_bytes());
            }
        }
    }
    if flags & FLAG_UVS != 0 {
        for uv in &mesh.uvs {
            for c in uv {
                out.extend_from_slice(&c.to_le_bytes());
            }
        }
    }
    if let Some(t) = tangents {
        for tan in t {
            for c in tan {
                out.extend_from_slice(&c.to_le_bytes());
            }
        }
    }
    for i in &mesh.indices {
        out.extend_from_slice(&i.to_le_bytes());
    }
    out
}

pub fn decode_blob(bytes: &[u8]) -> Result<(MeshData, Vec<[f32; 4]>)> {
    if bytes.len() < 16 || &bytes[..4] != BLOB_MAGIC {
        return Err(Error::AssetDecode(
            "not a degen-paint mesh blob (bad magic)".into(),
        ));
    }
    let u32_at = |off: usize| -> u32 {
        u32::from_le_bytes([bytes[off], bytes[off + 1], bytes[off + 2], bytes[off + 3]])
    };
    let flags = u32_at(4);
    let vcount = u32_at(8) as usize;
    let icount = u32_at(12) as usize;
    let mut off = 16;
    let need = 12 * vcount
        + if flags & FLAG_NORMALS != 0 { 12 * vcount } else { 0 }
        + if flags & FLAG_UVS != 0 { 8 * vcount } else { 0 }
        + if flags & FLAG_TANGENTS != 0 { 16 * vcount } else { 0 }
        + 4 * icount;
    if bytes.len() < off + need {
        return Err(Error::AssetDecode(format!(
            "mesh blob truncated: {} bytes, expected {}",
            bytes.len(),
            off + need
        )));
    }
    let f32_at = |off: &mut usize| -> f32 {
        let v = f32::from_le_bytes([bytes[*off], bytes[*off + 1], bytes[*off + 2], bytes[*off + 3]]);
        *off += 4;
        v
    };
    let mut mesh = MeshData::default();
    for _ in 0..vcount {
        mesh.positions
            .push([f32_at(&mut off), f32_at(&mut off), f32_at(&mut off)]);
    }
    if flags & FLAG_NORMALS != 0 {
        for _ in 0..vcount {
            mesh.normals
                .push([f32_at(&mut off), f32_at(&mut off), f32_at(&mut off)]);
        }
    }
    if flags & FLAG_UVS != 0 {
        for _ in 0..vcount {
            mesh.uvs.push([f32_at(&mut off), f32_at(&mut off)]);
        }
    }
    let mut tangents = Vec::new();
    if flags & FLAG_TANGENTS != 0 {
        for _ in 0..vcount {
            tangents.push([
                f32_at(&mut off),
                f32_at(&mut off),
                f32_at(&mut off),
                f32_at(&mut off),
            ]);
        }
    }
    for _ in 0..icount {
        let v = u32::from_le_bytes([bytes[off], bytes[off + 1], bytes[off + 2], bytes[off + 3]]);
        off += 4;
        mesh.indices.push(v);
    }
    let max = mesh.positions.len() as u32;
    if mesh.indices.iter().any(|i| *i >= max) {
        return Err(Error::AssetDecode(
            "mesh blob has an index past the end of its vertex list".into(),
        ));
    }
    Ok((mesh, tangents))
}

/// The outline of a vector object plus the fill rule it was authored with — the rule decides
/// whether a nested contour is a hole.
pub fn path_and_rule(project: &Project, pref: &PathRef) -> Result<(BezPath, FillRule)> {
    let path = dpaint_vector::path_of(project, &pref.document, &pref.object)?;
    let rule = project
        .vector(&pref.document)
        .ok()
        .and_then(|d| d.object(&pref.object))
        .map(|o| o.fill_rule)
        .unwrap_or(FillRule::Nonzero);
    Ok((path, rule))
}

/// Build one mesh's geometry.
///
/// Procedural sources regenerate from their recipe; a [`MeshSource::Buffer`] is decoded from
/// the asset store.
pub fn build_mesh(
    project: &Project,
    doc: &DocId,
    mesh_id: &MeshId,
    assets: &AssetStore,
) -> Result<MeshData> {
    let model = project.model(doc)?;
    let mesh = model
        .mesh(mesh_id)
        .ok_or_else(|| Error::Invalid(format!("no mesh '{mesh_id}' in {doc}")))?;
    build_source(project, &mesh.source, assets)
}

pub(crate) fn build_source(
    project: &Project,
    source: &MeshSource,
    assets: &AssetStore,
) -> Result<MeshData> {
    match source {
        MeshSource::Primitive { shape, size, segments } => primitive(*shape, *size, *segments),
        MeshSource::Extrude { from, depth, bevel, caps, flatten } => {
            let (path, rule) = path_and_rule(project, from)?;
            crate::extrude::extrude_rings(
                rings_of(&path, *flatten),
                *depth,
                *bevel,
                *caps,
                rule,
                *flatten,
            )
        }
        MeshSource::Revolve { from, angle, segments, flatten } => {
            let (path, _) = path_and_rule(project, from)?;
            let profiles = polylines_of(&path, *flatten);
            let profile = profiles.into_iter().next().ok_or_else(|| {
                Error::DegenerateGeometry("revolve source path is empty".into())
            })?;
            revolve_profile(&profile, *angle, *segments)
        }
        MeshSource::Loft { sections, flatten } => {
            let mut rule = FillRule::Nonzero;
            let mut rings = Vec::with_capacity(sections.len());
            for s in sections {
                let (path, r) = path_and_rule(project, s)?;
                rule = r;
                rings.push(rings_of(&path, *flatten));
            }
            loft_sections(rings, rule, *flatten)
        }
        MeshSource::Buffer { asset } => Ok(decode_blob(&assets.get(asset)?)?.0),
    }
}

/// Per-vertex tangents for a mesh: the baked ones when `model.mesh.generate-tangents` has
/// stored them, mikktspace on the fly otherwise.
pub fn mesh_tangents(
    project: &Project,
    doc: &DocId,
    mesh_id: &MeshId,
    assets: &AssetStore,
) -> Result<Vec<[f32; 4]>> {
    let model = project.model(doc)?;
    let mesh = model
        .mesh(mesh_id)
        .ok_or_else(|| Error::Invalid(format!("no mesh '{mesh_id}' in {doc}")))?;
    if let MeshSource::Buffer { asset } = &mesh.source {
        let (data, tangents) = decode_blob(&assets.get(asset)?)?;
        if !tangents.is_empty() {
            return Ok(tangents);
        }
        return Ok(crate::uv::tangents(&data));
    }
    let data = build_source(project, &mesh.source, assets)?;
    Ok(crate::uv::tangents(&data))
}

/// Every drawable in the scene with its world matrix (column-major, `m[col][row]`).
pub fn scene_meshes(
    project: &Project,
    doc: &DocId,
    assets: &AssetStore,
) -> Result<Vec<(NodeId, MeshData, Option<MaterialId>, [[f32; 4]; 4])>> {
    let model = project.model(doc)?;
    let mut out = Vec::new();
    let mut cache: std::collections::HashMap<MeshId, MeshData> = std::collections::HashMap::new();
    for (node_id, world) in world_transforms(model) {
        let Some(node) = model.node(&node_id) else { continue };
        let Some(mesh_id) = node.mesh.clone() else { continue };
        let data = match cache.get(&mesh_id) {
            Some(d) => d.clone(),
            None => {
                let d = build_mesh(project, doc, &mesh_id, assets)?;
                cache.insert(mesh_id.clone(), d.clone());
                d
            }
        };
        out.push((node_id, data, node.material.clone(), world));
    }
    Ok(out)
}

/// Depth-first walk of the node hierarchy, accumulating world matrices. Nodes reachable
/// through more than one parent are visited once, from the first parent in document order.
pub fn world_transforms(model: &ModelDoc) -> Vec<(NodeId, [[f32; 4]; 4])> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    fn visit(
        model: &ModelDoc,
        id: &NodeId,
        parent: [[f32; 4]; 4],
        seen: &mut std::collections::HashSet<NodeId>,
        out: &mut Vec<(NodeId, [[f32; 4]; 4])>,
    ) {
        if !seen.insert(id.clone()) {
            return;
        }
        let Some(node) = model.node(id) else { return };
        let local = trs_matrix(node.translation, node.rotation, node.scale);
        let world = crate::geom::mat_mul(&parent, &local);
        out.push((id.clone(), world));
        for child in &node.children {
            visit(model, child, world, seen, out);
        }
    }
    for root in model.roots() {
        visit(model, &root.id, IDENTITY4, &mut seen, &mut out);
    }
    // Nodes in a parent cycle would otherwise vanish; surface them at the root.
    for n in &model.nodes {
        if !seen.contains(&n.id) {
            visit(model, &n.id, IDENTITY4, &mut seen, &mut out);
        }
    }
    out
}

/// Import triangles from a `.glb` / self-contained `.gltf` byte stream, merging every
/// primitive of every mesh into one [`MeshData`].
pub fn import_gltf(bytes: &[u8]) -> Result<MeshData> {
    let gltf = gltf::Gltf::from_slice(bytes)
        .map_err(|e| Error::AssetDecode(format!("glTF import failed: {e}")))?;
    let blob = gltf.blob.clone();
    let mut buffers: Vec<Vec<u8>> = Vec::new();
    for buffer in gltf.buffers() {
        match buffer.source() {
            gltf::buffer::Source::Bin => buffers.push(blob.clone().ok_or_else(|| {
                Error::AssetDecode("glTF references its BIN chunk but none is present".into())
            })?),
            gltf::buffer::Source::Uri(uri) => {
                let data = uri
                    .strip_prefix("data:")
                    .and_then(|rest| rest.split_once(";base64,"))
                    .map(|(_, b64)| {
                        use base64::Engine;
                        base64::engine::general_purpose::STANDARD
                            .decode(b64)
                            .map_err(|e| Error::AssetDecode(format!("bad data uri: {e}")))
                    })
                    .transpose()?
                    .ok_or_else(|| {
                        Error::UnsupportedFormat(format!(
                            "glTF buffer '{uri}' is an external file; import a .glb or a \
                             glTF with embedded buffers"
                        ))
                    })?;
                buffers.push(data);
            }
        }
    }
    let mut out = MeshData::default();
    for mesh in gltf.document.meshes() {
        for prim in mesh.primitives() {
            let reader = prim.reader(|b| buffers.get(b.index()).map(|v| v.as_slice()));
            let Some(positions) = reader.read_positions() else { continue };
            let mut part = MeshData {
                positions: positions.collect(),
                ..Default::default()
            };
            if let Some(n) = reader.read_normals() {
                part.normals = n.collect();
            }
            if let Some(uv) = reader.read_tex_coords(0) {
                part.uvs = uv.into_f32().collect();
            }
            part.indices = match reader.read_indices() {
                Some(i) => i.into_u32().collect(),
                None => (0..part.positions.len() as u32).collect(),
            };
            if part.normals.len() != part.positions.len() {
                part.normals.clear();
                part.recompute_normals(60.0);
            }
            out.append(&part);
        }
    }
    if out.indices.is_empty() {
        return Err(Error::AssetDecode(
            "glTF contains no triangle geometry".into(),
        ));
    }
    Ok(out)
}

/// Bake geometry into the asset store and return the reference to store on the mesh.
pub fn bake(assets: &AssetStore, mesh: &MeshData, tangents: Option<&[[f32; 4]]>) -> Result<AssetRef> {
    assets.put(&encode_blob(mesh, tangents), "dpm")
}
