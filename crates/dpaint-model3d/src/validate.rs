//! `model.validate`: does this document export as conformant glTF, and is its geometry sane?

use crate::build::build_mesh;
use crate::export::{image_size, structural_errors, TextureResolver};
use crate::geom::{cross, length, pos_key, sub, MeshData, WELD_TOL};
use dpaint_core::doc::model::TextureSource;
use dpaint_core::{AssetStore, DocId, Project, Result};
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
    Info,
}

#[derive(Debug, Clone, Serialize)]
pub struct Finding {
    /// Stable machine code an agent can branch on.
    pub code: &'static str,
    pub severity: Severity,
    /// Id of the offending object, or the document when it is document-wide.
    pub target: String,
    pub detail: String,
}

impl Finding {
    fn new(code: &'static str, severity: Severity, target: impl Into<String>, detail: impl Into<String>) -> Self {
        Self { code, severity, target: target.into(), detail: detail.into() }
    }
}

/// Edge topology of one mesh, on welded positions so independently built caps and walls
/// count as joined.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Topology {
    /// Edges used by exactly one triangle: an open boundary.
    pub boundary_edges: usize,
    /// Edges used by three or more triangles: not a manifold surface.
    pub non_manifold_edges: usize,
    /// Edges whose two triangles disagree about which way round they go.
    pub inconsistent_edges: usize,
}

pub fn topology(mesh: &MeshData) -> Topology {
    let welded: Vec<u64> = {
        let mut map = std::collections::HashMap::new();
        mesh.positions
            .iter()
            .map(|p| {
                let k = pos_key(*p, WELD_TOL);
                let next = map.len() as u64;
                *map.entry(k).or_insert(next)
            })
            .collect()
    };
    // (min, max) -> (uses, forward uses)
    let mut edges: std::collections::HashMap<(u64, u64), (usize, i32)> =
        std::collections::HashMap::new();
    for t in mesh.indices.chunks_exact(3) {
        for k in 0..3 {
            let a = welded[t[k] as usize];
            let b = welded[t[(k + 1) % 3] as usize];
            if a == b {
                continue;
            }
            let e = edges.entry((a.min(b), a.max(b))).or_insert((0, 0));
            e.0 += 1;
            e.1 += if a < b { 1 } else { -1 };
        }
    }
    let mut out = Topology::default();
    for (uses, dir) in edges.values() {
        match uses {
            1 => out.boundary_edges += 1,
            2 => {
                if *dir != 0 {
                    out.inconsistent_edges += 1;
                }
            }
            _ => out.non_manifold_edges += 1,
        }
    }
    out
}

fn degenerate_triangles(mesh: &MeshData, epsilon: f32) -> usize {
    mesh.indices
        .chunks_exact(3)
        .filter(|t| {
            let (a, b, c) = (
                mesh.positions[t[0] as usize],
                mesh.positions[t[1] as usize],
                mesh.positions[t[2] as usize],
            );
            t[0] == t[1]
                || t[1] == t[2]
                || t[0] == t[2]
                || length(cross(sub(b, a), sub(c, a))) * 0.5 <= epsilon
        })
        .count()
}

/// Run every check. `max_texture_size` is the largest edge in pixels a texture may have
/// before it is reported.
pub fn validate(
    project: &Project,
    doc: &DocId,
    assets: &AssetStore,
    textures: &TextureResolver<'_>,
    max_texture_size: u32,
) -> Result<Vec<Finding>> {
    let model = project.model(doc)?;
    let mut out = Vec::new();

    match structural_errors(project, doc, assets, textures) {
        Ok(errors) => {
            for e in errors {
                out.push(Finding::new(
                    "gltf-structural",
                    Severity::Error,
                    doc.to_string(),
                    e,
                ));
            }
        }
        Err(e) => out.push(Finding::new(
            "gltf-export-failed",
            Severity::Error,
            doc.to_string(),
            e.to_string(),
        )),
    }

    // Which meshes carry a texture-bound material, so a missing UV set is fatal for them.
    let mut textured_meshes: std::collections::BTreeSet<String> = Default::default();
    for node in &model.nodes {
        if let (Some(mesh), Some(mat)) = (&node.mesh, &node.material) {
            if model.material(mat).map(|m| !m.textures.is_empty()).unwrap_or(false) {
                textured_meshes.insert(mesh.to_string());
            }
        }
        if let Some(mesh) = &node.mesh {
            if model.mesh(mesh).is_none() {
                out.push(Finding::new(
                    "missing-mesh",
                    Severity::Error,
                    node.id.to_string(),
                    format!("node references mesh '{mesh}', which does not exist"),
                ));
            }
        }
        if let Some(mat) = &node.material {
            if model.material(mat).is_none() {
                out.push(Finding::new(
                    "missing-material",
                    Severity::Error,
                    node.id.to_string(),
                    format!("node references material '{mat}', which does not exist"),
                ));
            }
        }
        if let Some(l) = &node.light {
            if !model.lights.iter().any(|x| &x.id == l) {
                out.push(Finding::new(
                    "missing-light",
                    Severity::Error,
                    node.id.to_string(),
                    format!("node references light '{l}', which does not exist"),
                ));
            }
        }
        if let Some(c) = &node.camera {
            if !model.cameras.iter().any(|x| &x.id == c) {
                out.push(Finding::new(
                    "missing-camera",
                    Severity::Error,
                    node.id.to_string(),
                    format!("node references camera '{c}', which does not exist"),
                ));
            }
        }
    }

    for mesh in &model.meshes {
        let data = match build_mesh(project, doc, &mesh.id, assets) {
            Ok(d) => d,
            Err(e) => {
                out.push(Finding::new(
                    "mesh-build-failed",
                    Severity::Error,
                    mesh.id.to_string(),
                    e.to_string(),
                ));
                continue;
            }
        };
        let topo = topology(&data);
        if topo.non_manifold_edges > 0 {
            out.push(Finding::new(
                "non-manifold",
                Severity::Error,
                mesh.id.to_string(),
                format!(
                    "{} edge(s) shared by three or more triangles",
                    topo.non_manifold_edges
                ),
            ));
        }
        if topo.inconsistent_edges > 0 {
            out.push(Finding::new(
                "inconsistent-winding",
                Severity::Warning,
                mesh.id.to_string(),
                format!(
                    "{} edge(s) whose two triangles wind in opposite directions",
                    topo.inconsistent_edges
                ),
            ));
        }
        if topo.boundary_edges > 0 {
            out.push(Finding::new(
                "open-surface",
                Severity::Info,
                mesh.id.to_string(),
                format!("{} boundary edge(s); the mesh is not a closed solid", topo.boundary_edges),
            ));
        }
        let degenerate = degenerate_triangles(&data, 1e-12);
        if degenerate > 0 {
            out.push(Finding::new(
                "degenerate-triangles",
                Severity::Warning,
                mesh.id.to_string(),
                format!("{degenerate} triangle(s) with zero area"),
            ));
        }
        if data.normals.len() != data.positions.len() {
            out.push(Finding::new(
                "missing-normals",
                Severity::Warning,
                mesh.id.to_string(),
                "mesh has no per-vertex normals".to_string(),
            ));
        }
        if textured_meshes.contains(mesh.id.as_str()) && data.uvs.len() != data.positions.len() {
            out.push(Finding::new(
                "missing-uv",
                Severity::Error,
                mesh.id.to_string(),
                "a texture is bound to this mesh's material but the mesh has no UVs".to_string(),
            ));
        }
    }

    for mat in &model.materials {
        for binding in &mat.textures {
            let bytes = match &binding.source {
                TextureSource::Asset { asset } => match assets.get(asset) {
                    Ok(b) => Some(b),
                    Err(e) => {
                        out.push(Finding::new(
                            "texture-unresolved",
                            Severity::Error,
                            mat.id.to_string(),
                            format!("{:?} texture: {e}", binding.slot),
                        ));
                        None
                    }
                },
                TextureSource::Document { document } => match project.doc(document) {
                    Ok(d) if d.as_raster().is_some() => match (textures)(document) {
                        Ok(b) => Some(b),
                        Err(e) => {
                            out.push(Finding::new(
                                "texture-unresolved",
                                Severity::Warning,
                                mat.id.to_string(),
                                format!("{:?} texture from {document}: {e}", binding.slot),
                            ));
                            None
                        }
                    },
                    Ok(d) => {
                        out.push(Finding::new(
                            "texture-unresolved",
                            Severity::Error,
                            mat.id.to_string(),
                            format!(
                                "{:?} texture points at {document}, which is a {} document, not a raster one",
                                binding.slot,
                                d.kind()
                            ),
                        ));
                        None
                    }
                    Err(e) => {
                        out.push(Finding::new(
                            "texture-unresolved",
                            Severity::Error,
                            mat.id.to_string(),
                            format!("{:?} texture: {e}", binding.slot),
                        ));
                        None
                    }
                },
            };
            if let Some(bytes) = bytes {
                match image_size(&bytes) {
                    Some((w, h)) if w.max(h) > max_texture_size => out.push(Finding::new(
                        "texture-oversized",
                        Severity::Warning,
                        mat.id.to_string(),
                        format!(
                            "{:?} texture is {w}x{h}, larger than the {max_texture_size}px limit",
                            binding.slot
                        ),
                    )),
                    None => out.push(Finding::new(
                        "texture-unsupported",
                        Severity::Error,
                        mat.id.to_string(),
                        format!(
                            "{:?} texture is neither PNG nor JPEG; glTF images must be one of those",
                            binding.slot
                        ),
                    )),
                    _ => {}
                }
            }
        }
    }

    for anim in &model.animations {
        for ch in &anim.channels {
            if model.node(&ch.node).is_none() {
                out.push(Finding::new(
                    "missing-node",
                    Severity::Error,
                    anim.id.to_string(),
                    format!("animation channel targets node '{}', which does not exist", ch.node),
                ));
            }
        }
    }

    Ok(out)
}
