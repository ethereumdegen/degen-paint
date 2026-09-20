//! `vector.style.*` — paint, stroke, gradients, dashes, opacity, blend, fill rule,
//! markers and style copying.

use super::{check_unlocked, doc_of, fresh_id, index_in_owner, many, one, owner_list, parse_color};
use crate::geom;
use crate::vop;
use dpaint_core::doc::common::{
    BlendMode, FillRule, GradientStop, LineCap, LineJoin, Paint, Stroke,
};
use dpaint_core::doc::vector::{VKind, VObject};
use dpaint_core::error::{Error, Result};
use dpaint_core::ids::ObjectId;
use dpaint_core::kurbo::{Affine, BezPath, Point, Shape, Vec2};
use dpaint_core::project::Project;
use dpaint_core::{Op, OpCx, OpEffect};
use serde::Deserialize;

pub fn ops() -> Vec<Box<dyn Op>> {
    vec![
        Box::new(Fill),
        Box::new(StrokeOp),
        Box::new(Gradient),
        Box::new(Dash),
        Box::new(Opacity),
        Box::new(Blend),
        Box::new(FillRuleOp),
        Box::new(Marker),
        Box::new(Copy),
    ]
}

// -------------------------------------------------------------------------------- fill

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct FillArgs {
    /// Selector of the objects to paint.
    pub target: String,
    /// `#rrggbb[aa]`, a palette name, or `none`.
    pub color: String,
}

vop!(Fill, FillArgs, "vector.style.fill", "Set a solid fill colour, or clear the fill with 'none'");

impl Fill {
    fn run(project: &mut Project, a: FillArgs, cx: &mut OpCx) -> Result<OpEffect> {
        let doc = doc_of(project, cx)?;
        let ids = many(project, &a.target, &doc)?;
        check_unlocked(project.vector(&doc)?, &ids)?;
        let paint = parse_color(project, &a.color)?
            .map(Paint::solid)
            .unwrap_or(Paint::None);
        let v = project.vector_mut(&doc)?;
        for id in &ids {
            if let Some(o) = v.object_mut(id) {
                o.fill = paint.clone();
            }
        }
        Ok(OpEffect::changed(&doc))
    }
}

// ------------------------------------------------------------------------------ stroke

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct StrokeArgs {
    /// Selector of the objects to stroke.
    pub target: String,
    /// `#rrggbb[aa]`, a palette name, or `none` to remove the stroke.
    #[serde(default)]
    pub color: Option<String>,
    /// Stroke width in document units.
    #[serde(default)]
    pub width: Option<f64>,
    /// butt | round | square
    #[serde(default)]
    pub cap: Option<LineCap>,
    /// miter | round | bevel
    #[serde(default)]
    pub join: Option<LineJoin>,
    /// Miter limit, 1 or more.
    #[serde(default)]
    pub miter: Option<f64>,
}

vop!(StrokeOp, StrokeArgs, "vector.style.stroke", "Set stroke colour, width, caps, joins and miter limit");

impl StrokeOp {
    fn run(project: &mut Project, a: StrokeArgs, cx: &mut OpCx) -> Result<OpEffect> {
        let doc = doc_of(project, cx)?;
        let ids = many(project, &a.target, &doc)?;
        check_unlocked(project.vector(&doc)?, &ids)?;
        if let Some(w) = a.width {
            if w < 0.0 {
                return Err(Error::Invalid(format!("stroke width {w} is negative")));
            }
        }
        if let Some(m) = a.miter {
            if m < 1.0 {
                return Err(Error::Invalid(format!("miter limit {m} is below 1")));
            }
        }
        let clear = a
            .color
            .as_deref()
            .map(|c| c.trim().eq_ignore_ascii_case("none"))
            .unwrap_or(false);
        let color = match &a.color {
            Some(c) if !clear => parse_color(project, c)?,
            _ => None,
        };
        let v = project.vector_mut(&doc)?;
        for id in &ids {
            let Some(o) = v.object_mut(id) else { continue };
            if clear {
                o.stroke = None;
                continue;
            }
            let s = o.stroke.get_or_insert_with(|| {
                Stroke::solid(color.unwrap_or(dpaint_core::Color::BLACK), a.width.unwrap_or(1.0))
            });
            if let Some(c) = color {
                s.paint = Paint::solid(c);
            }
            if let Some(w) = a.width {
                s.width = w;
            }
            if let Some(c) = a.cap {
                s.cap = c;
            }
            if let Some(j) = a.join {
                s.join = j;
            }
            if let Some(m) = a.miter {
                s.miter = m;
            }
        }
        Ok(OpEffect::changed(&doc))
    }
}

// ---------------------------------------------------------------------------- gradient

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum GradientKind {
    Linear,
    Radial,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, schemars::JsonSchema, Default)]
#[serde(rename_all = "kebab-case")]
pub enum PaintSlot {
    #[default]
    Fill,
    Stroke,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct GradientArgs {
    /// Selector of the objects to paint.
    pub target: String,
    /// linear | radial
    pub kind: GradientKind,
    /// Colour stops as `offset:#rrggbb[aa]`, e.g. `["0:#ff0000", "1:#0000ff80"]`.
    pub stops: Vec<String>,
    /// Linear: start point. Defaults to the left edge of the object's bounding box.
    #[serde(default)]
    pub from: Option<[f64; 2]>,
    /// Linear: end point. Defaults to the right edge of the bounding box.
    #[serde(default)]
    pub to: Option<[f64; 2]>,
    /// Radial: centre. Defaults to the bounding-box centre.
    #[serde(default)]
    pub center: Option<[f64; 2]>,
    /// Radial: radius. Defaults to half the bounding-box diagonal.
    #[serde(default)]
    pub radius: Option<f64>,
    /// Radial: focal point for an off-centre highlight.
    #[serde(default)]
    pub focal: Option<[f64; 2]>,
    /// Paint the fill or the stroke.
    #[serde(default)]
    pub slot: PaintSlot,
}

vop!(Gradient, GradientArgs, "vector.style.gradient", "Set a linear or radial gradient on the fill or the stroke");

impl Gradient {
    fn run(project: &mut Project, a: GradientArgs, cx: &mut OpCx) -> Result<OpEffect> {
        let doc = doc_of(project, cx)?;
        let ids = many(project, &a.target, &doc)?;
        check_unlocked(project.vector(&doc)?, &ids)?;
        if a.stops.len() < 2 {
            return Err(Error::Invalid(format!(
                "a gradient needs at least two stops, got {}",
                a.stops.len()
            )));
        }
        let mut stops = Vec::with_capacity(a.stops.len());
        for s in &a.stops {
            let (o, c) = s.split_once(':').ok_or_else(|| {
                Error::Invalid(format!("stop '{s}' must look like '0.5:#ff0000'"))
            })?;
            let offset: f64 = o
                .trim()
                .parse()
                .map_err(|_| Error::Invalid(format!("stop offset '{o}' is not a number")))?;
            let color = parse_color(project, c)?
                .ok_or_else(|| Error::Invalid(format!("stop colour '{c}' cannot be 'none'")))?;
            stops.push(GradientStop {
                offset: offset.clamp(0.0, 1.0),
                color,
            });
        }
        if let Some(r) = a.radius {
            if r <= 0.0 {
                return Err(Error::Invalid("gradient radius must be positive".into()));
            }
        }
        let v = project.vector(&doc)?;
        let mut plan = Vec::new();
        for id in &ids {
            let b = geom::bbox(&geom::path_in_doc(v, id)?).ok_or_else(|| {
                Error::DegenerateGeometry(format!("'{id}' has no extent to place a gradient on"))
            })?;
            let paint = match a.kind {
                GradientKind::Linear => Paint::Linear {
                    stops: stops.clone(),
                    from: a.from.unwrap_or([b.x0, b.center().y]),
                    to: a.to.unwrap_or([b.x1, b.center().y]),
                },
                GradientKind::Radial => Paint::Radial {
                    stops: stops.clone(),
                    center: a.center.unwrap_or([b.center().x, b.center().y]),
                    radius: a
                        .radius
                        .unwrap_or_else(|| (b.width().hypot(b.height())) / 2.0),
                    focal: a.focal,
                },
            };
            plan.push((id.clone(), paint));
        }
        let v = project.vector_mut(&doc)?;
        for (id, paint) in plan {
            let Some(o) = v.object_mut(&id) else { continue };
            match a.slot {
                PaintSlot::Fill => o.fill = paint,
                PaintSlot::Stroke => match o.stroke.as_mut() {
                    Some(s) => s.paint = paint,
                    None => {
                        o.stroke = Some(Stroke {
                            paint,
                            width: 1.0,
                            cap: LineCap::default(),
                            join: LineJoin::default(),
                            miter: 4.0,
                            dash: Vec::new(),
                            dash_offset: 0.0,
                        })
                    }
                },
            }
        }
        Ok(OpEffect::changed(&doc))
    }
}

// -------------------------------------------------------------------------------- dash

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct DashArgs {
    /// Selector of the stroked objects.
    pub target: String,
    /// Dash/gap lengths, e.g. `[6, 2]`. An empty list makes the stroke solid.
    pub pattern: Vec<f64>,
    /// Phase offset into the pattern.
    #[serde(default)]
    pub offset: f64,
}

vop!(Dash, DashArgs, "vector.style.dash", "Set or clear a stroke dash pattern");

impl Dash {
    fn run(project: &mut Project, a: DashArgs, cx: &mut OpCx) -> Result<OpEffect> {
        let doc = doc_of(project, cx)?;
        let ids = many(project, &a.target, &doc)?;
        check_unlocked(project.vector(&doc)?, &ids)?;
        if a.pattern.iter().any(|d| *d < 0.0) {
            return Err(Error::Invalid("dash lengths must not be negative".into()));
        }
        let v = project.vector_mut(&doc)?;
        let mut eff = OpEffect::changed(&doc);
        for id in &ids {
            let Some(o) = v.object_mut(id) else { continue };
            match o.stroke.as_mut() {
                Some(s) => {
                    s.dash = a.pattern.clone();
                    s.dash_offset = a.offset;
                }
                None => {
                    eff = eff.warn(
                        "no-stroke",
                        id.to_string(),
                        "object has no stroke, so the dash pattern has nothing to apply to",
                    )
                }
            }
        }
        Ok(eff)
    }
}

// ----------------------------------------------------------------------------- opacity

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct OpacityArgs {
    /// Selector of the objects to change.
    pub target: String,
    /// Opacity 0..1.
    #[serde(default)]
    pub opacity: Option<f64>,
    /// Show or hide without changing opacity.
    #[serde(default)]
    pub visible: Option<bool>,
    /// Lock or unlock against further edits.
    #[serde(default)]
    pub locked: Option<bool>,
}

vop!(Opacity, OpacityArgs, "vector.style.opacity", "Set object opacity, visibility and lock state");

impl Opacity {
    fn run(project: &mut Project, a: OpacityArgs, cx: &mut OpCx) -> Result<OpEffect> {
        let doc = doc_of(project, cx)?;
        let ids = many(project, &a.target, &doc)?;
        if a.opacity.is_none() && a.visible.is_none() && a.locked.is_none() {
            return Err(Error::Invalid(
                "nothing to change: pass opacity, visible or locked".into(),
            ));
        }
        // Unlocking must be possible on a locked object, so only value edits are gated.
        if a.opacity.is_some() || a.visible.is_some() {
            if a.locked != Some(false) {
                check_unlocked(project.vector(&doc)?, &ids)?;
            }
        }
        let mut eff = OpEffect::changed(&doc);
        let clamped = a.opacity.map(|o| o.clamp(0.0, 1.0));
        if let (Some(raw), Some(c)) = (a.opacity, clamped) {
            if (raw - c).abs() > f64::EPSILON {
                eff = eff.warn("clamped", a.target.clone(), format!("opacity {raw} clamped to {c}"));
            }
        }
        let v = project.vector_mut(&doc)?;
        for id in &ids {
            let Some(o) = v.object_mut(id) else { continue };
            if let Some(x) = clamped {
                o.opacity = x as f32;
            }
            if let Some(x) = a.visible {
                o.visible = x;
            }
            if let Some(x) = a.locked {
                o.locked = x;
            }
        }
        Ok(eff)
    }
}

// ------------------------------------------------------------------------------- blend

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct BlendArgs {
    /// Selector of the objects to change.
    pub target: String,
    /// Blend mode, e.g. `multiply`, `screen`, `color-dodge`.
    pub mode: BlendMode,
}

vop!(Blend, BlendArgs, "vector.style.blend", "Set the blend mode used to composite an object");

impl Blend {
    fn run(project: &mut Project, a: BlendArgs, cx: &mut OpCx) -> Result<OpEffect> {
        let doc = doc_of(project, cx)?;
        let ids = many(project, &a.target, &doc)?;
        check_unlocked(project.vector(&doc)?, &ids)?;
        let v = project.vector_mut(&doc)?;
        for id in &ids {
            if let Some(o) = v.object_mut(id) {
                o.blend = a.mode;
            }
        }
        Ok(OpEffect::changed(&doc))
    }
}

// --------------------------------------------------------------------------- fill-rule

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct FillRuleArgs {
    /// Selector of the objects to change.
    pub target: String,
    /// nonzero | evenodd
    pub rule: FillRule,
}

vop!(FillRuleOp, FillRuleArgs, "vector.style.fill-rule", "Choose the nonzero or even-odd fill rule");

impl FillRuleOp {
    fn run(project: &mut Project, a: FillRuleArgs, cx: &mut OpCx) -> Result<OpEffect> {
        let doc = doc_of(project, cx)?;
        let ids = many(project, &a.target, &doc)?;
        check_unlocked(project.vector(&doc)?, &ids)?;
        let v = project.vector_mut(&doc)?;
        for id in &ids {
            if let Some(o) = v.object_mut(id) {
                o.fill_rule = a.rule;
            }
        }
        Ok(OpEffect::changed(&doc))
    }
}

// ------------------------------------------------------------------------------ marker

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum MarkerShape {
    Arrow,
    Triangle,
    Circle,
    Square,
    /// Remove markers previously generated for this object.
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, schemars::JsonSchema, Default)]
#[serde(rename_all = "kebab-case")]
pub enum MarkerWhere {
    Start,
    #[default]
    End,
    Both,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct MarkerArgs {
    /// Selector of the paths to mark.
    pub target: String,
    /// arrow | triangle | circle | square | none
    pub shape: MarkerShape,
    /// start | end | both
    #[serde(default)]
    pub at: MarkerWhere,
    /// Marker size; defaults to four times the stroke width.
    #[serde(default)]
    pub size: Option<f64>,
}

vop!(Marker, MarkerArgs, "vector.style.marker", "Place arrowheads or dots at a path's ends as real, editable geometry");

impl Marker {
    fn run(project: &mut Project, a: MarkerArgs, cx: &mut OpCx) -> Result<OpEffect> {
        let doc = doc_of(project, cx)?;
        let ids = many(project, &a.target, &doc)?;
        check_unlocked(project.vector(&doc)?, &ids)?;
        if let Some(s) = a.size {
            if s <= 0.0 {
                return Err(Error::Invalid("marker size must be positive".into()));
            }
        }
        let v = project.vector(&doc)?;
        let mut plan: Vec<(ObjectId, String, VObject)> = Vec::new();
        let mut drop_names: Vec<String> = Vec::new();
        for id in &ids {
            let o = v
                .object(id)
                .ok_or_else(|| Error::Invalid(format!("object '{id}' vanished")))?;
            let width = o.stroke.as_ref().map(|s| s.width).unwrap_or(1.0);
            let size = a.size.unwrap_or(width * 4.0);
            let paint = o
                .stroke
                .as_ref()
                .map(|s| s.paint.clone())
                .unwrap_or(Paint::solid(dpaint_core::Color::BLACK));
            let path = geom::path_in_doc(v, id)?;
            let ends = endpoints(&path);
            let Some((start, end)) = ends else {
                return Err(Error::DegenerateGeometry(format!(
                    "'{id}' has no open end to place a marker on"
                )));
            };
            for (slot, (pt, dir)) in [("start", start), ("end", end)] {
                let wanted = matches!(
                    (a.at, slot),
                    (MarkerWhere::Both, _) | (MarkerWhere::Start, "start") | (MarkerWhere::End, "end")
                );
                let name = format!("{}-marker-{slot}", o.name);
                drop_names.push(name.clone());
                if !wanted || a.shape == MarkerShape::None {
                    continue;
                }
                let geo = marker_path(a.shape, pt, dir, size);
                let mut m = VObject::new(ObjectId::from("obj_placeholder"), name.clone(), VKind::Path {
                    d: geom::to_d(&geo),
                });
                m.fill = paint.clone();
                plan.push((id.clone(), name, m));
            }
        }
        let v = project.vector_mut(&doc)?;
        let mut eff = OpEffect::changed(&doc);
        // Re-running replaces the markers generated last time rather than stacking them.
        let stale: Vec<ObjectId> = v
            .walk()
            .iter()
            .filter(|o| drop_names.contains(&o.name))
            .map(|o| o.id.clone())
            .collect();
        for s in stale {
            v.remove_object(&s);
            eff = eff.with_removed(s.to_string());
        }
        for (anchor, name, mut m) in plan {
            let id = fresh_id(v, &name);
            m.id = id.clone();
            let at = index_in_owner(v, &anchor).map(|i| i + 1).unwrap_or(0);
            match owner_list(v, &anchor) {
                Some(list) => {
                    let at = at.min(list.len());
                    list.insert(at, m);
                }
                None => v.objects.push(m),
            }
            eff = eff.with_created(id.to_string());
        }
        Ok(eff)
    }
}

/// First and last point of the path with its outgoing/incoming direction.
fn endpoints(p: &BezPath) -> Option<((Point, Vec2), (Point, Vec2))> {
    let (start, start_dir) = geom::sample(p, 0.0)?;
    let (end, end_dir) = geom::sample(p, 1.0)?;
    // A start marker points back down the path, an end marker points forward.
    Some(((start, -start_dir), (end, end_dir)))
}

fn marker_path(shape: MarkerShape, at: Point, dir: Vec2, size: f64) -> BezPath {
    let rot = Affine::translate((at.x, at.y)) * Affine::rotate(dir.y.atan2(dir.x));
    let h = size / 2.0;
    let mut p = BezPath::new();
    match shape {
        MarkerShape::Arrow => {
            p.move_to((0.0, 0.0));
            p.line_to((-size, -h));
            p.line_to((-size * 0.6, 0.0));
            p.line_to((-size, h));
            p.close_path();
        }
        MarkerShape::Triangle => {
            p.move_to((0.0, 0.0));
            p.line_to((-size, -h));
            p.line_to((-size, h));
            p.close_path();
        }
        MarkerShape::Circle => {
            p = dpaint_core::kurbo::Circle::new(Point::new(0.0, 0.0), h)
                .to_path(1e-3);
        }
        MarkerShape::Square => {
            p = dpaint_core::kurbo::Rect::new(-h, -h, h, h).to_path(1e-3);
        }
        MarkerShape::None => {}
    }
    rot * p
}

// -------------------------------------------------------------------------------- copy

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct CopyArgs {
    /// Selector of the object to copy style from.
    pub source: String,
    /// Selector of the objects to copy style onto.
    pub target: String,
    /// Copy the fill.
    #[serde(default = "yes")]
    pub fill: bool,
    /// Copy the stroke.
    #[serde(default = "yes")]
    pub stroke: bool,
    /// Copy opacity, blend mode and fill rule.
    #[serde(default = "yes")]
    pub compositing: bool,
}

fn yes() -> bool {
    true
}

vop!(Copy, CopyArgs, "vector.style.copy", "Copy fill, stroke and compositing from one object onto others");

impl Copy {
    fn run(project: &mut Project, a: CopyArgs, cx: &mut OpCx) -> Result<OpEffect> {
        let doc = doc_of(project, cx)?;
        let src = one(project, &a.source, &doc)?;
        let ids: Vec<ObjectId> = many(project, &a.target, &doc)?
            .into_iter()
            .filter(|i| *i != src)
            .collect();
        if ids.is_empty() {
            return Err(Error::Invalid(format!(
                "'{}' matched nothing other than the source object",
                a.target
            )));
        }
        check_unlocked(project.vector(&doc)?, &ids)?;
        let v = project.vector(&doc)?;
        let s = v
            .object(&src)
            .cloned()
            .ok_or_else(|| Error::Invalid("source vanished".into()))?;
        let v = project.vector_mut(&doc)?;
        for id in &ids {
            let Some(o) = v.object_mut(id) else { continue };
            if a.fill {
                o.fill = s.fill.clone();
            }
            if a.stroke {
                o.stroke = s.stroke.clone();
            }
            if a.compositing {
                o.opacity = s.opacity;
                o.blend = s.blend;
                o.fill_rule = s.fill_rule;
            }
        }
        Ok(OpEffect::changed(&doc))
    }
}
