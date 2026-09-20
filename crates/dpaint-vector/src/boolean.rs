//! Boolean path algebra on top of `i_overlay`.
//!
//! Béziers are flattened to contours at an explicit tolerance, the overlay runs on the
//! contours, and the result comes back as closed polygonal subpaths — outer rings first,
//! then their holes. Self-intersecting and coincident-edge input is handled by the overlay
//! itself, so no caller has to pre-clean its geometry.

use crate::geom::flatten;
use dpaint_core::doc::common::FillRule as DocFillRule;
use dpaint_core::error::{Error, Result};
use dpaint_core::kurbo::{BezPath, Point};
use i_overlay::core::fill_rule::FillRule;
use i_overlay::core::overlay_rule::OverlayRule;
use i_overlay::float::single::SingleFloatOverlay;
use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum BoolOp {
    /// Everything covered by either operand.
    Union,
    /// The first operand with every later operand removed.
    Subtract,
    /// Only the area covered by all operands.
    Intersect,
    /// Area covered by an odd number of operands.
    Exclude,
    /// Cut the first operand into the pieces the second covers and the pieces it does not.
    Divide,
}

impl BoolOp {
    fn rule(self) -> OverlayRule {
        match self {
            BoolOp::Union => OverlayRule::Union,
            BoolOp::Subtract => OverlayRule::Difference,
            // `divide` is routed to `divide()`, which needs two passes.
            BoolOp::Divide => OverlayRule::Difference,
            BoolOp::Intersect => OverlayRule::Intersect,
            BoolOp::Exclude => OverlayRule::Xor,
        }
    }
}

fn fill_rule(r: DocFillRule) -> FillRule {
    match r {
        DocFillRule::Nonzero => FillRule::NonZero,
        DocFillRule::Evenodd => FillRule::EvenOdd,
    }
}

type Contours = Vec<Vec<[f64; 2]>>;

/// Flatten a path into closed contours. Open subpaths are implicitly closed, which is what
/// every 2D boolean engine does with an unclosed fill region.
pub fn contours(path: &BezPath, tol: f64) -> Contours {
    flatten(path, tol)
        .into_iter()
        .filter(|sp| sp.points.len() >= 3)
        .map(|sp| sp.points.iter().map(|p| [p.x, p.y]).collect())
        .collect()
}

fn shapes_to_bez(shapes: Vec<Vec<Vec<[f64; 2]>>>) -> BezPath {
    let mut out = BezPath::new();
    for shape in shapes {
        for contour in shape {
            push_contour(&mut out, &contour);
        }
    }
    out
}

fn push_contour(out: &mut BezPath, c: &[[f64; 2]]) {
    if c.len() < 3 {
        return;
    }
    out.move_to(Point::new(c[0][0], c[0][1]));
    for p in &c[1..] {
        out.line_to(Point::new(p[0], p[1]));
    }
    out.close_path();
}

/// `op` applied left to right: `paths[0] op paths[1] op …`.
pub fn boolean(paths: &[BezPath], op: BoolOp, rule: DocFillRule, tol: f64) -> Result<BezPath> {
    if paths.len() < 2 {
        return Err(Error::DegenerateGeometry(
            "a boolean needs at least two operands".into(),
        ));
    }
    if op == BoolOp::Divide {
        return Err(Error::Invalid(
            "divide produces several shapes; call `divide` instead".into(),
        ));
    }
    let fr = fill_rule(rule);
    let mut acc = contours(&paths[0], tol);
    for p in &paths[1..] {
        let clip = contours(p, tol);
        if clip.is_empty() {
            continue;
        }
        if acc.is_empty() {
            // Union and exclude with nothing on the left still produce the right operand.
            if matches!(op, BoolOp::Union | BoolOp::Exclude) {
                acc = clip;
            }
            continue;
        }
        let shapes = acc.overlay(&clip, op.rule(), fr);
        acc = shapes.into_iter().flatten().collect();
    }
    let mut out = BezPath::new();
    for c in &acc {
        push_contour(&mut out, c);
    }
    Ok(out)
}

/// `divide` yields several results: the overlap, then the remainder of the subject.
/// Empty pieces are dropped, so cutting with a shape that misses returns one piece.
pub fn divide(
    subject: &BezPath,
    cutter: &BezPath,
    rule: DocFillRule,
    tol: f64,
) -> Result<Vec<BezPath>> {
    let fr = fill_rule(rule);
    let subj = contours(subject, tol);
    let clip = contours(cutter, tol);
    if subj.is_empty() {
        return Err(Error::DegenerateGeometry(
            "the object being divided has no closed area".into(),
        ));
    }
    if clip.is_empty() {
        return Err(Error::DegenerateGeometry(
            "the cutting object has no closed area".into(),
        ));
    }
    let mut out = Vec::new();
    for rule_kind in [OverlayRule::Intersect, OverlayRule::Difference] {
        let shapes = subj.overlay(&clip, rule_kind, fr);
        for shape in shapes {
            let mut p = BezPath::new();
            for c in shape {
                push_contour(&mut p, &c);
            }
            if !p.is_empty() {
                out.push(p);
            }
        }
    }
    if out.is_empty() {
        return Err(Error::DegenerateGeometry(
            "divide produced no pieces".into(),
        ));
    }
    Ok(out)
}

/// Resolve self-intersections and coincident edges into clean non-overlapping rings.
/// This is what every offset/outline result is pushed through before it is stored.
pub fn simplify_self(path: &BezPath, rule: DocFillRule, tol: f64) -> BezPath {
    use i_overlay::float::simplify::SimplifyShape;
    let cs = contours(path, tol);
    if cs.is_empty() {
        return BezPath::new();
    }
    shapes_to_bez(cs.simplify_shape(fill_rule(rule)))
}

/// Union of many contour sets at once — the shape assembler used by the stroker.
pub fn union_contours(sets: Vec<Contours>, tol: f64) -> BezPath {
    let _ = tol;
    let merged: Contours = sets.into_iter().flatten().collect();
    if merged.is_empty() {
        return BezPath::new();
    }
    use i_overlay::float::simplify::SimplifyShape;
    shapes_to_bez(merged.simplify_shape(FillRule::NonZero))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::area;
    use dpaint_core::kurbo::{Circle, Rect, Shape};

    fn sq(x: f64, y: f64, s: f64) -> BezPath {
        Rect::new(x, y, x + s, y + s).to_path(1e-4)
    }

    #[test]
    fn subtracting_a_circle_from_a_square_removes_the_circle_area() {
        let square = sq(0.0, 0.0, 100.0);
        let circle = Circle::new((50.0, 50.0), 20.0).to_path(1e-4);
        let out = boolean(
            &[square, circle],
            BoolOp::Subtract,
            DocFillRule::Nonzero,
            0.01,
        )
        .unwrap();
        let expect = 10_000.0 - std::f64::consts::PI * 400.0;
        assert!(
            (area(&out) - expect).abs() < 2.0,
            "area {} should be about {expect}",
            area(&out)
        );
    }

    #[test]
    fn union_of_two_overlapping_circles_is_smaller_than_their_sum() {
        let a = Circle::new((0.0, 0.0), 10.0).to_path(1e-4);
        let b = Circle::new((8.0, 0.0), 10.0).to_path(1e-4);
        let each = std::f64::consts::PI * 100.0;
        let out = boolean(&[a, b], BoolOp::Union, DocFillRule::Nonzero, 0.01).unwrap();
        let u = area(&out);
        assert!(u < 2.0 * each - 1.0, "union {u} < {}", 2.0 * each);
        assert!(u > each, "union {u} > one circle {each}");
    }

    #[test]
    fn intersect_keeps_only_the_overlap_and_exclude_keeps_only_the_rest() {
        let a = sq(0.0, 0.0, 10.0);
        let b = sq(5.0, 0.0, 10.0);
        let i = boolean(
            &[a.clone(), b.clone()],
            BoolOp::Intersect,
            DocFillRule::Nonzero,
            0.01,
        )
        .unwrap();
        assert!((area(&i) - 50.0).abs() < 0.01, "overlap area {}", area(&i));
        let x = boolean(&[a, b], BoolOp::Exclude, DocFillRule::Nonzero, 0.01).unwrap();
        assert!((area(&x) - 100.0).abs() < 0.01, "xor area {}", area(&x));
    }

    #[test]
    fn divide_splits_a_square_into_inside_and_outside_pieces() {
        let a = sq(0.0, 0.0, 10.0);
        let b = sq(5.0, -5.0, 10.0);
        let pieces = divide(&a, &b, DocFillRule::Nonzero, 0.01).unwrap();
        assert_eq!(pieces.len(), 2, "one piece inside the cutter, one outside");
        let total: f64 = pieces.iter().map(area).sum();
        assert!(
            (total - 100.0).abs() < 0.05,
            "pieces reassemble the square: {total}"
        );
    }

    #[test]
    fn a_self_intersecting_bowtie_is_cleaned_without_panicking() {
        let mut bow = BezPath::new();
        bow.move_to((0.0, 0.0));
        bow.line_to((10.0, 10.0));
        bow.line_to((10.0, 0.0));
        bow.line_to((0.0, 10.0));
        bow.close_path();
        let clean = simplify_self(&bow, DocFillRule::Nonzero, 0.01);
        assert!(
            (area(&clean) - 50.0).abs() < 0.1,
            "two triangles of 25: {}",
            area(&clean)
        );
    }

    #[test]
    fn coincident_edges_union_into_one_rectangle() {
        let a = sq(0.0, 0.0, 10.0);
        let b = sq(10.0, 0.0, 10.0);
        let out = boolean(&[a, b], BoolOp::Union, DocFillRule::Nonzero, 0.01).unwrap();
        assert!((area(&out) - 200.0).abs() < 0.01, "area {}", area(&out));
        let subpaths = crate::geom::split_subpaths(&out);
        assert_eq!(subpaths.len(), 1, "the shared edge is dissolved");
    }
}
