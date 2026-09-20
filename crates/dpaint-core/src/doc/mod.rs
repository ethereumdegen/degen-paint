//! The three document kinds and the enum that unifies them.

pub mod common;
pub mod model;
pub mod raster;
pub mod vector;

pub use common::*;
pub use model::ModelDoc;
pub use raster::RasterDoc;
pub use vector::VectorDoc;

use crate::ids::DocId;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Document {
    Raster(RasterDoc),
    Vector(VectorDoc),
    Model(ModelDoc),
}

impl Document {
    pub fn id(&self) -> &DocId {
        match self {
            Document::Raster(d) => &d.id,
            Document::Vector(d) => &d.id,
            Document::Model(d) => &d.id,
        }
    }

    pub fn name(&self) -> &str {
        match self {
            Document::Raster(d) => &d.name,
            Document::Vector(d) => &d.name,
            Document::Model(d) => &d.name,
        }
    }

    pub fn set_name(&mut self, name: String) {
        match self {
            Document::Raster(d) => d.name = name,
            Document::Vector(d) => d.name = name,
            Document::Model(d) => d.name = name,
        }
    }

    pub fn kind(&self) -> DocKind {
        match self {
            Document::Raster(_) => DocKind::Raster,
            Document::Vector(_) => DocKind::Vector,
            Document::Model(_) => DocKind::Model,
        }
    }

    /// Nominal size in px. Model documents have no 2D extent; callers choose a render size.
    pub fn size(&self) -> Option<(f64, f64)> {
        match self {
            Document::Raster(d) => Some((d.size[0] as f64, d.size[1] as f64)),
            Document::Vector(d) => Some(d.size()),
            Document::Model(_) => None,
        }
    }

    pub fn as_raster(&self) -> Option<&RasterDoc> {
        match self {
            Document::Raster(d) => Some(d),
            _ => None,
        }
    }

    pub fn as_raster_mut(&mut self) -> Option<&mut RasterDoc> {
        match self {
            Document::Raster(d) => Some(d),
            _ => None,
        }
    }

    pub fn as_vector(&self) -> Option<&VectorDoc> {
        match self {
            Document::Vector(d) => Some(d),
            _ => None,
        }
    }

    pub fn as_vector_mut(&mut self) -> Option<&mut VectorDoc> {
        match self {
            Document::Vector(d) => Some(d),
            _ => None,
        }
    }

    pub fn as_model(&self) -> Option<&ModelDoc> {
        match self {
            Document::Model(d) => Some(d),
            _ => None,
        }
    }

    pub fn as_model_mut(&mut self) -> Option<&mut ModelDoc> {
        match self {
            Document::Model(d) => Some(d),
            _ => None,
        }
    }

    /// Documents this one references, for cycle detection and render ordering.
    pub fn dependencies(&self) -> Vec<DocId> {
        let mut out = Vec::new();
        match self {
            Document::Raster(d) => {
                for l in d.walk() {
                    if let raster::LayerKind::Linked { document, .. } = &l.kind {
                        out.push(document.clone());
                    }
                }
            }
            Document::Vector(d) => {
                for o in d.walk() {
                    if let Paint::Document { document } = &o.fill {
                        out.push(document.clone());
                    }
                }
            }
            Document::Model(d) => {
                for m in &d.meshes {
                    match &m.source {
                        model::MeshSource::Extrude { from, .. }
                        | model::MeshSource::Revolve { from, .. } => out.push(from.document.clone()),
                        model::MeshSource::Loft { sections, .. } => {
                            out.extend(sections.iter().map(|s| s.document.clone()))
                        }
                        _ => {}
                    }
                }
                for mat in &d.materials {
                    for t in &mat.textures {
                        if let model::TextureSource::Document { document } = &t.source {
                            out.push(document.clone());
                        }
                    }
                }
            }
        }
        out.sort();
        out.dedup();
        out
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum DocKind {
    Raster,
    Vector,
    Model,
}

impl DocKind {
    pub const ALL: [DocKind; 3] = [DocKind::Raster, DocKind::Vector, DocKind::Model];

    pub fn as_str(self) -> &'static str {
        match self {
            DocKind::Raster => "raster",
            DocKind::Vector => "vector",
            DocKind::Model => "model",
        }
    }
}

impl std::str::FromStr for DocKind {
    type Err = crate::error::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "raster" => Ok(DocKind::Raster),
            "vector" => Ok(DocKind::Vector),
            "model" | "3d" | "gltf" => Ok(DocKind::Model),
            other => Err(crate::error::Error::Invalid(format!(
                "unknown document kind '{other}' (expected raster, vector or model)"
            ))),
        }
    }
}

impl std::fmt::Display for DocKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{LayerId, MeshId, ObjectId};

    #[test]
    fn dependencies_find_cross_mode_references() {
        let mut r = RasterDoc::new(DocId::from("doc_main"), "main", 10, 10);
        r.layers.push(raster::Layer::new(
            LayerId::from("lyr_badge"),
            "badge",
            raster::LayerKind::Linked {
                document: DocId::from("doc_logo"),
                fit: raster::Fit::Contain,
                r#box: Rect::new(0.0, 0.0, 10.0, 10.0),
            },
        ));
        assert_eq!(Document::Raster(r).dependencies(), vec![DocId::from("doc_logo")]);

        let mut m = ModelDoc::new(DocId::from("doc_badge"), "badge");
        m.meshes.push(model::Mesh {
            id: MeshId::from("msh_1"),
            name: "m".into(),
            source: model::MeshSource::Extrude {
                from: model::PathRef {
                    document: DocId::from("doc_logo"),
                    object: ObjectId::from("obj_mark"),
                },
                depth: 1.0,
                bevel: None,
                caps: model::Caps::Both,
                flatten: 0.25,
            },
        });
        assert_eq!(Document::Model(m).dependencies(), vec![DocId::from("doc_logo")]);
    }

    #[test]
    fn document_enum_tags_its_kind() {
        let d = Document::Vector(VectorDoc::new(DocId::from("doc_l"), "l", 10.0, 10.0));
        let j = serde_json::to_value(&d).unwrap();
        assert_eq!(j["kind"], "vector");
        assert_eq!(serde_json::from_value::<Document>(j).unwrap(), d);
    }
}
