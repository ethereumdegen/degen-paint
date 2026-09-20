//! `vector.path.*` — booleans, offsetting, stroking to outline, simplification and
//! node-level editing.

use super::{check_unlocked, doc_of, fresh_id, index_in_owner, many, one, owner_list, set_geometry};
use crate::boolean::{self, BoolOp};
use crate::geom::{self, DEFAULT_TOLERANCE};
use crate::pathops::{self, NodeType};
use crate::vop;
use dpaint_core::doc::common::{LineCap, LineJoin, Paint};
use dpaint_core::doc::vector::{VKind, VObject};
use dpaint_core::error::{Error, Result};
use dpaint_core::ids::ObjectId;
use dpaint_core::kurbo::{BezPath, Point};
use dpaint_core::project::Project;
use dpaint_core::{Op, OpCx, OpEffect};
use serde::Deserialize;

pub fn ops() -> Vec<Box<dyn Op>> {
    vec![
        Box::new(Boolean),
        Box::new(Offset),
        Box::new(OutlineStroke),
        Box::new(Simplify),
        Box::new(Reverse),
        Box::new(Close),
        Box::new(Append),
        Box::new(NodeInsert),
        Box::new(NodeRemove),
        Box::new(NodeMove),
        Box::new(NodeSetType),
        Box::new(RoundCorners),
    ]
}

fn tolerance(t: Option<f64>) -> f64 {
    t.filter(|v| *v > 0.0).unwrap_or(DEFAULT_TOLERANCE)
}

// ----------------------------------------------------------------------------- boolean

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct BooleanArgs {
    /// Selector naming two or more objects. The first in document order is the subject.
    pub target: String,
    /// union | subtract | intersect | exclude | divide
    pub op: BoolOp,
    /// Flattening tolerance in document units; smaller is more faithful and slower.
    #[serde(default)]
    pub tolerance: Option<f64>,
    /// Keep the operands instead of replacing them with the result.
    #[serde(default)]
    pub keep_originals: bool,
}

vop!(Boolean, BooleanArgs, "vector.path.boolean", "Union, subtract, intersect, exclude or divide the selected paths");

impl Boolean {
    fn run(project: &mut Project, a: BooleanArgs, cx: &mut OpCx) -> Result<OpEffect> {
        let doc = doc_of(project, cx)?;
        let ids = many(project, &a.target, &doc)?;
        if ids.len() < 2 {
            return Err(Error::Invalid(format!(
                "a boolean needs at least two objects, '{}' matched {}",
                a.target,
                ids.len()
            )));
        }
        check_unlocked(project.vector(&doc)?, &ids)?;
        let tol = tolerance(a.tolerance);
        let v = project.vector(&doc)?;
        let paths: Vec<BezPath> = ids
            .iter()
            .map(|i| geom::path_in_doc(v, i))
            .collect::<Result<_>>()?;
        let subject = ids[0].clone();
        let style = v
            .object(&subject)
            .cloned()
            .ok_or_else(|| Error::Invalid("subject vanished".into()))?;

        if a.op == BoolOp::Divide {
            let pieces = boolean::divide(&paths[0], &paths[1], style.fill_rule, tol)?;
            let v = project.vector_mut(&doc)?;
            let at = super::index_path(v, &subject).unwrap_or_default();
            let mut new_ids = Vec::new();
            for (n, piece) in pieces.iter().enumerate() {
                let id = fresh_id(v, &format!("{}-piece-{}", style.name, n + 1));
                let mut o = style.clone();
                o.id = id.clone();
                o.name = format!("{}-piece-{}", style.name, n + 1);
                o.transform = dpaint_core::doc::common::Transform::IDENTITY;
                o.kind = VKind::Path {
                    d: geom::to_d(piece),
                };
                let mut spot = at.clone();
                if let Some(last) = spot.last_mut() {
                    *last += n;
                }
                new_ids.push((spot, o, id));
            }
            let mut eff = OpEffect::changed(&doc);
            if !a.keep_originals {
                for id in &ids {
                    v.remove_object(id);
                    eff = eff.with_removed(id.to_string());
                }
            }
            for (spot, o, id) in new_ids {
                super::insert_at_path(v, &spot, o);
                eff = eff.with_created(id.to_string());
            }
            return Ok(eff);
        }

        let result = boolean::boolean(&paths, a.op, style.fill_rule, tol)?;
        if result.elements().is_empty() {
            return Err(Error::DegenerateGeometry(format!(
                "{:?} left nothing behind",
                a.op
            )));
        }
        let v = project.vector_mut(&doc)?;
        let mut eff = OpEffect::changed(&doc);
        if a.keep_originals {
            let id = fresh_id(v, &format!("{}-{:?}", style.name, a.op).to_lowercase());
            let mut o = style.clone();
            o.id = id.clone();
            o.transform = dpaint_core::doc::common::Transform::IDENTITY;
            o.kind = VKind::Path {
                d: geom::to_d(&result),
            };
            // The result belongs beside the subject, not at the document root.
            let at = super::index_path(v, &subject).unwrap_or_default();
            super::insert_at_path(v, &at, o);
            eff = eff.with_created(id.to_string());
        } else {
            for id in &ids[1..] {
                v.remove_object(id);
                eff = eff.with_removed(id.to_string());
            }
            set_geometry(v, &subject, &result)?;
        }
        Ok(eff)
    }
}

// ------------------------------------------------------------------------------ offset

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct OffsetArgs {
    /// Selector of the paths to offset.
    pub target: String,
    /// Distance in document units. Positive grows the filled region, negative shrinks it.
    pub distance: f64,
    #[serde(default)]
    pub tolerance: Option<f64>,
}

vop!(Offset, OffsetArgs, "vector.path.offset", "Grow or shrink a path's filled region by a distance");

impl Offset {
    fn run(project: &mut Project, a: OffsetArgs, cx: &mut OpCx) -> Result<OpEffect> {
        let doc = doc_of(project, cx)?;
        let ids = many(project, &a.target, &doc)?;
        check_unlocked(project.vector(&doc)?, &ids)?;
        if a.distance == 0.0 {
            return Err(Error::Invalid("offset distance must not be zero".into()));
        }
        let tol = tolerance(a.tolerance);
        let v = project.vector(&doc)?;
        let mut new: Vec<(ObjectId, BezPath)> = Vec::new();
        for id in &ids {
            let p = geom::path_in_doc(v, id)?;
            let rule = v.object(id).map(|o| o.fill_rule).unwrap_or_default();
            new.push((id.clone(), pathops::offset(&p, a.distance, rule, tol)?));
        }
        let v = project.vector_mut(&doc)?;
        for (id, p) in &new {
            set_geometry(v, id, p)?;
        }
        Ok(OpEffect::changed(&doc))
    }
}

// ---------------------------------------------------------------------- outline-stroke

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct OutlineStrokeArgs {
    /// Selector of the stroked objects to convert.
    pub target: String,
    /// Stroke width to outline; defaults to the object's own stroke width.
    #[serde(default)]
    pub width: Option<f64>,
    #[serde(default)]
    pub tolerance: Option<f64>,
    /// Keep the original fill as a separate object underneath the outline.
    #[serde(default)]
    pub keep_fill: bool,
}

vop!(OutlineStroke, OutlineStrokeArgs, "vector.path.outline-stroke", "Convert a stroke into a fillable outline, honouring width, caps, joins and dashes");

impl OutlineStroke {
    fn run(project: &mut Project, a: OutlineStrokeArgs, cx: &mut OpCx) -> Result<OpEffect> {
        let doc = doc_of(project, cx)?;
        let ids = many(project, &a.target, &doc)?;
        check_unlocked(project.vector(&doc)?, &ids)?;
        let tol = tolerance(a.tolerance);
        let v = project.vector(&doc)?;
        let mut plan: Vec<(ObjectId, BezPath, Paint, Option<VObject>)> = Vec::new();
        for id in &ids {
            let o = v
                .object(id)
                .ok_or_else(|| Error::Invalid(format!("object '{id}' vanished")))?;
            let stroke = o.stroke.clone();
            let width = a
                .width
                .or_else(|| stroke.as_ref().map(|s| s.width))
                .unwrap_or(0.0);
            if width <= 0.0 {
                return Err(Error::Invalid(format!(
                    "'{id}' has no stroke to outline; pass --width"
                )));
            }
            let p = geom::path_in_doc(v, id)?;
            let outline = pathops::outline_stroke(
                &p,
                width,
                stroke.as_ref().map(|s| s.cap).unwrap_or(LineCap::Butt),
                stroke.as_ref().map(|s| s.join).unwrap_or(LineJoin::Miter),
                stroke.as_ref().map(|s| s.miter).unwrap_or(4.0),
                stroke.as_ref().map(|s| s.dash.as_slice()).unwrap_or(&[]),
                stroke.as_ref().map(|s| s.dash_offset).unwrap_or(0.0),
                tol,
            )?;
            let paint = stroke.map(|s| s.paint).unwrap_or(Paint::None);
            let keep = if a.keep_fill && o.fill != Paint::None {
                let mut f = o.clone();
                f.stroke = None;
                Some(f)
            } else {
                None
            };
            plan.push((id.clone(), outline, paint, keep));
        }
        let v = project.vector_mut(&doc)?;
        let mut eff = OpEffect::changed(&doc);
        for (id, outline, paint, keep) in plan {
            if let Some(mut f) = keep {
                let nid = fresh_id(v, &format!("{}-fill", f.name));
                f.id = nid.clone();
                f.name = format!("{}-fill", f.name);
                let at = index_in_owner(v, &id).unwrap_or(0);
                if let Some(list) = owner_list(v, &id) {
                    list.insert(at, f);
                }
                eff = eff.with_created(nid.to_string());
            }
            set_geometry(v, &id, &outline)?;
            if let Some(o) = v.object_mut(&id) {
                o.fill = paint;
                o.stroke = None;
            }
        }
        Ok(eff)
    }
}

// ---------------------------------------------------------------------------- simplify

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SimplifyArgs {
    /// Selector of the paths to simplify.
    pub target: String,
    /// Maximum deviation from the original, in document units.
    #[serde(default)]
    pub tolerance: Option<f64>,
}

vop!(Simplify, SimplifyArgs, "vector.path.simplify", "Reduce node count while staying within a tolerance of the original");

impl Simplify {
    fn run(project: &mut Project, a: SimplifyArgs, cx: &mut OpCx) -> Result<OpEffect> {
        let doc = doc_of(project, cx)?;
        let ids = many(project, &a.target, &doc)?;
        check_unlocked(project.vector(&doc)?, &ids)?;
        let tol = a.tolerance.filter(|v| *v > 0.0).unwrap_or(1.0);
        let v = project.vector(&doc)?;
        let mut plan = Vec::new();
        for id in &ids {
            let before = geom::path_in_doc(v, id)?;
            let after = pathops::simplify(&before, tol);
            plan.push((
                id.clone(),
                before.segments().count(),
                after.segments().count(),
                after,
            ));
        }
        let v = project.vector_mut(&doc)?;
        let mut eff = OpEffect::changed(&doc);
        for (id, before, after, p) in plan {
            set_geometry(v, &id, &p)?;
            if after >= before {
                eff = eff.warn(
                    "no-reduction",
                    id.to_string(),
                    format!("already minimal at this tolerance: {before} segments in, {after} out"),
                );
            }
        }
        Ok(eff)
    }
}

// ----------------------------------------------------------------------------- reverse

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct PathTargetArgs {
    /// Selector of the paths to act on.
    pub target: String,
}

vop!(Reverse, PathTargetArgs, "vector.path.reverse", "Reverse path direction, flipping winding for nonzero fills");

impl Reverse {
    fn run(project: &mut Project, a: PathTargetArgs, cx: &mut OpCx) -> Result<OpEffect> {
        edit_paths(project, cx, &a.target, |p| Ok(pathops::reverse(p)))
    }
}

// ------------------------------------------------------------------------------- close

vop!(Close, PathTargetArgs, "vector.path.close", "Close every open subpath");

impl Close {
    fn run(project: &mut Project, a: PathTargetArgs, cx: &mut OpCx) -> Result<OpEffect> {
        edit_paths(project, cx, &a.target, |p| Ok(pathops::close_all(p)))
    }
}

/// Shared shape for the ops that rewrite one path in place.
fn edit_paths(
    project: &mut Project,
    cx: &OpCx,
    target: &str,
    f: impl Fn(&BezPath) -> Result<BezPath>,
) -> Result<OpEffect> {
    let doc = doc_of(project, cx)?;
    let ids = many(project, target, &doc)?;
    check_unlocked(project.vector(&doc)?, &ids)?;
    let v = project.vector(&doc)?;
    let mut plan = Vec::new();
    for id in &ids {
        plan.push((id.clone(), f(&geom::path_in_doc(v, id)?)?));
    }
    let v = project.vector_mut(&doc)?;
    for (id, p) in &plan {
        set_geometry(v, id, p)?;
    }
    Ok(OpEffect::changed(&doc))
}

// ------------------------------------------------------------------------------ append

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct AppendArgs {
    /// Selector of the object that receives the geometry.
    pub target: String,
    /// Selector of the objects whose geometry is appended.
    pub source: String,
    /// Join the end of each part to the start of the next instead of keeping subpaths.
    #[serde(default)]
    pub connect: bool,
    /// Delete the source objects once their geometry has moved.
    #[serde(default = "yes")]
    pub consume: bool,
}

fn yes() -> bool {
    true
}

vop!(Append, AppendArgs, "vector.path.append", "Append other objects' geometry onto one path");

impl Append {
    fn run(project: &mut Project, a: AppendArgs, cx: &mut OpCx) -> Result<OpEffect> {
        let doc = doc_of(project, cx)?;
        let target = one(project, &a.target, &doc)?;
        let sources = many(project, &a.source, &doc)?;
        let sources: Vec<ObjectId> = sources.into_iter().filter(|s| *s != target).collect();
        if sources.is_empty() {
            return Err(Error::Invalid(format!(
                "'{}' matched nothing other than the target",
                a.source
            )));
        }
        check_unlocked(project.vector(&doc)?, &[target.clone()])?;
        let v = project.vector(&doc)?;
        let mut parts = vec![geom::path_in_doc(v, &target)?];
        for s in &sources {
            parts.push(geom::path_in_doc(v, s)?);
        }
        let joined = pathops::append(&parts, a.connect);
        let v = project.vector_mut(&doc)?;
        set_geometry(v, &target, &joined)?;
        let mut eff = OpEffect::changed(&doc);
        if a.consume {
            for s in &sources {
                v.remove_object(s);
                eff = eff.with_removed(s.to_string());
            }
        }
        Ok(eff)
    }
}

// -------------------------------------------------------------------------- node edits

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct NodeInsertArgs {
    /// Selector of the path to edit.
    pub target: String,
    /// Which subpath, 0-based.
    #[serde(default)]
    pub subpath: usize,
    /// The node the new one follows, 0-based.
    pub index: usize,
    /// Position along that segment, 0..1.
    #[serde(default = "half")]
    pub t: f64,
}

fn half() -> f64 {
    0.5
}

vop!(NodeInsert, NodeInsertArgs, "vector.path.node-insert", "Insert a node partway along a segment without changing the curve");

impl NodeInsert {
    fn run(project: &mut Project, a: NodeInsertArgs, cx: &mut OpCx) -> Result<OpEffect> {
        node_edit(project, cx, &a.target, |p| {
            pathops::node_insert(p, a.subpath, a.index, a.t)
        })
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct NodeIndexArgs {
    /// Selector of the path to edit.
    pub target: String,
    #[serde(default)]
    pub subpath: usize,
    /// Node index, 0-based.
    pub index: usize,
}

vop!(NodeRemove, NodeIndexArgs, "vector.path.node-remove", "Delete a node, joining its neighbours");

impl NodeRemove {
    fn run(project: &mut Project, a: NodeIndexArgs, cx: &mut OpCx) -> Result<OpEffect> {
        node_edit(project, cx, &a.target, |p| {
            pathops::node_remove(p, a.subpath, a.index)
        })
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct NodeMoveArgs {
    /// Selector of the path to edit.
    pub target: String,
    #[serde(default)]
    pub subpath: usize,
    pub index: usize,
    /// Destination x, or the x delta when `relative` is set.
    pub x: f64,
    pub y: f64,
    #[serde(default)]
    pub relative: bool,
}

vop!(NodeMove, NodeMoveArgs, "vector.path.node-move", "Move a node and drag its handles with it");

impl NodeMove {
    fn run(project: &mut Project, a: NodeMoveArgs, cx: &mut OpCx) -> Result<OpEffect> {
        node_edit(project, cx, &a.target, |p| {
            pathops::node_move(p, a.subpath, a.index, Point::new(a.x, a.y), a.relative)
        })
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct NodeTypeArgs {
    /// Selector of the path to edit.
    pub target: String,
    #[serde(default)]
    pub subpath: usize,
    pub index: usize,
    /// corner | smooth | symmetric
    pub kind: NodeType,
}

vop!(NodeSetType, NodeTypeArgs, "vector.path.node-set-type", "Make a node a corner, a smooth tangent, or a symmetric tangent");

impl NodeSetType {
    fn run(project: &mut Project, a: NodeTypeArgs, cx: &mut OpCx) -> Result<OpEffect> {
        node_edit(project, cx, &a.target, |p| {
            pathops::node_set_type(p, a.subpath, a.index, a.kind)
        })
    }
}

fn node_edit(
    project: &mut Project,
    cx: &OpCx,
    target: &str,
    f: impl Fn(&BezPath) -> Result<BezPath>,
) -> Result<OpEffect> {
    let doc = doc_of(project, cx)?;
    let id = one(project, target, &doc)?;
    check_unlocked(project.vector(&doc)?, &[id.clone()])?;
    let v = project.vector(&doc)?;
    let edited = f(&geom::path_in_doc(v, &id)?)?;
    let v = project.vector_mut(&doc)?;
    set_geometry(v, &id, &edited)?;
    Ok(OpEffect::changed(&doc))
}

// ----------------------------------------------------------------------- round-corners

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct RoundCornersArgs {
    /// Selector of the paths to round.
    pub target: String,
    /// Corner radius; clamped per corner to half the shorter adjacent segment.
    pub radius: f64,
}

vop!(RoundCorners, RoundCornersArgs, "vector.path.round-corners", "Replace straight-segment corners with circular fillets");

impl RoundCorners {
    fn run(project: &mut Project, a: RoundCornersArgs, cx: &mut OpCx) -> Result<OpEffect> {
        // `!(a < b)` is deliberate: it is true when the values are incomparable, which is the
        // branch degenerate geometry needs. Rewriting it as `a >= b` would silently drop NaN.
        #[allow(clippy::neg_cmp_op_on_partial_ord)]
        if !(a.radius > 0.0) {
            return Err(Error::Invalid("radius must be greater than zero".into()));
        }
        edit_paths(project, cx, &a.target, |p| pathops::round_corners(p, a.radius))
    }
}
