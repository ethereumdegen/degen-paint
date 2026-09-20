//! `model.mesh.*` — procedural geometry, the cross-mode bridge, and the baking verbs.

use super::{all_of_type, bake_mesh, declare_op, one_of_type, target_model, unique_id};
use crate::build::{bake, build_mesh, import_gltf};
use crate::geom::{trs_matrix, MeshData};
use dpaint_core::doc::model::{Bevel, Caps, Mesh, MeshSource, Node, PathRef, Primitive};
use dpaint_core::{
    resolve_one, AssetRef, DocId, Error, MaterialId, MeshId, NodeId, ObjectId, OpCx, OpEffect,
    Project, Result,
};
use serde::Deserialize;

/// Object kinds `dpaint_vector::path_of` can turn into an outline.
const OUTLINE_KINDS: &[&str] = &[
    "path", "rect", "ellipse", "polygon", "star", "line", "text", "group",
];

fn resolve_path_ref(
    project: &Project,
    selector: &str,
    document: Option<&str>,
    require_text: bool,
) -> Result<PathRef> {
    let default = document.map(|d| project.resolve_doc(Some(d))).transpose()?;
    let m = resolve_one(project, selector, default.as_ref())?;
    if require_text && m.type_name != "text" {
        return Err(Error::Invalid(format!(
            "'{selector}' matched a {} named '{}'; model.mesh.from-text needs a text object",
            m.type_name, m.name
        )));
    }
    if !OUTLINE_KINDS.contains(&m.type_name.as_str()) {
        return Err(Error::Invalid(format!(
            "'{selector}' matched a {}, which has no outline to build geometry from",
            m.type_name
        )));
    }
    project.vector(&m.document)?;
    Ok(PathRef {
        document: m.document,
        object: ObjectId::from(m.id),
    })
}

/// Insert a mesh, and optionally a node that draws it, reporting both as created.
fn install(
    project: &mut Project,
    doc: &DocId,
    name: &str,
    source: MeshSource,
    make_node: bool,
    material: Option<MaterialId>,
) -> Result<OpEffect> {
    let model = project.model_mut(doc)?;
    let taken: Vec<String> = model.meshes.iter().map(|m| m.id.to_string()).collect();
    let mesh_id = unique_id(name, |s| MeshId::from_name(s), &taken);
    model.meshes.push(Mesh {
        id: mesh_id.clone(),
        name: name.to_string(),
        source,
    });
    let mut effect = OpEffect::changed(doc).with_created(mesh_id.to_string());
    if make_node {
        let taken: Vec<String> = model.nodes.iter().map(|n| n.id.to_string()).collect();
        let node_id = unique_id(name, |s| NodeId::from_name(s), &taken);
        let mut node = Node::new(node_id.clone(), name.to_string());
        node.mesh = Some(mesh_id);
        node.material = material;
        model.nodes.push(node);
        effect = effect.with_created(node_id.to_string());
    }
    Ok(effect)
}

fn bevel_of(size: Option<f32>, segments: u32) -> Result<Option<Bevel>> {
    match size {
        None => Ok(None),
        Some(s) if s <= 0.0 => Ok(None),
        Some(s) if !s.is_finite() => Err(Error::Invalid(format!("bevel size {s} is not finite"))),
        Some(s) => Ok(Some(Bevel {
            size: s,
            segments: segments.clamp(1, 64),
        })),
    }
}

fn default_size() -> [f32; 3] {
    [1.0, 1.0, 1.0]
}

fn default_segments() -> u32 {
    32
}

fn default_flatten() -> f64 {
    0.25
}

fn yes() -> bool {
    true
}

fn three() -> u32 {
    3
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PrimitiveArgs {
    /// Which primitive to build.
    pub shape: Primitive,
    /// Name for the mesh and its node.
    #[serde(default)]
    pub name: Option<String>,
    /// Full extents on each axis. For a torus, `size.x` is the ring diameter and the tube
    /// diameter is `size.y / 2`.
    #[serde(default = "default_size")]
    pub size: [f32; 3],
    /// Angular or grid resolution. Ignored by `box`, whose six flat faces never subdivide.
    #[serde(default = "default_segments")]
    pub segments: u32,
    /// Also create a node that draws the mesh.
    #[serde(default = "yes")]
    pub node: bool,
    /// Selector for the material the new node uses.
    #[serde(default)]
    pub material: Option<String>,
}

fn mesh_primitive(project: &mut Project, args: PrimitiveArgs, cx: &mut OpCx) -> Result<OpEffect> {
    let doc = target_model(project, cx)?;
    let material = args
        .material
        .as_deref()
        .map(|s| one_of_type(project, s, &doc, "material").map(MaterialId::from))
        .transpose()?;
    // Build once up front: a bad size must fail before the document is touched.
    let source = MeshSource::Primitive {
        shape: args.shape,
        size: args.size,
        segments: args.segments,
    };
    crate::build::build_source(project, &source, cx.assets)?;
    let name = args
        .name
        .unwrap_or_else(|| format!("{:?}", args.shape).to_lowercase());
    install(project, &doc, &name, source, args.node, material)
}

declare_op!(
    MeshPrimitive,
    "model.mesh.primitive",
    "Create a box, sphere, cylinder, cone, torus, plane or capsule mesh",
    PrimitiveArgs,
    mesh_primitive
);

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExtrudeArgs {
    /// Selector for the vector object to extrude, e.g. `logo:#mark`.
    pub path: String,
    /// Vector document the selector resolves in, when the selector does not name one
    /// itself. The model document being edited is chosen with the global `--doc`.
    #[serde(default)]
    pub source: Option<String>,
    /// Extrusion depth along Z. The solid is centred on the origin.
    pub depth: f32,
    /// Bevel radius. The solid grows to `depth + 2 * bevel` on Z and never in X/Y.
    #[serde(default)]
    pub bevel: Option<f32>,
    /// Rings across the bevel.
    #[serde(default = "three")]
    pub bevel_segments: u32,
    /// Which end caps to build.
    #[serde(default)]
    pub caps: Caps,
    /// Curve flattening tolerance in document units; smaller means more vertices.
    #[serde(default = "default_flatten")]
    pub flatten: f64,
    /// Name for the mesh and its node. Defaults to the source object's name.
    #[serde(default)]
    pub name: Option<String>,
    /// Also create a node that draws the mesh.
    #[serde(default = "yes")]
    pub node: bool,
    /// Selector for the material the new node uses.
    #[serde(default)]
    pub material: Option<String>,
}

fn extrude_from(
    project: &mut Project,
    args: ExtrudeArgs,
    cx: &mut OpCx,
    require_text: bool,
) -> Result<OpEffect> {
    let doc = target_model(project, cx)?;
    let from = resolve_path_ref(project, &args.path, args.source.as_deref(), require_text)?;
    let material = args
        .material
        .as_deref()
        .map(|s| one_of_type(project, s, &doc, "material").map(MaterialId::from))
        .transpose()?;
    let source = MeshSource::Extrude {
        from: from.clone(),
        depth: args.depth,
        bevel: bevel_of(args.bevel, args.bevel_segments)?,
        caps: args.caps,
        flatten: args.flatten,
    };
    // Prove the recipe builds before it lands in the document.
    crate::build::build_source(project, &source, cx.assets)?;
    let name = args.name.unwrap_or_else(|| {
        project
            .vector(&from.document)
            .ok()
            .and_then(|d| d.object(&from.object).map(|o| o.name.clone()))
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| from.object.to_string())
    });
    install(project, &doc, &name, source, args.node, material)
}

fn mesh_extrude(project: &mut Project, args: ExtrudeArgs, cx: &mut OpCx) -> Result<OpEffect> {
    extrude_from(project, args, cx, false)
}

declare_op!(
    MeshExtrude,
    "model.mesh.extrude",
    "Extrude a vector path into a solid, with optional bevel and end caps",
    ExtrudeArgs,
    mesh_extrude
);

fn mesh_from_text(project: &mut Project, args: ExtrudeArgs, cx: &mut OpCx) -> Result<OpEffect> {
    extrude_from(project, args, cx, true)
}

declare_op!(
    MeshFromText,
    "model.mesh.from-text",
    "Extrude a vector text object's glyph outlines into 3D lettering",
    ExtrudeArgs,
    mesh_from_text
);

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RevolveArgs {
    /// Selector for the profile to lathe. `x` is the radius, `-y` the height; run the
    /// profile bottom to top so the normals face outward.
    pub path: String,
    /// Vector document the selector resolves in, when the selector does not name one
    /// itself. The model document being edited is chosen with the global `--doc`.
    #[serde(default)]
    pub source: Option<String>,
    /// Sweep in degrees around the Y axis.
    #[serde(default = "full_turn")]
    pub angle: f32,
    /// Sectors around the sweep.
    #[serde(default = "default_segments")]
    pub segments: u32,
    /// Curve flattening tolerance in document units.
    #[serde(default = "default_flatten")]
    pub flatten: f64,
    /// Name for the mesh and its node.
    #[serde(default)]
    pub name: Option<String>,
    /// Also create a node that draws the mesh.
    #[serde(default = "yes")]
    pub node: bool,
    /// Selector for the material the new node uses.
    #[serde(default)]
    pub material: Option<String>,
}

fn full_turn() -> f32 {
    360.0
}

fn mesh_revolve(project: &mut Project, args: RevolveArgs, cx: &mut OpCx) -> Result<OpEffect> {
    let doc = target_model(project, cx)?;
    let from = resolve_path_ref(project, &args.path, args.source.as_deref(), false)?;
    let material = args
        .material
        .as_deref()
        .map(|s| one_of_type(project, s, &doc, "material").map(MaterialId::from))
        .transpose()?;
    let source = MeshSource::Revolve {
        from: from.clone(),
        angle: args.angle,
        segments: args.segments,
        flatten: args.flatten,
    };
    crate::build::build_source(project, &source, cx.assets)?;
    let name = args
        .name
        .unwrap_or_else(|| format!("{} revolve", from.object));
    install(project, &doc, &name, source, args.node, material)
}

declare_op!(
    MeshRevolve,
    "model.mesh.revolve",
    "Lathe a vector profile around the Y axis",
    RevolveArgs,
    mesh_revolve
);

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LoftArgs {
    /// Selectors for the ordered cross sections, placed one unit apart along Z.
    pub paths: Vec<String>,
    /// Vector document the selectors resolve in, when they do not name one themselves.
    #[serde(default)]
    pub source: Option<String>,
    /// Curve flattening tolerance in document units.
    #[serde(default = "default_flatten")]
    pub flatten: f64,
    /// Name for the mesh and its node.
    #[serde(default)]
    pub name: Option<String>,
    /// Also create a node that draws the mesh.
    #[serde(default = "yes")]
    pub node: bool,
    /// Selector for the material the new node uses.
    #[serde(default)]
    pub material: Option<String>,
}

fn mesh_loft(project: &mut Project, args: LoftArgs, cx: &mut OpCx) -> Result<OpEffect> {
    let doc = target_model(project, cx)?;
    if args.paths.len() < 2 {
        return Err(Error::Invalid(
            "model.mesh.loft needs at least two sections".into(),
        ));
    }
    let mut sections = Vec::with_capacity(args.paths.len());
    for p in &args.paths {
        sections.push(resolve_path_ref(project, p, args.source.as_deref(), false)?);
    }
    let material = args
        .material
        .as_deref()
        .map(|s| one_of_type(project, s, &doc, "material").map(MaterialId::from))
        .transpose()?;
    let source = MeshSource::Loft {
        sections,
        flatten: args.flatten,
    };
    crate::build::build_source(project, &source, cx.assets)?;
    let name = args.name.unwrap_or_else(|| "loft".to_string());
    install(project, &doc, &name, source, args.node, material)
}

declare_op!(
    MeshLoft,
    "model.mesh.loft",
    "Skin a surface through ordered vector cross sections",
    LoftArgs,
    mesh_loft
);

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MergeArgs {
    /// Selector for the nodes whose meshes are merged. Their world transforms are baked in.
    pub targets: String,
    /// Name for the merged mesh and its node.
    #[serde(default)]
    pub name: Option<String>,
}

fn mesh_merge(project: &mut Project, args: MergeArgs, cx: &mut OpCx) -> Result<OpEffect> {
    let doc = target_model(project, cx)?;
    let node_ids = all_of_type(project, &args.targets, &doc, "node")?;
    let worlds: std::collections::HashMap<NodeId, [[f32; 4]; 4]> =
        crate::build::world_transforms(project.model(&doc)?)
            .into_iter()
            .collect();
    let mut merged = MeshData::default();
    let mut sources: Vec<NodeId> = Vec::new();
    let mut material = None;
    for id in &node_ids {
        let node_id = NodeId::from(id.as_str());
        let model = project.model(&doc)?;
        let Some(node) = model.node(&node_id) else {
            continue;
        };
        let Some(mesh_id) = node.mesh.clone() else {
            continue;
        };
        if material.is_none() {
            material = node.material.clone();
        }
        let mut data = build_mesh(project, &doc, &mesh_id, cx.assets)?;
        if let Some(w) = worlds.get(&node_id) {
            data.transform(w);
        }
        merged.append(&data);
        sources.push(node_id);
    }
    if sources.len() < 2 {
        return Err(Error::Invalid(format!(
            "'{}' matched {} node(s) with a mesh; merging needs at least two",
            args.targets,
            sources.len()
        )));
    }
    merged.drop_degenerate(1e-14);
    let asset = bake(cx.assets, &merged, None)?;
    let name = args.name.unwrap_or_else(|| "merged".to_string());
    let effect = install(
        project,
        &doc,
        &name,
        MeshSource::Buffer { asset },
        true,
        material,
    )?;

    // Retire the sources and any mesh that nothing draws any more.
    let model = project.model_mut(&doc)?;
    model.nodes.retain(|n| !sources.contains(&n.id));
    for n in &mut model.nodes {
        n.children.retain(|c| !sources.contains(c));
    }
    let used: std::collections::HashSet<MeshId> =
        model.nodes.iter().filter_map(|n| n.mesh.clone()).collect();
    model.meshes.retain(|m| used.contains(&m.id));
    for anim in &mut model.animations {
        anim.channels.retain(|c| !sources.contains(&c.node));
    }
    let mut effect = effect;
    for s in sources {
        effect = effect.with_removed(s.to_string());
    }
    Ok(effect)
}

declare_op!(
    MeshMerge,
    "model.mesh.merge",
    "Merge several nodes' meshes into one baked mesh in world space",
    MergeArgs,
    mesh_merge
);

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WeldArgs {
    /// Selector for the mesh(es) to weld.
    pub target: String,
    /// Vertices closer than this collapse into one.
    #[serde(default = "default_weld_tol")]
    pub tolerance: f32,
    /// Crease angle in degrees used when normals are rebuilt after welding.
    #[serde(default = "default_angle")]
    pub angle: f32,
}

fn default_weld_tol() -> f32 {
    1e-4
}

fn default_angle() -> f32 {
    40.0
}

fn mesh_weld(project: &mut Project, args: WeldArgs, cx: &mut OpCx) -> Result<OpEffect> {
    let doc = target_model(project, cx)?;
    let ids = all_of_type(project, &args.target, &doc, "mesh")?;
    if !(args.tolerance.is_finite() && args.tolerance > 0.0) {
        return Err(Error::Invalid(format!(
            "weld tolerance must be positive, got {}",
            args.tolerance
        )));
    }
    let mut effect = OpEffect::changed(&doc);
    for id in ids {
        let mesh_id = MeshId::from(id.as_str());
        let mut removed = 0usize;
        bake_mesh(project, &doc, &mesh_id, cx, |m| {
            removed = m.weld(args.tolerance);
            m.recompute_normals(args.angle);
            Ok(None)
        })?;
        if removed == 0 {
            effect = effect.warn("no-op", id.clone(), "no vertices were close enough to weld");
        }
    }
    Ok(effect)
}

declare_op!(
    MeshWeld,
    "model.mesh.weld",
    "Merge coincident vertices and bake the result",
    WeldArgs,
    mesh_weld
);

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NormalsArgs {
    /// Selector for the mesh(es).
    pub target: String,
    /// Crease angle in degrees: faces meeting at a sharper angle keep separate normals.
    #[serde(default = "default_angle")]
    pub angle: f32,
    /// Force flat shading, as if the crease angle were zero.
    #[serde(default)]
    pub flat: bool,
}

fn mesh_recompute_normals(
    project: &mut Project,
    args: NormalsArgs,
    cx: &mut OpCx,
) -> Result<OpEffect> {
    let doc = target_model(project, cx)?;
    let ids = all_of_type(project, &args.target, &doc, "mesh")?;
    let angle = if args.flat { 0.0 } else { args.angle };
    for id in ids {
        bake_mesh(project, &doc, &MeshId::from(id.as_str()), cx, |m| {
            m.recompute_normals(angle);
            Ok(None)
        })?;
    }
    Ok(OpEffect::changed(&doc))
}

declare_op!(
    MeshRecomputeNormals,
    "model.mesh.recompute-normals",
    "Rebuild vertex normals with a crease angle, then bake",
    NormalsArgs,
    mesh_recompute_normals
);

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TargetArgs {
    /// Selector for the mesh(es).
    pub target: String,
}

fn mesh_generate_tangents(
    project: &mut Project,
    args: TargetArgs,
    cx: &mut OpCx,
) -> Result<OpEffect> {
    let doc = target_model(project, cx)?;
    let ids = all_of_type(project, &args.target, &doc, "mesh")?;
    let mut effect = OpEffect::changed(&doc);
    for id in ids {
        let mut warned = false;
        bake_mesh(project, &doc, &MeshId::from(id.as_str()), cx, |m| {
            if m.uvs.len() != m.positions.len() {
                warned = true;
            }
            if m.normals.len() != m.positions.len() {
                m.recompute_normals(40.0);
            }
            Ok(Some(crate::uv::tangents(m)))
        })?;
        if warned {
            effect = effect.warn(
                "missing-uv",
                id.clone(),
                "mesh has no UVs; tangents fall back to an arbitrary perpendicular basis. \
                 Run model.mesh.generate-uv first",
            );
        }
    }
    Ok(effect)
}

declare_op!(
    MeshGenerateTangents,
    "model.mesh.generate-tangents",
    "Compute mikktspace tangents and bake them alongside the geometry",
    TargetArgs,
    mesh_generate_tangents
);

#[derive(Debug, Clone, Copy, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum UvMode {
    /// Flat projection along one axis.
    Planar,
    /// Six-sided projection, each triangle taking the axis it faces.
    Box,
    /// Angle-based charts, packed into the unit square without overlap.
    Unwrap,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GenerateUvArgs {
    /// Selector for the mesh(es).
    pub target: String,
    /// Projection to use.
    pub mode: UvMode,
    /// Projection axis for `planar`.
    #[serde(default)]
    pub axis: crate::uv::Axis,
    /// Chart-splitting angle in degrees for `unwrap`.
    #[serde(default = "default_unwrap_angle")]
    pub angle: f32,
}

fn default_unwrap_angle() -> f32 {
    60.0
}

fn mesh_generate_uv(
    project: &mut Project,
    args: GenerateUvArgs,
    cx: &mut OpCx,
) -> Result<OpEffect> {
    let doc = target_model(project, cx)?;
    let ids = all_of_type(project, &args.target, &doc, "mesh")?;
    for id in ids {
        bake_mesh(project, &doc, &MeshId::from(id.as_str()), cx, |m| {
            match args.mode {
                UvMode::Planar => crate::uv::planar(m, args.axis),
                UvMode::Box => crate::uv::box_project(m),
                UvMode::Unwrap => crate::uv::unwrap(m, args.angle),
            }
            Ok(None)
        })?;
    }
    Ok(OpEffect::changed(&doc))
}

declare_op!(
    MeshGenerateUv,
    "model.mesh.generate-uv",
    "Project or unwrap texture coordinates, then bake",
    GenerateUvArgs,
    mesh_generate_uv
);

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TransformBakeArgs {
    /// Selector for the node(s) whose transform is baked into their geometry.
    pub target: String,
}

fn mesh_transform_bake(
    project: &mut Project,
    args: TransformBakeArgs,
    cx: &mut OpCx,
) -> Result<OpEffect> {
    let doc = target_model(project, cx)?;
    let node_ids = all_of_type(project, &args.target, &doc, "node")?;
    // Baking edits shared geometry, so refuse when another node draws the same mesh.
    let model = project.model(&doc)?;
    let mut plan: Vec<(NodeId, MeshId, [[f32; 4]; 4])> = Vec::new();
    for id in &node_ids {
        let node_id = NodeId::from(id.as_str());
        let Some(node) = model.node(&node_id) else {
            continue;
        };
        let Some(mesh_id) = node.mesh.clone() else {
            return Err(Error::Invalid(format!("node '{id}' draws no mesh")));
        };
        let users = model
            .nodes
            .iter()
            .filter(|n| n.mesh.as_ref() == Some(&mesh_id))
            .count();
        if users > 1 {
            return Err(Error::Invalid(format!(
                "mesh '{mesh_id}' is drawn by {users} nodes; baking '{id}' would move them all"
            )));
        }
        plan.push((
            node_id,
            mesh_id,
            trs_matrix(node.translation, node.rotation, node.scale),
        ));
    }
    for (node_id, mesh_id, m) in plan {
        bake_mesh(project, &doc, &mesh_id, cx, |data| {
            data.transform(&m);
            Ok(None)
        })?;
        let node = project
            .model_mut(&doc)?
            .node_mut(&node_id)
            .ok_or_else(|| Error::Invalid(format!("no node '{node_id}'")))?;
        node.translation = [0.0; 3];
        node.rotation = [0.0, 0.0, 0.0, 1.0];
        node.scale = [1.0; 3];
    }
    Ok(OpEffect::changed(&doc))
}

declare_op!(
    MeshTransformBake,
    "model.mesh.transform-bake",
    "Bake a node's transform into its geometry and reset the node to identity",
    TransformBakeArgs,
    mesh_transform_bake
);

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DecimateArgs {
    /// Selector for the mesh(es).
    pub target: String,
    /// Fraction of triangles to keep, 0..1.
    pub ratio: f32,
    /// Crease angle in degrees for the normals rebuilt afterwards.
    #[serde(default = "default_angle")]
    pub angle: f32,
}

fn mesh_decimate(project: &mut Project, args: DecimateArgs, cx: &mut OpCx) -> Result<OpEffect> {
    let doc = target_model(project, cx)?;
    let ids = all_of_type(project, &args.target, &doc, "mesh")?;
    if !(args.ratio.is_finite() && args.ratio > 0.0 && args.ratio <= 1.0) {
        return Err(Error::Invalid(format!(
            "decimate ratio must be in (0, 1], got {}",
            args.ratio
        )));
    }
    let mut effect = OpEffect::changed(&doc);
    for id in ids {
        let mut before = 0;
        let mut after = 0;
        bake_mesh(project, &doc, &MeshId::from(id.as_str()), cx, |m| {
            before = m.triangle_count();
            m.decimate(args.ratio, args.angle);
            after = m.triangle_count();
            Ok(None)
        })?;
        effect = effect.warn(
            "decimated",
            id.clone(),
            format!("{before} triangles -> {after}"),
        );
    }
    Ok(effect)
}

declare_op!(
    MeshDecimate,
    "model.mesh.decimate",
    "Reduce triangle count by vertex clustering, then bake",
    DecimateArgs,
    mesh_decimate
);

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ImportArgs {
    /// Path to a `.glb` or self-contained `.gltf` file on disk.
    #[serde(default)]
    pub file: Option<String>,
    /// An already-imported asset reference (`blake3:....glb`) to read instead.
    #[serde(default)]
    pub asset: Option<String>,
    /// Name for the mesh and its node.
    #[serde(default)]
    pub name: Option<String>,
    /// Also create a node that draws the mesh.
    #[serde(default = "yes")]
    pub node: bool,
}

fn mesh_import(project: &mut Project, args: ImportArgs, cx: &mut OpCx) -> Result<OpEffect> {
    let doc = target_model(project, cx)?;
    let bytes = match (&args.file, &args.asset) {
        (Some(f), None) => std::fs::read(f)?,
        (None, Some(a)) => cx.assets.get(&AssetRef(a.clone()))?,
        _ => {
            return Err(Error::Invalid(
                "model.mesh.import needs exactly one of `file` or `asset`".into(),
            ))
        }
    };
    let data = import_gltf(&bytes)?;
    let asset = bake(cx.assets, &data, None)?;
    let name = args.name.unwrap_or_else(|| {
        args.file
            .as_deref()
            .and_then(|f| {
                std::path::Path::new(f)
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .map(|s| s.to_string())
            })
            .unwrap_or_else(|| "imported".to_string())
    });
    install(
        project,
        &doc,
        &name,
        MeshSource::Buffer { asset },
        args.node,
        None,
    )
}

declare_op!(
    MeshImport,
    "model.mesh.import",
    "Import triangles from a .glb or embedded .gltf file into a baked mesh",
    ImportArgs,
    mesh_import
);
