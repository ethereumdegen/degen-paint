//! Raster documents: a GIMP-style non-destructive layer stack.
//!
//! Pixels never live in this JSON. A pixel layer references an immutable blake3 blob in the
//! asset store, so a filter writes a new blob and repoints the layer: undo is instant and
//! `project.json` stays small and diffable.

pub use super::common::BlendMode;
use super::common::*;
use crate::asset::AssetRef;
use crate::color::{Color, ColorSpace};
use crate::ids::{DocId, LayerId};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RasterDoc {
    pub id: DocId,
    pub name: String,
    pub size: [u32; 2],
    #[serde(default = "default_dpi")]
    pub dpi: f32,
    #[serde(default)]
    pub space: ColorSpace,
    #[serde(default = "default_depth")]
    pub depth: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background: Option<Color>,
    #[serde(default)]
    pub layers: Vec<Layer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection: Option<Selection>,
    #[serde(default)]
    pub guides: Guides,
}

fn default_dpi() -> f32 {
    72.0
}
fn default_depth() -> u8 {
    8
}

impl RasterDoc {
    pub fn new(id: DocId, name: impl Into<String>, w: u32, h: u32) -> Self {
        Self {
            id,
            name: name.into(),
            size: [w, h],
            dpi: 72.0,
            space: ColorSpace::Srgb,
            depth: 8,
            background: None,
            layers: Vec::new(),
            selection: None,
            guides: Guides::default(),
        }
    }

    pub fn width(&self) -> u32 {
        self.size[0]
    }
    pub fn height(&self) -> u32 {
        self.size[1]
    }

    pub fn bounds(&self) -> Rect {
        Rect::new(0.0, 0.0, self.size[0] as f64, self.size[1] as f64)
    }

    /// Depth-first walk in z-order (first element is bottom-most).
    pub fn walk(&self) -> Vec<&Layer> {
        fn rec<'a>(ls: &'a [Layer], out: &mut Vec<&'a Layer>) {
            for l in ls {
                out.push(l);
                if let LayerKind::Group { layers } = &l.kind {
                    rec(layers, out);
                }
            }
        }
        let mut out = Vec::new();
        rec(&self.layers, &mut out);
        out
    }

    pub fn layer(&self, id: &LayerId) -> Option<&Layer> {
        self.walk().into_iter().find(|l| &l.id == id)
    }

    pub fn layer_mut(&mut self, id: &LayerId) -> Option<&mut Layer> {
        fn rec<'a>(ls: &'a mut [Layer], id: &LayerId) -> Option<&'a mut Layer> {
            for l in ls.iter_mut() {
                if &l.id == id {
                    return Some(l);
                }
                if let LayerKind::Group { layers } = &mut l.kind {
                    if let Some(found) = rec(layers, id) {
                        return Some(found);
                    }
                }
            }
            None
        }
        rec(&mut self.layers, id)
    }

    /// Remove a layer anywhere in the tree, returning it.
    pub fn remove_layer(&mut self, id: &LayerId) -> Option<Layer> {
        fn rec(ls: &mut Vec<Layer>, id: &LayerId) -> Option<Layer> {
            if let Some(i) = ls.iter().position(|l| &l.id == id) {
                return Some(ls.remove(i));
            }
            for l in ls.iter_mut() {
                if let LayerKind::Group { layers } = &mut l.kind {
                    if let Some(found) = rec(layers, id) {
                        return Some(found);
                    }
                }
            }
            None
        }
        rec(&mut self.layers, id)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Layer {
    pub id: LayerId,
    #[serde(default)]
    pub name: String,
    #[serde(flatten)]
    pub kind: LayerKind,
    #[serde(default = "one", skip_serializing_if = "is_one")]
    pub opacity: f32,
    #[serde(default, skip_serializing_if = "BlendMode::is_normal")]
    pub blend: BlendMode,
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub visible: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub locked: bool,
    #[serde(default, skip_serializing_if = "Transform::is_identity")]
    pub transform: Transform,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mask: Option<Mask>,
    /// Clip to the layer beneath, like a clipping mask in Photoshop/GIMP.
    #[serde(default, skip_serializing_if = "is_false")]
    pub clip: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub effects: Vec<Effect>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<Provenance>,
}

fn one() -> f32 {
    1.0
}
fn yes() -> bool {
    true
}
fn is_one(v: &f32) -> bool {
    *v == 1.0
}
fn is_true(v: &bool) -> bool {
    *v
}
fn is_false(v: &bool) -> bool {
    !*v
}

impl Layer {
    pub fn new(id: LayerId, name: impl Into<String>, kind: LayerKind) -> Self {
        Self {
            id,
            name: name.into(),
            kind,
            opacity: 1.0,
            blend: BlendMode::Normal,
            visible: true,
            locked: false,
            transform: Transform::IDENTITY,
            mask: None,
            clip: false,
            effects: Vec::new(),
            provenance: None,
        }
    }

    pub fn type_name(&self) -> &'static str {
        match self.kind {
            LayerKind::Pixel { .. } => "pixel",
            LayerKind::Fill { .. } => "fill",
            LayerKind::Gradient { .. } => "gradient",
            LayerKind::Text { .. } => "text",
            LayerKind::Shape { .. } => "shape",
            LayerKind::Adjustment { .. } => "adjustment",
            LayerKind::Group { .. } => "group",
            LayerKind::Linked { .. } => "linked",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum LayerKind {
    Pixel {
        asset: AssetRef,
        #[serde(default)]
        offset: [i32; 2],
    },
    Fill {
        color: Color,
    },
    Gradient {
        paint: Paint,
    },
    Text {
        #[serde(flatten)]
        spec: TextSpec,
        #[serde(default = "black_paint")]
        fill: Paint,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stroke: Option<Stroke>,
    },
    Shape {
        /// SVG path data in document coordinates.
        d: String,
        #[serde(default = "black_paint")]
        fill: Paint,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stroke: Option<Stroke>,
        #[serde(default)]
        fill_rule: FillRule,
    },
    Adjustment {
        #[serde(flatten)]
        adjustment: Adjustment,
    },
    Group {
        #[serde(default)]
        layers: Vec<Layer>,
    },
    /// Another document in the project, rendered live into this one.
    Linked {
        document: DocId,
        #[serde(default)]
        fit: Fit,
        r#box: Rect,
    },
}

fn black_paint() -> Paint {
    Paint::solid(Color::BLACK)
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default,
)]
#[serde(rename_all = "kebab-case")]
pub enum Fit {
    #[default]
    Contain,
    Cover,
    Stretch,
    None,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Mask {
    pub asset: AssetRef,
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default)]
    pub inverted: bool,
    #[serde(default)]
    pub offset: [i32; 2],
}

/// Current selection. Selections scope every raster op and double as AI inpaint masks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Selection {
    /// Vector outline of the selection, when it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub d: Option<String>,
    /// Rasterized coverage mask, for wand and color-range selections.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mask: Option<AssetRef>,
    #[serde(default)]
    pub feather: f64,
    #[serde(default)]
    pub inverted: bool,
    /// Cached bounds, for digests and quick rejection.
    #[serde(default)]
    pub bounds: Rect,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct Guides {
    #[serde(default)]
    pub bleed: f64,
    #[serde(default)]
    pub safe: f64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub vertical: Vec<f64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub horizontal: Vec<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Adjustment {
    Curves {
        #[serde(default)]
        channel: Channel,
        /// Control points in 0..=1, ascending in x.
        points: Vec<[f64; 2]>,
    },
    Levels {
        #[serde(default)]
        channel: Channel,
        #[serde(default)]
        in_black: f64,
        #[serde(default = "one_f64")]
        in_white: f64,
        #[serde(default = "one_f64")]
        gamma: f64,
        #[serde(default)]
        out_black: f64,
        #[serde(default = "one_f64")]
        out_white: f64,
    },
    BrightnessContrast {
        #[serde(default)]
        brightness: f64,
        #[serde(default)]
        contrast: f64,
    },
    Hsl {
        #[serde(default)]
        hue: f64,
        #[serde(default)]
        saturation: f64,
        #[serde(default)]
        lightness: f64,
    },
    ColorBalance {
        #[serde(default)]
        shadows: [f64; 3],
        #[serde(default)]
        midtones: [f64; 3],
        #[serde(default)]
        highlights: [f64; 3],
    },
    Exposure {
        #[serde(default)]
        stops: f64,
        #[serde(default)]
        offset: f64,
    },
    ChannelMixer {
        #[serde(default = "identity_matrix")]
        matrix: [[f64; 3]; 3],
    },
    Threshold {
        #[serde(default = "half")]
        level: f64,
    },
    Posterize {
        #[serde(default = "four")]
        levels: u32,
    },
    Invert,
    Desaturate {
        #[serde(default)]
        mode: DesaturateMode,
    },
    Lut {
        asset: AssetRef,
        #[serde(default = "one_f64")]
        amount: f64,
    },
}

fn one_f64() -> f64 {
    1.0
}
fn half() -> f64 {
    0.5
}
fn four() -> u32 {
    4
}
fn identity_matrix() -> [[f64; 3]; 3] {
    [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default,
)]
#[serde(rename_all = "kebab-case")]
pub enum Channel {
    #[default]
    Rgb,
    Red,
    Green,
    Blue,
    Alpha,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default,
)]
#[serde(rename_all = "kebab-case")]
pub enum DesaturateMode {
    #[default]
    Luminosity,
    Average,
    Lightness,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum Effect {
    DropShadow {
        #[serde(default)]
        dx: f64,
        #[serde(default)]
        dy: f64,
        #[serde(default)]
        blur: f64,
        color: Color,
    },
    InnerShadow {
        #[serde(default)]
        dx: f64,
        #[serde(default)]
        dy: f64,
        #[serde(default)]
        blur: f64,
        color: Color,
    },
    Stroke {
        width: f64,
        color: Color,
        #[serde(default)]
        align: StrokeAlign,
    },
    OuterGlow {
        #[serde(default)]
        blur: f64,
        color: Color,
    },
    Blur {
        radius: f64,
    },
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default,
)]
#[serde(rename_all = "kebab-case")]
pub enum StrokeAlign {
    #[default]
    Outside,
    Center,
    Inside,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc() -> RasterDoc {
        let mut d = RasterDoc::new(DocId::from("doc_main"), "main", 100, 100);
        d.layers.push(Layer::new(
            LayerId::from("lyr_bg"),
            "bg",
            LayerKind::Fill {
                color: Color::WHITE,
            },
        ));
        d.layers.push(Layer::new(
            LayerId::from("grp_fg"),
            "fg",
            LayerKind::Group {
                layers: vec![Layer::new(
                    LayerId::from("lyr_title"),
                    "title",
                    LayerKind::Text {
                        spec: TextSpec::new("hello"),
                        fill: black_paint(),
                        stroke: None,
                    },
                )],
            },
        ));
        d
    }

    #[test]
    fn nested_layers_are_reachable_and_removable_by_id() {
        let mut d = doc();
        assert_eq!(d.walk().len(), 3);
        assert_eq!(d.layer(&LayerId::from("lyr_title")).unwrap().name, "title");

        d.layer_mut(&LayerId::from("lyr_title")).unwrap().opacity = 0.5;
        assert_eq!(d.layer(&LayerId::from("lyr_title")).unwrap().opacity, 0.5);

        let removed = d.remove_layer(&LayerId::from("lyr_title")).unwrap();
        assert_eq!(removed.id.as_str(), "lyr_title");
        assert_eq!(d.walk().len(), 2);
        assert!(d.layer(&LayerId::from("lyr_title")).is_none());
    }

    #[test]
    fn layer_kind_is_flattened_with_a_type_tag() {
        let d = doc();
        let j = serde_json::to_value(&d).unwrap();
        assert_eq!(j["layers"][0]["type"], "fill");
        assert_eq!(j["layers"][0]["color"], "#ffffff");
        // Defaults are omitted so project.json stays readable and diffs stay small.
        assert!(j["layers"][0].get("transform").is_none());
    }

    #[test]
    fn documents_round_trip_through_json_unchanged() {
        let d = doc();
        let s = serde_json::to_string(&d).unwrap();
        let back: RasterDoc = serde_json::from_str(&s).unwrap();
        assert_eq!(d, back);
    }

    #[test]
    fn every_blend_mode_has_a_distinct_kebab_name() {
        let names: std::collections::BTreeSet<String> = BlendMode::ALL
            .iter()
            .map(|b| {
                serde_json::to_value(b)
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect();
        assert_eq!(names.len(), BlendMode::ALL.len());
        assert!(names.contains("color-dodge"));
    }
}
