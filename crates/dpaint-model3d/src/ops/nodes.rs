//! `model.node.*` — the scene graph.

use super::{all_of_type, declare_op, one_of_type, target_model, unique_id};
use crate::geom::{look_rotation, normalize_quat, quat_from_euler_deg, sub};
use dpaint_core::doc::model::Node;
use dpaint_core::{
    CameraId, Error, LightId, MaterialId, MeshId, NodeId, OpCx, OpEffect, Project, Result,
};
use serde::Deserialize;

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NodeAddArgs {
    /// Name of the new node; also seeds its id (`nd_<slug>`).
    pub name: String,
    /// Selector for the parent node. Omit for a root node.
    #[serde(default)]
    pub parent: Option<String>,
    /// Selector for the mesh this node draws.
    #[serde(default)]
    pub mesh: Option<String>,
    /// Selector for the material the mesh is drawn with.
    #[serde(default)]
    pub material: Option<String>,
    /// Selector for a light this node carries.
    #[serde(default)]
    pub light: Option<String>,
    /// Selector for a camera this node carries.
    #[serde(default)]
    pub camera: Option<String>,
    /// Local position.
    #[serde(default)]
    pub translation: Option<[f32; 3]>,
    /// Local rotation as XYZ Euler angles in degrees.
    #[serde(default)]
    pub rotation: Option<[f32; 3]>,
    /// Local scale.
    #[serde(default)]
    pub scale: Option<[f32; 3]>,
}

fn node_add(project: &mut Project, args: NodeAddArgs, cx: &mut OpCx) -> Result<OpEffect> {
    let doc = target_model(project, cx)?;
    let parent = args
        .parent
        .as_deref()
        .map(|s| one_of_type(project, s, &doc, "node"))
        .transpose()?;
    let mesh = args
        .mesh
        .as_deref()
        .map(|s| one_of_type(project, s, &doc, "mesh"))
        .transpose()?;
    let material = args
        .material
        .as_deref()
        .map(|s| one_of_type(project, s, &doc, "material"))
        .transpose()?;
    let light = args
        .light
        .as_deref()
        .map(|s| one_of_type(project, s, &doc, "light"))
        .transpose()?;
    let camera = args
        .camera
        .as_deref()
        .map(|s| one_of_type(project, s, &doc, "camera"))
        .transpose()?;

    let model = project.model_mut(&doc)?;
    let taken: Vec<String> = model.nodes.iter().map(|n| n.id.to_string()).collect();
    let id = unique_id(&args.name, |s| NodeId::from_name(s), &taken);
    let mut node = Node::new(id.clone(), args.name.clone());
    node.mesh = mesh.map(MeshId::from);
    node.material = material.map(MaterialId::from);
    node.light = light.map(LightId::from);
    node.camera = camera.map(CameraId::from);
    if let Some(t) = args.translation {
        node.translation = t;
    }
    if let Some(r) = args.rotation {
        node.rotation = quat_from_euler_deg(r);
    }
    if let Some(s) = args.scale {
        node.scale = s;
    }
    model.nodes.push(node);
    if let Some(p) = parent {
        let pid = NodeId::from(p);
        if let Some(pn) = model.node_mut(&pid) {
            pn.children.push(id.clone());
        }
    }
    Ok(OpEffect::changed(&doc).with_created(id.to_string()))
}

declare_op!(
    NodeAdd,
    "model.node.add",
    "Add a node to the scene graph, optionally carrying a mesh, light or camera",
    NodeAddArgs,
    node_add
);

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NodeRemoveArgs {
    /// Selector for the node(s) to remove. Descendants go with them.
    pub target: String,
}

fn node_remove(project: &mut Project, args: NodeRemoveArgs, cx: &mut OpCx) -> Result<OpEffect> {
    let doc = target_model(project, cx)?;
    let ids = all_of_type(project, &args.target, &doc, "node")?;
    let model = project.model_mut(&doc)?;
    let mut doomed: Vec<NodeId> = Vec::new();
    let mut stack: Vec<NodeId> = ids.iter().map(|i| NodeId::from(i.as_str())).collect();
    while let Some(id) = stack.pop() {
        if doomed.contains(&id) {
            continue;
        }
        if let Some(n) = model.node(&id) {
            stack.extend(n.children.iter().cloned());
        }
        doomed.push(id);
    }
    model.nodes.retain(|n| !doomed.contains(&n.id));
    for n in &mut model.nodes {
        n.children.retain(|c| !doomed.contains(c));
    }
    for anim in &mut model.animations {
        anim.channels.retain(|c| !doomed.contains(&c.node));
    }
    let mut effect = OpEffect::changed(&doc);
    for id in &doomed {
        effect = effect.with_removed(id.to_string());
    }
    Ok(effect)
}

declare_op!(
    NodeRemove,
    "model.node.remove",
    "Remove nodes and everything parented under them",
    NodeRemoveArgs,
    node_remove
);

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NodeReparentArgs {
    /// Selector for the node to move.
    pub target: String,
    /// Selector for the new parent. Omit to make the node a root.
    #[serde(default)]
    pub parent: Option<String>,
}

fn node_reparent(project: &mut Project, args: NodeReparentArgs, cx: &mut OpCx) -> Result<OpEffect> {
    let doc = target_model(project, cx)?;
    let target = NodeId::from(one_of_type(project, &args.target, &doc, "node")?);
    let parent = args
        .parent
        .as_deref()
        .map(|s| one_of_type(project, s, &doc, "node").map(NodeId::from))
        .transpose()?;
    if let Some(p) = &parent {
        if p == &target {
            return Err(Error::Invalid(format!("node '{target}' cannot parent itself")));
        }
        let model = project.model(&doc)?;
        let mut stack = vec![target.clone()];
        while let Some(cur) = stack.pop() {
            if &cur == p {
                return Err(Error::CyclicLink {
                    from: target.to_string(),
                    to: p.to_string(),
                });
            }
            if let Some(n) = model.node(&cur) {
                stack.extend(n.children.iter().cloned());
            }
        }
    }
    let model = project.model_mut(&doc)?;
    for n in &mut model.nodes {
        n.children.retain(|c| c != &target);
    }
    if let Some(p) = parent {
        model
            .node_mut(&p)
            .ok_or_else(|| Error::Invalid(format!("no node '{p}'")))?
            .children
            .push(target.clone());
    }
    Ok(OpEffect::changed(&doc))
}

declare_op!(
    NodeReparent,
    "model.node.reparent",
    "Move a node under a different parent, or to the scene root",
    NodeReparentArgs,
    node_reparent
);

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NodeRenameArgs {
    /// Selector for the node to rename.
    pub target: String,
    /// The new name. Ids never change, so selectors by id keep working.
    pub name: String,
}

fn node_rename(project: &mut Project, args: NodeRenameArgs, cx: &mut OpCx) -> Result<OpEffect> {
    let doc = target_model(project, cx)?;
    let id = NodeId::from(one_of_type(project, &args.target, &doc, "node")?);
    project
        .model_mut(&doc)?
        .node_mut(&id)
        .ok_or_else(|| Error::Invalid(format!("no node '{id}'")))?
        .name = args.name;
    Ok(OpEffect::changed(&doc))
}

declare_op!(
    NodeRename,
    "model.node.rename",
    "Rename a node",
    NodeRenameArgs,
    node_rename
);

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NodeSetTrsArgs {
    /// Selector for the node(s) to transform.
    pub target: String,
    /// Local position.
    #[serde(default)]
    pub translation: Option<[f32; 3]>,
    /// Local rotation as XYZ Euler angles in degrees.
    #[serde(default)]
    pub rotation: Option<[f32; 3]>,
    /// Local rotation as a raw quaternion `[x, y, z, w]`; wins over `rotation`.
    #[serde(default)]
    pub quaternion: Option<[f32; 4]>,
    /// Local scale.
    #[serde(default)]
    pub scale: Option<[f32; 3]>,
}

fn node_set_trs(project: &mut Project, args: NodeSetTrsArgs, cx: &mut OpCx) -> Result<OpEffect> {
    let doc = target_model(project, cx)?;
    let ids = all_of_type(project, &args.target, &doc, "node")?;
    if args.translation.is_none()
        && args.rotation.is_none()
        && args.quaternion.is_none()
        && args.scale.is_none()
    {
        return Err(Error::Invalid(
            "model.node.set-trs needs at least one of translation, rotation, quaternion, scale"
                .into(),
        ));
    }
    if let Some(s) = args.scale {
        if s.iter().any(|v| *v == 0.0 || !v.is_finite()) {
            return Err(Error::Invalid(format!(
                "scale must be non-zero and finite, got {s:?}"
            )));
        }
    }
    let model = project.model_mut(&doc)?;
    for id in &ids {
        let node = model
            .node_mut(&NodeId::from(id.as_str()))
            .ok_or_else(|| Error::Invalid(format!("no node '{id}'")))?;
        if let Some(t) = args.translation {
            node.translation = t;
        }
        if let Some(q) = args.quaternion {
            node.rotation = normalize_quat(q);
        } else if let Some(r) = args.rotation {
            node.rotation = quat_from_euler_deg(r);
        }
        if let Some(s) = args.scale {
            node.scale = s;
        }
    }
    Ok(OpEffect::changed(&doc))
}

declare_op!(
    NodeSetTrs,
    "model.node.set-trs",
    "Set a node's translation, rotation and scale",
    NodeSetTrsArgs,
    node_set_trs
);

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NodeLookAtArgs {
    /// Selector for the node to aim.
    pub target: String,
    /// Point to aim at, in the node's parent space.
    pub at: [f32; 3],
    /// Up hint used to resolve roll.
    #[serde(default = "default_up")]
    pub up: [f32; 3],
}

fn default_up() -> [f32; 3] {
    [0.0, 1.0, 0.0]
}

fn node_look_at(project: &mut Project, args: NodeLookAtArgs, cx: &mut OpCx) -> Result<OpEffect> {
    let doc = target_model(project, cx)?;
    let ids = all_of_type(project, &args.target, &doc, "node")?;
    let model = project.model_mut(&doc)?;
    let mut effect = OpEffect::changed(&doc);
    for id in &ids {
        let node = model
            .node_mut(&NodeId::from(id.as_str()))
            .ok_or_else(|| Error::Invalid(format!("no node '{id}'")))?;
        let dir = sub(args.at, node.translation);
        if crate::geom::length(dir) <= 1e-6 {
            effect = effect.warn(
                "degenerate-aim",
                id.clone(),
                "node already sits on the look-at point; rotation left unchanged",
            );
            continue;
        }
        // glTF cameras and lights look down -Z, and meshes follow the same convention here.
        node.rotation = look_rotation(dir, args.up);
    }
    Ok(effect)
}

declare_op!(
    NodeLookAt,
    "model.node.look-at",
    "Rotate a node so its -Z axis points at a world-space point",
    NodeLookAtArgs,
    node_look_at
);
