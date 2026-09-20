//! Types shared by all three document kinds: paint, strokes, gradients, text, transforms.

use crate::color::Color;
use crate::ids::DocId;
use serde::{Deserialize, Serialize};

/// Affine transform, serialized as SVG's `[a b c d e f]`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(transparent)]
pub struct Transform(pub [f64; 6]);

impl Default for Transform {
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl Transform {
    pub const IDENTITY: Transform = Transform([1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);

    pub fn translate(x: f64, y: f64) -> Self {
        Transform([1.0, 0.0, 0.0, 1.0, x, y])
    }

    pub fn scale(sx: f64, sy: f64) -> Self {
        Transform([sx, 0.0, 0.0, sy, 0.0, 0.0])
    }

    pub fn rotate_deg(deg: f64) -> Self {
        let (s, c) = deg.to_radians().sin_cos();
        Transform([c, s, -s, c, 0.0, 0.0])
    }

    pub fn is_identity(&self) -> bool {
        self.0 == Self::IDENTITY.0
    }

    pub fn to_kurbo(self) -> kurbo::Affine {
        kurbo::Affine::new(self.0)
    }

    pub fn from_kurbo(a: kurbo::Affine) -> Self {
        Transform(a.as_coeffs())
    }

    pub fn then(self, other: Transform) -> Transform {
        Transform::from_kurbo(other.to_kurbo() * self.to_kurbo())
    }
}

/// Axis-aligned rectangle `[x, y, w, h]`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, schemars::JsonSchema, Default)]
#[serde(transparent)]
pub struct Rect(pub [f64; 4]);

impl Rect {
    pub fn new(x: f64, y: f64, w: f64, h: f64) -> Self {
        Rect([x, y, w, h])
    }
    pub fn x(&self) -> f64 {
        self.0[0]
    }
    pub fn y(&self) -> f64 {
        self.0[1]
    }
    pub fn w(&self) -> f64 {
        self.0[2]
    }
    pub fn h(&self) -> f64 {
        self.0[3]
    }
    pub fn right(&self) -> f64 {
        self.0[0] + self.0[2]
    }
    pub fn bottom(&self) -> f64 {
        self.0[1] + self.0[3]
    }
    pub fn is_empty(&self) -> bool {
        self.w() <= 0.0 || self.h() <= 0.0
    }

    pub fn union(self, other: Rect) -> Rect {
        if self.is_empty() {
            return other;
        }
        if other.is_empty() {
            return self;
        }
        let (x0, y0) = (self.x().min(other.x()), self.y().min(other.y()));
        let (x1, y1) = (
            self.right().max(other.right()),
            self.bottom().max(other.bottom()),
        );
        Rect([x0, y0, x1 - x0, y1 - y0])
    }

    pub fn intersects(self, other: Rect) -> bool {
        self.x() < other.right()
            && other.x() < self.right()
            && self.y() < other.bottom()
            && other.y() < self.bottom()
    }

    pub fn contains_rect(self, inner: Rect) -> bool {
        inner.x() >= self.x()
            && inner.y() >= self.y()
            && inner.right() <= self.right()
            && inner.bottom() <= self.bottom()
    }

    pub fn to_kurbo(self) -> kurbo::Rect {
        kurbo::Rect::new(self.x(), self.y(), self.right(), self.bottom())
    }

    pub fn from_kurbo(r: kurbo::Rect) -> Self {
        Rect([r.x0, r.y0, r.width(), r.height()])
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(remote = "Self", tag = "type", rename_all = "kebab-case")]
pub enum Paint {
    Solid {
        color: Color,
    },
    Linear {
        stops: Vec<GradientStop>,
        from: [f64; 2],
        to: [f64; 2],
    },
    Radial {
        stops: Vec<GradientStop>,
        center: [f64; 2],
        radius: f64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        focal: Option<[f64; 2]>,
    },
    /// A rendered document used as a fill — the raster→vector→3D bridge in fill form.
    Document {
        document: DocId,
    },
    None,
}

impl Paint {
    pub fn solid(c: Color) -> Self {
        Paint::Solid { color: c }
    }

    /// Representative color for digests, lint contrast checks, and thumbnails.
    pub fn average_color(&self) -> Option<Color> {
        match self {
            Paint::Solid { color } => Some(*color),
            Paint::Linear { stops, .. } | Paint::Radial { stops, .. } => {
                if stops.is_empty() {
                    return None;
                }
                let n = stops.len() as f32;
                let acc = stops.iter().fold([0.0f32; 4], |mut a, s| {
                    let c = s.color;
                    a[0] += c.r;
                    a[1] += c.g;
                    a[2] += c.b;
                    a[3] += c.a;
                    a
                });
                Some(Color::rgba(acc[0] / n, acc[1] / n, acc[2] / n, acc[3] / n))
            }
            _ => None,
        }
    }
}

// `remote = "Self"` turns the derives into associated functions; these two impls put the
// derived behavior back on the trait, leaving room for the shorthand in Deserialize.
impl Serialize for Paint {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        Paint::serialize(self, s)
    }
}

/// Accept `"#fb8500"` and `"none"` as well as the full tagged form. Every surface — CLI
/// flags, MCP arguments, hand-written JSON — gets the shorthand for free.
impl<'de> Deserialize<'de> for Paint {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = serde_json::Value::deserialize(d)?;
        if let Some(s) = v.as_str() {
            return Paint::from_shorthand(s).map_err(serde::de::Error::custom);
        }
        Paint::deserialize(v).map_err(serde::de::Error::custom)
    }
}

impl Paint {
    /// `none` or a hex color. Palette lookups happen in the ops, which know the project.
    pub fn from_shorthand(s: &str) -> std::result::Result<Self, String> {
        let t = s.trim();
        if t.eq_ignore_ascii_case("none") || t.is_empty() {
            return Ok(Paint::None);
        }
        Color::parse(t)
            .map(Paint::solid)
            .ok_or_else(|| format!("'{t}' is not a hex color or 'none'"))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct GradientStop {
    pub offset: f64,
    pub color: Color,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(remote = "Self")]
pub struct Stroke {
    pub paint: Paint,
    pub width: f64,
    #[serde(default)]
    pub cap: LineCap,
    #[serde(default)]
    pub join: LineJoin,
    #[serde(default = "default_miter")]
    pub miter: f64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dash: Vec<f64>,
    #[serde(default)]
    pub dash_offset: f64,
}

fn default_miter() -> f64 {
    4.0
}

impl Serialize for Stroke {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        Stroke::serialize(self, s)
    }
}

/// `"#1d3557"` means a 1-unit solid stroke; `"2 #1d3557"` sets the width too.
impl<'de> Deserialize<'de> for Stroke {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = serde_json::Value::deserialize(d)?;
        if let Some(s) = v.as_str() {
            return Stroke::from_shorthand(s).map_err(serde::de::Error::custom);
        }
        Stroke::deserialize(v).map_err(serde::de::Error::custom)
    }
}

impl Stroke {
    pub fn from_shorthand(s: &str) -> std::result::Result<Self, String> {
        let t = s.trim();
        let (width, paint) = match t.split_once(char::is_whitespace) {
            Some((w, rest)) if w.parse::<f64>().is_ok() => {
                (w.parse::<f64>().expect("checked"), rest.trim())
            }
            _ => (1.0, t),
        };
        let color = Color::parse(paint).ok_or_else(|| format!("'{paint}' is not a hex color"))?;
        Ok(Stroke::solid(color, width))
    }
}

impl Stroke {
    pub fn solid(color: Color, width: f64) -> Self {
        Self {
            paint: Paint::solid(color),
            width,
            cap: LineCap::default(),
            join: LineJoin::default(),
            miter: 4.0,
            dash: Vec::new(),
            dash_offset: 0.0,
        }
    }
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default,
)]
#[serde(rename_all = "kebab-case")]
pub enum LineCap {
    #[default]
    Butt,
    Round,
    Square,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default,
)]
#[serde(rename_all = "kebab-case")]
pub enum LineJoin {
    #[default]
    Miter,
    Round,
    Bevel,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default,
)]
#[serde(rename_all = "kebab-case")]
pub enum FillRule {
    #[default]
    Nonzero,
    Evenodd,
}

/// Text specification shared by raster text layers and vector text objects, so a
/// string typed in one mode shapes identically in the other and in the 3D extruder.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TextSpec {
    pub text: String,
    #[serde(default = "default_family")]
    pub family: String,
    #[serde(default = "default_weight")]
    pub weight: u16,
    #[serde(default)]
    pub italic: bool,
    #[serde(default = "default_size")]
    pub size: f64,
    #[serde(default)]
    pub align: TextAlign,
    #[serde(default = "default_leading")]
    pub leading: f64,
    #[serde(default)]
    pub tracking: f64,
    /// Layout box; when absent the text is laid out unbounded from its origin.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r#box: Option<Rect>,
}

fn default_family() -> String {
    "sans-serif".into()
}
fn default_weight() -> u16 {
    400
}
fn default_size() -> f64 {
    16.0
}
fn default_leading() -> f64 {
    1.2
}

impl TextSpec {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            family: default_family(),
            weight: default_weight(),
            italic: false,
            size: default_size(),
            align: TextAlign::default(),
            leading: default_leading(),
            tracking: 0.0,
            r#box: None,
        }
    }
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default,
)]
#[serde(rename_all = "kebab-case")]
pub enum TextAlign {
    #[default]
    Left,
    Center,
    Right,
    Justify,
}

/// Separable and non-separable blend modes, per the PDF/CSS compositing spec.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default,
)]
#[serde(rename_all = "kebab-case")]
pub enum BlendMode {
    #[default]
    Normal,
    Multiply,
    Screen,
    Overlay,
    Darken,
    Lighten,
    ColorDodge,
    ColorBurn,
    HardLight,
    SoftLight,
    Difference,
    Exclusion,
    Hue,
    Saturation,
    Color,
    Luminosity,
    // Extras beyond the CSS set, familiar from GIMP/Photoshop.
    LinearBurn,
    LinearDodge,
    VividLight,
    LinearLight,
    PinLight,
    HardMix,
    Subtract,
    Divide,
    DarkerColor,
    LighterColor,
    Dissolve,
}

impl BlendMode {
    pub fn is_normal(&self) -> bool {
        matches!(self, BlendMode::Normal)
    }

    pub const ALL: [BlendMode; 27] = [
        BlendMode::Normal,
        BlendMode::Multiply,
        BlendMode::Screen,
        BlendMode::Overlay,
        BlendMode::Darken,
        BlendMode::Lighten,
        BlendMode::ColorDodge,
        BlendMode::ColorBurn,
        BlendMode::HardLight,
        BlendMode::SoftLight,
        BlendMode::Difference,
        BlendMode::Exclusion,
        BlendMode::Hue,
        BlendMode::Saturation,
        BlendMode::Color,
        BlendMode::Luminosity,
        BlendMode::LinearBurn,
        BlendMode::LinearDodge,
        BlendMode::VividLight,
        BlendMode::LinearLight,
        BlendMode::PinLight,
        BlendMode::HardMix,
        BlendMode::Subtract,
        BlendMode::Divide,
        BlendMode::DarkerColor,
        BlendMode::LighterColor,
        BlendMode::Dissolve,
    ];

    pub fn is_separable(self) -> bool {
        !matches!(
            self,
            BlendMode::Hue
                | BlendMode::Saturation
                | BlendMode::Color
                | BlendMode::Luminosity
                | BlendMode::DarkerColor
                | BlendMode::LighterColor
                | BlendMode::Dissolve
        )
    }
}

/// Where a generated object came from. Recorded so AI output is auditable,
/// reproducible, and separable from hand-authored work.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Provenance {
    pub provider: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    pub at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transform_composition_matches_kurbo_order() {
        let t = Transform::translate(10.0, 0.0).then(Transform::scale(2.0, 2.0));
        let p = t.to_kurbo() * kurbo::Point::new(0.0, 0.0);
        assert_eq!(
            (p.x, p.y),
            (20.0, 0.0),
            "translate then scale must scale the translation"
        );
    }

    #[test]
    fn rect_union_ignores_empty_operands() {
        let a = Rect::new(0.0, 0.0, 10.0, 10.0);
        assert_eq!(a.union(Rect::default()), a);
        assert_eq!(
            a.union(Rect::new(10.0, 10.0, 5.0, 5.0)),
            Rect::new(0.0, 0.0, 15.0, 15.0)
        );
    }

    #[test]
    fn paint_accepts_a_hex_string_or_the_full_tagged_form() {
        let solid: Paint = serde_json::from_value(serde_json::json!("#fb8500")).unwrap();
        assert_eq!(solid, Paint::solid(Color::parse("#fb8500").unwrap()));
        assert_eq!(
            serde_json::from_value::<Paint>(serde_json::json!("none")).unwrap(),
            Paint::None
        );

        let tagged: Paint =
            serde_json::from_value(serde_json::json!({ "type": "solid", "color": "#112233" }))
                .unwrap();
        assert_eq!(tagged, Paint::solid(Color::parse("#112233").unwrap()));

        assert!(serde_json::from_value::<Paint>(serde_json::json!("chartreuse")).is_err());
    }

    #[test]
    fn stroke_accepts_a_shorthand_string_with_an_optional_width() {
        let s: Stroke = serde_json::from_value(serde_json::json!("#1d3557")).unwrap();
        assert_eq!(s.width, 1.0);
        let s: Stroke = serde_json::from_value(serde_json::json!("3 #1d3557")).unwrap();
        assert_eq!(s.width, 3.0);
        assert_eq!(s.paint, Paint::solid(Color::parse("#1d3557").unwrap()));

        let full: Stroke =
            serde_json::from_value(serde_json::json!({ "paint": "#000000", "width": 8 })).unwrap();
        assert_eq!(full.width, 8.0);
    }

    #[test]
    fn paint_serializes_with_a_type_tag() {
        let j = serde_json::to_value(Paint::solid(Color::parse("#fb8500").unwrap())).unwrap();
        assert_eq!(j["type"], "solid");
        assert_eq!(j["color"], "#fb8500");
    }
}
