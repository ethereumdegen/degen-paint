//! `vector.transform.*` — moving, rotating, scaling, skewing, baking and arranging.

use super::{check_unlocked, doc_of, many, set_geometry};
use crate::geom;
use crate::vop;
use dpaint_core::doc::common::Transform as DocTransform;
use dpaint_core::doc::vector::{VKind, VectorDoc};
use dpaint_core::error::{Error, Result};
use dpaint_core::ids::ObjectId;
use dpaint_core::kurbo::{Affine, Rect as KRect};
use dpaint_core::project::Project;
use dpaint_core::{Op, OpCx, OpEffect};
use serde::Deserialize;

pub fn ops() -> Vec<Box<dyn Op>> {
    vec![
        Box::new(Translate),
        Box::new(Rotate),
        Box::new(Scale),
        Box::new(Skew),
        Box::new(Matrix),
        Box::new(Flatten),
        Box::new(Align),
        Box::new(Distribute),
    ]
}

/// Apply a document-space matrix to an object by composing it into the object's own
/// transform, with the ancestor transform divided out so the visible result matches.
fn apply_world(v: &mut VectorDoc, id: &ObjectId, m: Affine) -> Result<()> {
    let (_, parent) = geom::locate(v, id)
        .ok_or_else(|| Error::Invalid(format!("object '{id}' vanished")))?;
    let local = parent.inverse() * m * parent;
    let o = v
        .object_mut(id)
        .ok_or_else(|| Error::Invalid(format!("object '{id}' vanished")))?;
    o.transform = DocTransform::from_kurbo(local * o.transform.to_kurbo());
    Ok(())
}

/// Bounding box of a set of objects in document space.
fn selection_bbox(v: &VectorDoc, ids: &[ObjectId]) -> Result<KRect> {
    let mut r: Option<KRect> = None;
    for id in ids {
        if let Some(b) = geom::bbox(&geom::path_in_doc(v, id)?) {
            r = Some(match r {
                Some(p) => p.union(b),
                None => b,
            });
        }
    }
    r.ok_or_else(|| Error::DegenerateGeometry("the selection has no extent".into()))
}

fn transform_each(
    project: &mut Project,
    cx: &OpCx,
    target: &str,
    build: impl Fn(&VectorDoc, &[ObjectId]) -> Result<Affine>,
) -> Result<OpEffect> {
    let doc = doc_of(project, cx)?;
    let ids = many(project, target, &doc)?;
    check_unlocked(project.vector(&doc)?, &ids)?;
    let m = build(project.vector(&doc)?, &ids)?;
    let v = project.vector_mut(&doc)?;
    for id in &ids {
        apply_world(v, id, m)?;
    }
    Ok(OpEffect::changed(&doc))
}

/// Pivot shared by rotate, scale and skew: an explicit point, else the selection centre.
fn pivot(v: &VectorDoc, ids: &[ObjectId], explicit: Option<[f64; 2]>) -> Result<(f64, f64)> {
    Ok(match explicit {
        Some(p) => (p[0], p[1]),
        None => {
            let b = selection_bbox(v, ids)?;
            (b.center().x, b.center().y)
        }
    })
}

// --------------------------------------------------------------------------- translate

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct TranslateArgs {
    /// Selector of the objects to move.
    pub target: String,
    #[serde(default)]
    pub dx: f64,
    #[serde(default)]
    pub dy: f64,
}

vop!(Translate, TranslateArgs, "vector.transform.translate", "Move objects by an offset");

impl Translate {
    fn run(project: &mut Project, a: TranslateArgs, cx: &mut OpCx) -> Result<OpEffect> {
        if a.dx == 0.0 && a.dy == 0.0 {
            return Err(Error::Invalid("translate by zero does nothing".into()));
        }
        transform_each(project, cx, &a.target, |_, _| {
            Ok(Affine::translate((a.dx, a.dy)))
        })
    }
}

// ------------------------------------------------------------------------------ rotate

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct RotateArgs {
    /// Selector of the objects to rotate.
    pub target: String,
    /// Clockwise degrees.
    pub degrees: f64,
    /// Pivot point; defaults to the selection's bounding-box centre.
    #[serde(default)]
    pub around: Option<[f64; 2]>,
}

vop!(Rotate, RotateArgs, "vector.transform.rotate", "Rotate objects about a pivot");

impl Rotate {
    fn run(project: &mut Project, a: RotateArgs, cx: &mut OpCx) -> Result<OpEffect> {
        transform_each(project, cx, &a.target, |v, ids| {
            let (px, py) = pivot(v, ids, a.around)?;
            Ok(Affine::translate((px, py))
                * Affine::rotate(a.degrees.to_radians())
                * Affine::translate((-px, -py)))
        })
    }
}

// ------------------------------------------------------------------------------- scale

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ScaleArgs {
    /// Selector of the objects to scale.
    pub target: String,
    /// Horizontal factor.
    pub sx: f64,
    /// Vertical factor; defaults to `sx` for a uniform scale.
    #[serde(default)]
    pub sy: Option<f64>,
    /// Pivot point; defaults to the selection's bounding-box centre.
    #[serde(default)]
    pub around: Option<[f64; 2]>,
}

vop!(Scale, ScaleArgs, "vector.transform.scale", "Scale objects about a pivot; negative factors mirror");

impl Scale {
    fn run(project: &mut Project, a: ScaleArgs, cx: &mut OpCx) -> Result<OpEffect> {
        let sy = a.sy.unwrap_or(a.sx);
        if a.sx == 0.0 || sy == 0.0 {
            return Err(Error::DegenerateGeometry(
                "a zero scale factor would collapse the objects".into(),
            ));
        }
        transform_each(project, cx, &a.target, |v, ids| {
            let (px, py) = pivot(v, ids, a.around)?;
            Ok(Affine::translate((px, py))
                * Affine::scale_non_uniform(a.sx, sy)
                * Affine::translate((-px, -py)))
        })
    }
}

// -------------------------------------------------------------------------------- skew

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SkewArgs {
    /// Selector of the objects to skew.
    pub target: String,
    /// Horizontal skew angle in degrees.
    #[serde(default)]
    pub x_degrees: f64,
    /// Vertical skew angle in degrees.
    #[serde(default)]
    pub y_degrees: f64,
    #[serde(default)]
    pub around: Option<[f64; 2]>,
}

vop!(Skew, SkewArgs, "vector.transform.skew", "Skew objects about a pivot");

impl Skew {
    fn run(project: &mut Project, a: SkewArgs, cx: &mut OpCx) -> Result<OpEffect> {
        if a.x_degrees == 0.0 && a.y_degrees == 0.0 {
            return Err(Error::Invalid("skew by zero does nothing".into()));
        }
        for d in [a.x_degrees, a.y_degrees] {
            if (d.abs() - 90.0).abs() < 1e-9 {
                return Err(Error::DegenerateGeometry(
                    "a 90 degree skew is undefined".into(),
                ));
            }
        }
        transform_each(project, cx, &a.target, |v, ids| {
            let (px, py) = pivot(v, ids, a.around)?;
            let m = Affine::new([
                1.0,
                a.y_degrees.to_radians().tan(),
                a.x_degrees.to_radians().tan(),
                1.0,
                0.0,
                0.0,
            ]);
            Ok(Affine::translate((px, py)) * m * Affine::translate((-px, -py)))
        })
    }
}

// ------------------------------------------------------------------------------ matrix

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct MatrixArgs {
    /// Selector of the objects to transform.
    pub target: String,
    /// SVG-order affine `[a, b, c, d, e, f]`.
    pub matrix: [f64; 6],
    /// Replace the object's transform instead of composing onto it.
    #[serde(default)]
    pub replace: bool,
}

vop!(Matrix, MatrixArgs, "vector.transform.matrix", "Apply an arbitrary affine matrix");

impl Matrix {
    fn run(project: &mut Project, a: MatrixArgs, cx: &mut OpCx) -> Result<OpEffect> {
        let m = Affine::new(a.matrix);
        if m.determinant().abs() < 1e-12 {
            return Err(Error::DegenerateGeometry(
                "the matrix is singular and would collapse the objects".into(),
            ));
        }
        let doc = doc_of(project, cx)?;
        let ids = many(project, &a.target, &doc)?;
        check_unlocked(project.vector(&doc)?, &ids)?;
        let v = project.vector_mut(&doc)?;
        for id in &ids {
            if a.replace {
                if let Some(o) = v.object_mut(id) {
                    o.transform = DocTransform(a.matrix);
                }
            } else {
                apply_world(v, id, m)?;
            }
        }
        Ok(OpEffect::changed(&doc))
    }
}

// ----------------------------------------------------------------------------- flatten

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct FlattenArgs {
    /// Selector of the objects whose transform should be baked into their geometry.
    pub target: String,
}

vop!(Flatten, FlattenArgs, "vector.transform.flatten", "Bake transforms into geometry so the object's matrix becomes the identity");

impl Flatten {
    fn run(project: &mut Project, a: FlattenArgs, cx: &mut OpCx) -> Result<OpEffect> {
        let doc = doc_of(project, cx)?;
        let ids = many(project, &a.target, &doc)?;
        check_unlocked(project.vector(&doc)?, &ids)?;
        let v = project.vector(&doc)?;
        let mut plan = Vec::new();
        for id in &ids {
            let is_group = matches!(v.object(id).map(|o| &o.kind), Some(VKind::Group { .. }));
            if is_group {
                continue;
            }
            plan.push((id.clone(), geom::path_in_doc(v, id)?));
        }
        if plan.is_empty() {
            return Err(Error::Invalid(format!(
                "'{}' matched only groups; flatten their children instead",
                a.target
            )));
        }
        let v = project.vector_mut(&doc)?;
        for (id, p) in &plan {
            set_geometry(v, id, p)?;
        }
        Ok(OpEffect::changed(&doc))
    }
}

// ------------------------------------------------------------------------------- align

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum AlignEdge {
    Left,
    HCenter,
    Right,
    Top,
    VCenter,
    Bottom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, schemars::JsonSchema, Default)]
#[serde(rename_all = "kebab-case")]
pub enum AlignTo {
    /// The bounding box of everything selected.
    #[default]
    Selection,
    /// The first artboard of the document.
    Artboard,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct AlignArgs {
    /// Selector of the objects to align.
    pub target: String,
    /// left | h-center | right | top | v-center | bottom
    pub edge: AlignEdge,
    /// Align within the selection's bounds or the artboard's.
    #[serde(default)]
    pub to: AlignTo,
}

vop!(Align, AlignArgs, "vector.transform.align", "Align objects to a shared edge or centre line");

impl Align {
    fn run(project: &mut Project, a: AlignArgs, cx: &mut OpCx) -> Result<OpEffect> {
        let doc = doc_of(project, cx)?;
        let ids = many(project, &a.target, &doc)?;
        check_unlocked(project.vector(&doc)?, &ids)?;
        let v = project.vector(&doc)?;
        let frame = match a.to {
            AlignTo::Selection => selection_bbox(v, &ids)?,
            AlignTo::Artboard => crate::raster::doc_bounds(v),
        };
        let mut moves = Vec::new();
        for id in &ids {
            let b = match geom::bbox(&geom::path_in_doc(v, id)?) {
                Some(b) => b,
                None => continue,
            };
            let (dx, dy) = match a.edge {
                AlignEdge::Left => (frame.x0 - b.x0, 0.0),
                AlignEdge::Right => (frame.x1 - b.x1, 0.0),
                AlignEdge::HCenter => (frame.center().x - b.center().x, 0.0),
                AlignEdge::Top => (0.0, frame.y0 - b.y0),
                AlignEdge::Bottom => (0.0, frame.y1 - b.y1),
                AlignEdge::VCenter => (0.0, frame.center().y - b.center().y),
            };
            moves.push((id.clone(), dx, dy));
        }
        let v = project.vector_mut(&doc)?;
        for (id, dx, dy) in moves {
            apply_world(v, &id, Affine::translate((dx, dy)))?;
        }
        Ok(OpEffect::changed(&doc))
    }
}

// -------------------------------------------------------------------------- distribute

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Axis {
    Horizontal,
    Vertical,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct DistributeArgs {
    /// Selector naming three or more objects.
    pub target: String,
    /// horizontal | vertical
    pub axis: Axis,
    /// Space edges evenly instead of centres.
    #[serde(default)]
    pub gaps: bool,
}

vop!(Distribute, DistributeArgs, "vector.transform.distribute", "Space objects evenly along an axis");

impl Distribute {
    fn run(project: &mut Project, a: DistributeArgs, cx: &mut OpCx) -> Result<OpEffect> {
        let doc = doc_of(project, cx)?;
        let ids = many(project, &a.target, &doc)?;
        if ids.len() < 3 {
            return Err(Error::Invalid(format!(
                "distributing needs at least three objects, '{}' matched {}",
                a.target,
                ids.len()
            )));
        }
        check_unlocked(project.vector(&doc)?, &ids)?;
        let v = project.vector(&doc)?;
        let mut boxes: Vec<(ObjectId, KRect)> = Vec::new();
        for id in &ids {
            if let Some(b) = geom::bbox(&geom::path_in_doc(v, id)?) {
                boxes.push((id.clone(), b));
            }
        }
        let horiz = a.axis == Axis::Horizontal;
        boxes.sort_by(|x, y| {
            let (p, q) = if horiz {
                (x.1.center().x, y.1.center().x)
            } else {
                (x.1.center().y, y.1.center().y)
            };
            p.partial_cmp(&q).unwrap_or(std::cmp::Ordering::Equal)
        });
        let n = boxes.len();
        let first = boxes[0].1;
        let last = boxes[n - 1].1;
        let mut moves = Vec::new();
        if a.gaps {
            let span = if horiz {
                last.x1 - first.x0
            } else {
                last.y1 - first.y0
            };
            let used: f64 = boxes
                .iter()
                .map(|(_, b)| if horiz { b.width() } else { b.height() })
                .sum();
            let gap = (span - used) / (n - 1) as f64;
            let mut cursor = if horiz { first.x0 } else { first.y0 };
            for (id, b) in &boxes {
                let cur = if horiz { b.x0 } else { b.y0 };
                let d = cursor - cur;
                moves.push((id.clone(), if horiz { (d, 0.0) } else { (0.0, d) }));
                cursor += (if horiz { b.width() } else { b.height() }) + gap;
            }
        } else {
            let (a0, a1) = if horiz {
                (first.center().x, last.center().x)
            } else {
                (first.center().y, last.center().y)
            };
            let step = (a1 - a0) / (n - 1) as f64;
            for (i, (id, b)) in boxes.iter().enumerate() {
                let want = a0 + step * i as f64;
                let cur = if horiz { b.center().x } else { b.center().y };
                let d = want - cur;
                moves.push((id.clone(), if horiz { (d, 0.0) } else { (0.0, d) }));
            }
        }
        let v = project.vector_mut(&doc)?;
        for (id, (dx, dy)) in moves {
            if dx != 0.0 || dy != 0.0 {
                apply_world(v, &id, Affine::translate((dx, dy)))?;
            }
        }
        Ok(OpEffect::changed(&doc))
    }
}

/// Exposed for the artboard ops, which need the same measurement.
pub(crate) fn content_bounds(v: &VectorDoc) -> Option<KRect> {
    let ids: Vec<ObjectId> = v.objects.iter().map(|o| o.id.clone()).collect();
    selection_bbox(v, &ids).ok()
}
