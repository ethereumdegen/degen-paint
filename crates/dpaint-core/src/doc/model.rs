//! Model documents: a glTF-shaped scene graph.
//!
//! Meshes are **procedural by default** — the document stores the recipe ("extrude
//! doc_logo:#mark, depth 12, bevel 1.5"), not a vertex soup. Edit the source path and the
//! mesh regenerates. That is what keeps 3D diffable and agent-editable instead of opaque.

use crate::asset::AssetRef;
use crate::color::Color;
use crate::ids::{AnimId, CameraId, DocId, LightId, MaterialId, MeshId, NodeId, ObjectId};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ModelDoc {
    pub id: DocId,
    pub name: String,
    #[serde(default)]
    pub up_axis: UpAxis,
    #[serde(default)]
    pub nodes: Vec<Node>,
    #[serde(default)]
    pub meshes: Vec<Mesh>,
    #[serde(default)]
    pub materials: Vec<Material>,
    #[serde(default)]
    pub lights: Vec<Light>,
    #[serde(default)]
    pub cameras: Vec<Camera>,
    #[serde(default)]
    pub animations: Vec<Animation>,
}

impl ModelDoc {
    pub fn new(id: DocId, name: impl Into<String>) -> Self {
        Self {
            id,
            name: name.into(),
            up_axis: UpAxis::Y,
            nodes: Vec::new(),
            meshes: Vec::new(),
            materials: Vec::new(),
            lights: Vec::new(),
            cameras: Vec::new(),
            animations: Vec::new(),
        }
    }

    pub fn node(&self, id: &NodeId) -> Option<&Node> {
        self.nodes.iter().find(|n| &n.id == id)
    }

    pub fn node_mut(&mut self, id: &NodeId) -> Option<&mut Node> {
        self.nodes.iter_mut().find(|n| &n.id == id)
    }

    pub fn mesh(&self, id: &MeshId) -> Option<&Mesh> {
        self.meshes.iter().find(|m| &m.id == id)
    }

    pub fn material(&self, id: &MaterialId) -> Option<&Material> {
        self.materials.iter().find(|m| &m.id == id)
    }

    /// Nodes with no parent.
    pub fn roots(&self) -> Vec<&Node> {
        let children: std::collections::BTreeSet<&NodeId> =
            self.nodes.iter().flat_map(|n| n.children.iter()).collect();
        self.nodes
            .iter()
            .filter(|n| !children.contains(&n.id))
            .collect()
    }
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default,
)]
#[serde(rename_all = "lowercase")]
pub enum UpAxis {
    #[default]
    Y,
    Z,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Node {
    pub id: NodeId,
    #[serde(default)]
    pub name: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<NodeId>,
    #[serde(default)]
    pub translation: [f32; 3],
    #[serde(default = "unit_quat")]
    pub rotation: [f32; 4],
    #[serde(default = "unit_scale")]
    pub scale: [f32; 3],
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mesh: Option<MeshId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub material: Option<MaterialId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub light: Option<LightId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub camera: Option<CameraId>,
}

fn unit_quat() -> [f32; 4] {
    [0.0, 0.0, 0.0, 1.0]
}
fn unit_scale() -> [f32; 3] {
    [1.0, 1.0, 1.0]
}

impl Node {
    pub fn new(id: NodeId, name: impl Into<String>) -> Self {
        Self {
            id,
            name: name.into(),
            children: Vec::new(),
            translation: [0.0; 3],
            rotation: unit_quat(),
            scale: unit_scale(),
            mesh: None,
            material: None,
            light: None,
            camera: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Mesh {
    pub id: MeshId,
    #[serde(default)]
    pub name: String,
    #[serde(flatten)]
    pub source: MeshSource,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "op", rename_all = "kebab-case")]
pub enum MeshSource {
    Primitive {
        shape: Primitive,
        #[serde(default = "unit_size")]
        size: [f32; 3],
        #[serde(default = "default_segments")]
        segments: u32,
    },
    /// Extrude a vector path into a solid. The cross-mode bridge.
    Extrude {
        from: PathRef,
        depth: f32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bevel: Option<Bevel>,
        #[serde(default)]
        caps: Caps,
        #[serde(default = "default_flatten")]
        flatten: f64,
    },
    Revolve {
        from: PathRef,
        #[serde(default = "full_turn")]
        angle: f32,
        #[serde(default = "default_segments")]
        segments: u32,
        #[serde(default = "default_flatten")]
        flatten: f64,
    },
    Loft {
        sections: Vec<PathRef>,
        #[serde(default = "default_flatten")]
        flatten: f64,
    },
    /// Baked or imported geometry stored as a binary blob.
    Buffer { asset: AssetRef },
}

fn unit_size() -> [f32; 3] {
    [1.0, 1.0, 1.0]
}
fn default_segments() -> u32 {
    32
}
fn default_flatten() -> f64 {
    0.25
}
fn full_turn() -> f32 {
    360.0
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Primitive {
    Box,
    Sphere,
    Cylinder,
    Cone,
    Torus,
    Plane,
    Capsule,
}

/// Reference to a path living in a vector document: `doc_logo` / `obj_mark`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PathRef {
    pub document: DocId,
    pub object: ObjectId,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Bevel {
    pub size: f32,
    #[serde(default = "three")]
    pub segments: u32,
}

fn three() -> u32 {
    3
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default,
)]
#[serde(rename_all = "kebab-case")]
pub enum Caps {
    #[default]
    Both,
    Front,
    Back,
    None,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Material {
    pub id: MaterialId,
    #[serde(default)]
    pub name: String,
    #[serde(default = "white")]
    pub base_color: Color,
    #[serde(default)]
    pub metallic: f32,
    #[serde(default = "half_f32")]
    pub roughness: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub emissive: Option<Color>,
    #[serde(default)]
    pub emissive_strength: f32,
    #[serde(default)]
    pub double_sided: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alpha_mode: Option<AlphaMode>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub textures: Vec<TextureBinding>,
}

fn white() -> Color {
    Color::WHITE
}
fn half_f32() -> f32 {
    0.5
}

impl Material {
    pub fn new(id: MaterialId, name: impl Into<String>) -> Self {
        Self {
            id,
            name: name.into(),
            base_color: Color::WHITE,
            metallic: 0.0,
            roughness: 0.5,
            emissive: None,
            emissive_strength: 0.0,
            double_sided: false,
            alpha_mode: None,
            textures: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "UPPERCASE")]
pub enum AlphaMode {
    Opaque,
    Mask,
    Blend,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TextureBinding {
    pub slot: TextureSlot,
    #[serde(flatten)]
    pub source: TextureSource,
    #[serde(default = "one_f32")]
    pub scale: f32,
    #[serde(default)]
    pub uv_set: u32,
}

fn one_f32() -> f32 {
    1.0
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum TextureSlot {
    BaseColor,
    Normal,
    MetallicRoughness,
    Occlusion,
    Emissive,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum TextureSource {
    /// A raster document in this project, rendered on demand.
    Document {
        document: DocId,
    },
    Asset {
        asset: AssetRef,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Light {
    pub id: LightId,
    #[serde(default)]
    pub name: String,
    pub kind: LightKind,
    #[serde(default = "white")]
    pub color: Color,
    #[serde(default = "one_f32")]
    pub intensity: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range: Option<f32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum LightKind {
    Directional,
    Point,
    Spot,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Camera {
    pub id: CameraId,
    #[serde(default)]
    pub name: String,
    #[serde(default = "default_yfov")]
    pub yfov: f32,
    #[serde(default = "default_znear")]
    pub znear: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zfar: Option<f32>,
}

fn default_yfov() -> f32 {
    0.6
}
fn default_znear() -> f32 {
    0.01
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Animation {
    pub id: AnimId,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub channels: Vec<AnimChannel>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AnimChannel {
    pub node: NodeId,
    pub path: AnimPath,
    #[serde(default)]
    pub interpolation: Interpolation,
    pub keys: Vec<AnimKey>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum AnimPath {
    Translation,
    Rotation,
    Scale,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default,
)]
#[serde(rename_all = "UPPERCASE")]
pub enum Interpolation {
    #[default]
    Linear,
    Step,
    CubicSpline,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AnimKey {
    pub t: f32,
    pub v: Vec<f32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roots_exclude_every_referenced_child() {
        let mut d = ModelDoc::new(DocId::from("doc_badge"), "badge");
        let mut root = Node::new(NodeId::from("nd_root"), "root");
        root.children.push(NodeId::from("nd_body"));
        d.nodes.push(root);
        d.nodes.push(Node::new(NodeId::from("nd_body"), "body"));
        let roots = d.roots();
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0].id.as_str(), "nd_root");
    }

    #[test]
    fn extrude_recipe_is_stored_not_baked_geometry() {
        let m = Mesh {
            id: MeshId::from("msh_body"),
            name: "body".into(),
            source: MeshSource::Extrude {
                from: PathRef {
                    document: DocId::from("doc_logo"),
                    object: ObjectId::from("obj_mark"),
                },
                depth: 12.0,
                bevel: Some(Bevel {
                    size: 1.5,
                    segments: 3,
                }),
                caps: Caps::Both,
                flatten: 0.05,
            },
        };
        let j = serde_json::to_value(&m).unwrap();
        assert_eq!(j["op"], "extrude");
        assert_eq!(j["from"]["object"], "obj_mark");
        assert_eq!(serde_json::from_value::<Mesh>(j).unwrap(), m);
    }

    #[test]
    fn model_documents_round_trip() {
        let mut d = ModelDoc::new(DocId::from("doc_badge"), "badge");
        d.materials
            .push(Material::new(MaterialId::from("mat_gold"), "gold"));
        d.nodes.push(Node::new(NodeId::from("nd_root"), "root"));
        let s = serde_json::to_string(&d).unwrap();
        assert_eq!(serde_json::from_str::<ModelDoc>(&s).unwrap(), d);
    }
}
