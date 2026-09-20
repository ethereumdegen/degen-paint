//! Vector documents: artboards of Bézier objects, Inkscape/Illustrator style.

use super::common::*;
use crate::asset::AssetRef;
use crate::color::Color;
use crate::ids::{ArtboardId, DocId, ObjectId};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct VectorDoc {
    pub id: DocId,
    pub name: String,
    #[serde(default)]
    pub units: Units,
    #[serde(default)]
    pub artboards: Vec<Artboard>,
    #[serde(default)]
    pub objects: Vec<VObject>,
}

impl VectorDoc {
    pub fn new(id: DocId, name: impl Into<String>, w: f64, h: f64) -> Self {
        Self {
            id,
            name: name.into(),
            units: Units::Px,
            artboards: vec![Artboard {
                id: ArtboardId::from("ab_1"),
                name: "artboard".into(),
                rect: Rect::new(0.0, 0.0, w, h),
                background: None,
            }],
            objects: Vec::new(),
        }
    }

    pub fn size(&self) -> (f64, f64) {
        let b = self
            .artboards
            .iter()
            .fold(Rect::default(), |acc, a| acc.union(a.rect));
        (b.w().max(1.0), b.h().max(1.0))
    }

    pub fn walk(&self) -> Vec<&VObject> {
        fn rec<'a>(os: &'a [VObject], out: &mut Vec<&'a VObject>) {
            for o in os {
                out.push(o);
                if let VKind::Group { objects } = &o.kind {
                    rec(objects, out);
                }
            }
        }
        let mut out = Vec::new();
        rec(&self.objects, &mut out);
        out
    }

    pub fn object(&self, id: &ObjectId) -> Option<&VObject> {
        self.walk().into_iter().find(|o| &o.id == id)
    }

    pub fn object_mut(&mut self, id: &ObjectId) -> Option<&mut VObject> {
        fn rec<'a>(os: &'a mut [VObject], id: &ObjectId) -> Option<&'a mut VObject> {
            for o in os.iter_mut() {
                if &o.id == id {
                    return Some(o);
                }
                if let VKind::Group { objects } = &mut o.kind {
                    if let Some(f) = rec(objects, id) {
                        return Some(f);
                    }
                }
            }
            None
        }
        rec(&mut self.objects, id)
    }

    pub fn remove_object(&mut self, id: &ObjectId) -> Option<VObject> {
        fn rec(os: &mut Vec<VObject>, id: &ObjectId) -> Option<VObject> {
            if let Some(i) = os.iter().position(|o| &o.id == id) {
                return Some(os.remove(i));
            }
            for o in os.iter_mut() {
                if let VKind::Group { objects } = &mut o.kind {
                    if let Some(f) = rec(objects, id) {
                        return Some(f);
                    }
                }
            }
            None
        }
        rec(&mut self.objects, id)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Artboard {
    pub id: ArtboardId,
    #[serde(default)]
    pub name: String,
    pub rect: Rect,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background: Option<Color>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
#[serde(rename_all = "kebab-case")]
pub enum Units {
    #[default]
    Px,
    Mm,
    In,
    Pt,
}

impl Units {
    /// Multiplier to CSS px at 96 dpi.
    pub fn to_px(self) -> f64 {
        match self {
            Units::Px => 1.0,
            Units::Mm => 96.0 / 25.4,
            Units::In => 96.0,
            Units::Pt => 96.0 / 72.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct VObject {
    pub id: ObjectId,
    #[serde(default)]
    pub name: String,
    #[serde(flatten)]
    pub kind: VKind,
    #[serde(default = "no_paint", skip_serializing_if = "Paint::is_none")]
    pub fill: Paint,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stroke: Option<Stroke>,
    #[serde(default)]
    pub fill_rule: FillRule,
    #[serde(default = "one", skip_serializing_if = "is_one")]
    pub opacity: f32,
    #[serde(default, skip_serializing_if = "BlendMode::is_normal")]
    pub blend: BlendMode,
    /// Another object used as a luminance mask.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mask: Option<ObjectId>,
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub visible: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub locked: bool,
    #[serde(default, skip_serializing_if = "Transform::is_identity")]
    pub transform: Transform,
    /// Another object used as a clip path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clip: Option<ObjectId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<Provenance>,
}

fn one() -> f32 { 1.0 }
fn yes() -> bool { true }
fn is_one(v: &f32) -> bool { *v == 1.0 }
fn is_true(v: &bool) -> bool { *v }
fn is_false(v: &bool) -> bool { !*v }
fn no_paint() -> Paint { Paint::None }

impl Paint {
    pub fn is_none(&self) -> bool {
        matches!(self, Paint::None)
    }
}

impl VObject {
    pub fn new(id: ObjectId, name: impl Into<String>, kind: VKind) -> Self {
        Self {
            id,
            name: name.into(),
            kind,
            fill: Paint::None,
            stroke: None,
            fill_rule: FillRule::Nonzero,
            opacity: 1.0,
            blend: BlendMode::Normal,
            mask: None,
            visible: true,
            locked: false,
            transform: Transform::IDENTITY,
            clip: None,
            provenance: None,
        }
    }

    pub fn with_fill(mut self, p: Paint) -> Self {
        self.fill = p;
        self
    }

    pub fn type_name(&self) -> &'static str {
        match self.kind {
            VKind::Path { .. } => "path",
            VKind::Rect { .. } => "rect",
            VKind::Ellipse { .. } => "ellipse",
            VKind::Polygon { .. } => "polygon",
            VKind::Star { .. } => "star",
            VKind::Line { .. } => "line",
            VKind::Text { .. } => "text",
            VKind::Image { .. } => "image",
            VKind::Group { .. } => "group",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum VKind {
    Path {
        /// SVG path data. Cubic Béziers; `kurbo` is the in-memory form.
        d: String,
    },
    Rect {
        rect: Rect,
        #[serde(default)]
        radius: f64,
    },
    Ellipse {
        center: [f64; 2],
        radius: [f64; 2],
    },
    Polygon {
        center: [f64; 2],
        radius: f64,
        sides: u32,
        #[serde(default)]
        rotation: f64,
    },
    Star {
        center: [f64; 2],
        outer: f64,
        inner: f64,
        points: u32,
        #[serde(default)]
        rotation: f64,
    },
    Line {
        from: [f64; 2],
        to: [f64; 2],
    },
    Text {
        #[serde(flatten)]
        spec: TextSpec,
        #[serde(default)]
        origin: [f64; 2],
        #[serde(default, skip_serializing_if = "Option::is_none")]
        on_path: Option<TextOnPath>,
    },
    Image {
        asset: AssetRef,
        rect: Rect,
    },
    Group {
        #[serde(default)]
        objects: Vec<VObject>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TextOnPath {
    pub target: ObjectId,
    #[serde(default)]
    pub offset: f64,
    #[serde(default)]
    pub side: PathSide,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
#[serde(rename_all = "kebab-case")]
pub enum PathSide {
    #[default]
    Left,
    Right,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn objects_nest_and_resolve_by_id() {
        let mut d = VectorDoc::new(DocId::from("doc_logo"), "logo", 512.0, 512.0);
        d.objects.push(VObject::new(
            ObjectId::from("grp_1"),
            "lockup",
            VKind::Group {
                objects: vec![VObject::new(
                    ObjectId::from("obj_mark"),
                    "mark",
                    VKind::Path { d: "M0 0 L10 0 L5 10 Z".into() },
                )
                .with_fill(Paint::solid(Color::parse("#fb8500").unwrap()))],
            },
        ));
        assert_eq!(d.walk().len(), 2);
        assert_eq!(d.object(&ObjectId::from("obj_mark")).unwrap().name, "mark");
        assert!(d.remove_object(&ObjectId::from("obj_mark")).is_some());
        assert_eq!(d.walk().len(), 1);
    }

    #[test]
    fn size_is_the_union_of_artboards() {
        let mut d = VectorDoc::new(DocId::from("doc_a"), "a", 100.0, 50.0);
        d.artboards.push(Artboard {
            id: ArtboardId::from("ab_2"),
            name: "b".into(),
            rect: Rect::new(120.0, 0.0, 80.0, 200.0),
            background: None,
        });
        assert_eq!(d.size(), (200.0, 200.0));
    }

    #[test]
    fn vector_documents_round_trip() {
        let mut d = VectorDoc::new(DocId::from("doc_logo"), "logo", 512.0, 512.0);
        d.objects.push(
            VObject::new(ObjectId::from("obj_1"), "mark", VKind::Path { d: "M0 0 H10".into() })
                .with_fill(Paint::solid(Color::BLACK)),
        );
        let s = serde_json::to_string(&d).unwrap();
        assert_eq!(serde_json::from_str::<VectorDoc>(&s).unwrap(), d);
    }
}
