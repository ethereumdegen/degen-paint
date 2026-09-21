//! Geometry core: every vector object resolves to one `kurbo::BezPath` in document space.
//!
//! This is the crate's load-bearing function. The rasterizer, the SVG writer, the boolean
//! engine, the measure ops and the 3D extruder in `dpaint-model3d` all consume exactly the
//! outline produced here, so shapes, text and groups can never disagree about their outline.

use crate::text;
use dpaint_core::doc::vector::{PathSide, VKind, VObject, VectorDoc};
use dpaint_core::error::{Error, Result};
use dpaint_core::ids::{DocId, ObjectId};
use dpaint_core::kurbo::{
    Affine, BezPath, Ellipse, ParamCurve, ParamCurveArclen, ParamCurveDeriv, ParamCurveNearest,
    PathEl, PathSeg, Point, Rect as KRect, RoundedRect, Shape, Vec2,
};
use dpaint_core::project::Project;

/// Default flattening tolerance in document units. Fine enough that a 1000 px shape stays
/// sub-pixel, coarse enough that boolean input stays small.
pub const DEFAULT_TOLERANCE: f64 = 0.05;

/// Resolved outline of one object in document space, with its own and every ancestor
/// transform applied. Subpaths are preserved, so a ring stays outer-contour plus hole.
pub fn path_of(project: &Project, doc: &DocId, object: &ObjectId) -> Result<BezPath> {
    let v = project.vector(doc)?;
    let (obj, parent) = locate(v, object)
        .ok_or_else(|| Error::Invalid(format!("no object '{object}' in document '{doc}'")))?;
    let p = object_path(v, obj)?;
    Ok(parent * p)
}

/// Like [`path_of`] but on a document you already hold.
pub fn path_in_doc(v: &VectorDoc, object: &ObjectId) -> Result<BezPath> {
    let (obj, parent) = locate(v, object)
        .ok_or_else(|| Error::Invalid(format!("no object '{object}' in document '{}'", v.id)))?;
    Ok(parent * object_path(v, obj)?)
}

/// The object and the product of all of its *ancestors'* transforms.
pub fn locate<'a>(v: &'a VectorDoc, id: &ObjectId) -> Option<(&'a VObject, Affine)> {
    fn rec<'a>(os: &'a [VObject], id: &ObjectId, acc: Affine) -> Option<(&'a VObject, Affine)> {
        for o in os {
            if &o.id == id {
                return Some((o, acc));
            }
            if let VKind::Group { objects } = &o.kind {
                if let Some(hit) = rec(objects, id, acc * o.transform.to_kurbo()) {
                    return Some(hit);
                }
            }
        }
        None
    }
    rec(&v.objects, id, Affine::IDENTITY)
}

/// The chain of ancestors of `id`, outermost first. Used by reparenting ops.
pub fn ancestors(v: &VectorDoc, id: &ObjectId) -> Vec<ObjectId> {
    fn rec(os: &[VObject], id: &ObjectId, trail: &mut Vec<ObjectId>) -> bool {
        for o in os {
            if &o.id == id {
                return true;
            }
            if let VKind::Group { objects } = &o.kind {
                trail.push(o.id.clone());
                if rec(objects, id, trail) {
                    return true;
                }
                trail.pop();
            }
        }
        false
    }
    let mut trail = Vec::new();
    rec(&v.objects, id, &mut trail);
    trail
}

/// Outline of one object in its parent's coordinate space: local geometry with the
/// object's own transform applied.
///
/// Text is the only kind whose geometry depends on a font, and the project's registered
/// faces live in the asset store — which this signature cannot reach. So the plain form
/// shapes with the embedded fallback, and the `_with` forms take the set a render already
/// built. A document's *appearance* has to honour `font.register`; a boolean or a
/// measurement against live text does not, and `vector.text.to-outlines` is how text
/// becomes geometry those ops should be trusted with.
pub fn object_path(v: &VectorDoc, obj: &VObject) -> Result<BezPath> {
    object_path_with(v, obj, text::fonts())
}

pub fn object_path_with(v: &VectorDoc, obj: &VObject, fonts: &text::Fonts) -> Result<BezPath> {
    Ok(obj.transform.to_kurbo() * local_path_with(v, obj, fonts)?)
}

/// Outline of one object in its own coordinate space, before its own transform.
pub fn local_path(v: &VectorDoc, obj: &VObject) -> Result<BezPath> {
    local_path_with(v, obj, text::fonts())
}

pub fn local_path_with(v: &VectorDoc, obj: &VObject, fonts: &text::Fonts) -> Result<BezPath> {
    match &obj.kind {
        VKind::Path { d } => parse_d(d),
        VKind::Rect { rect, radius } => {
            let r = rect.to_kurbo();
            if *radius > 0.0 {
                let max = (r.width().min(r.height()) / 2.0).max(0.0);
                Ok(RoundedRect::from_rect(r, radius.min(max)).to_path(1e-3))
            } else {
                Ok(r.to_path(1e-3))
            }
        }
        VKind::Ellipse { center, radius } => Ok(Ellipse::new(
            Point::new(center[0], center[1]),
            (radius[0], radius[1]),
            0.0,
        )
        .to_path(1e-3)),
        VKind::Polygon {
            center,
            radius,
            sides,
            rotation,
        } => {
            if *sides < 3 {
                return Err(Error::DegenerateGeometry(format!(
                    "polygon '{}' needs at least 3 sides, has {sides}",
                    obj.id
                )));
            }
            Ok(regular_polygon(
                *center, *radius, *radius, *sides, *rotation,
            ))
        }
        VKind::Star {
            center,
            outer,
            inner,
            points,
            rotation,
        } => {
            if *points < 3 {
                return Err(Error::DegenerateGeometry(format!(
                    "star '{}' needs at least 3 points, has {points}",
                    obj.id
                )));
            }
            Ok(regular_polygon(
                *center,
                *outer,
                *inner,
                points * 2,
                *rotation,
            ))
        }
        VKind::Line { from, to } => {
            let mut p = BezPath::new();
            p.move_to(Point::new(from[0], from[1]));
            p.line_to(Point::new(to[0], to[1]));
            Ok(p)
        }
        VKind::Image { rect, .. } => Ok(rect.to_kurbo().to_path(1e-3)),
        VKind::Group { objects } => {
            let mut out = BezPath::new();
            for child in objects {
                out.extend(object_path_with(v, child, fonts)?);
            }
            Ok(out)
        }
        VKind::Text {
            spec,
            origin,
            on_path,
        } => {
            // The set the caller resolved, so a registered face reaches the render.
            match on_path {
                Some(tp) => {
                    let target = path_in_doc(v, &tp.target)?;
                    let flat = flatten(&target, DEFAULT_TOLERANCE);
                    Ok(text::outline_on_path(
                        fonts,
                        spec,
                        &flat,
                        tp.offset,
                        tp.side == PathSide::Right,
                    )
                    .0)
                }
                None => Ok(text::outline_block(fonts, spec, (origin[0], origin[1])).0),
            }
        }
    }
}

/// Star / polygon generator. `rotation` is degrees clockwise from "first vertex straight up",
/// which is what both Inkscape and Illustrator show the user.
pub fn regular_polygon(
    center: [f64; 2],
    outer: f64,
    inner: f64,
    verts: u32,
    rotation: f64,
) -> BezPath {
    let mut p = BezPath::new();
    let base = -std::f64::consts::FRAC_PI_2 + rotation.to_radians();
    let step = std::f64::consts::TAU / verts as f64;
    for i in 0..verts {
        let r = if i % 2 == 0 { outer } else { inner };
        let a = base + step * i as f64;
        let pt = Point::new(center[0] + r * a.cos(), center[1] + r * a.sin());
        if i == 0 {
            p.move_to(pt);
        } else {
            p.line_to(pt);
        }
    }
    p.close_path();
    p
}

/// Parse SVG path data. `kurbo` handles the full grammar; we only reword the error.
pub fn parse_d(d: &str) -> Result<BezPath> {
    if d.trim().is_empty() {
        return Ok(BezPath::new());
    }
    BezPath::from_svg(d).map_err(|e| Error::DegenerateGeometry(format!("bad path data: {e}")))
}

/// Deterministic SVG path data: fixed precision so round-trips are byte-stable.
pub fn to_d(p: &BezPath) -> String {
    let mut s = String::new();
    let n = |v: f64| -> String {
        let r = (v * 1000.0).round() / 1000.0;
        let r = if r == 0.0 { 0.0 } else { r };
        let mut t = format!("{r}");
        if t.ends_with(".0") {
            t.truncate(t.len() - 2);
        }
        t
    };
    for el in p.elements() {
        if !s.is_empty() {
            s.push(' ');
        }
        match el {
            PathEl::MoveTo(p) => s.push_str(&format!("M {} {}", n(p.x), n(p.y))),
            PathEl::LineTo(p) => s.push_str(&format!("L {} {}", n(p.x), n(p.y))),
            PathEl::QuadTo(a, b) => {
                s.push_str(&format!("Q {} {} {} {}", n(a.x), n(a.y), n(b.x), n(b.y)))
            }
            PathEl::CurveTo(a, b, c) => s.push_str(&format!(
                "C {} {} {} {} {} {}",
                n(a.x),
                n(a.y),
                n(b.x),
                n(b.y),
                n(c.x),
                n(c.y)
            )),
            PathEl::ClosePath => s.push('Z'),
        }
    }
    s
}

/// Flatten to polylines: one `Vec<Point>` per subpath. Closed subpaths keep their first
/// point once; the caller treats them as implicitly closed.
pub fn flatten(p: &BezPath, tol: f64) -> Vec<Subpath> {
    let tol = tol.max(1e-6);
    let mut out: Vec<Subpath> = Vec::new();
    let mut cur: Option<Subpath> = None;
    dpaint_core::kurbo::flatten(p.iter(), tol, |el| match el {
        PathEl::MoveTo(pt) => {
            if let Some(sp) = cur.take() {
                if sp.points.len() > 1 {
                    out.push(sp);
                }
            }
            cur = Some(Subpath {
                points: vec![pt],
                closed: false,
            });
        }
        PathEl::LineTo(pt) => {
            if let Some(sp) = cur.as_mut() {
                if sp
                    .points
                    .last()
                    .map(|l| l.distance(pt) > 1e-12)
                    .unwrap_or(true)
                {
                    sp.points.push(pt);
                }
            }
        }
        PathEl::ClosePath => {
            if let Some(mut sp) = cur.take() {
                sp.closed = true;
                if sp.points.len() > 2 && sp.points[0].distance(*sp.points.last().unwrap()) < 1e-12
                {
                    sp.points.pop();
                }
                if sp.points.len() > 1 {
                    out.push(sp);
                }
            }
            cur = None;
        }
        _ => unreachable!("flatten only emits move/line/close"),
    });
    if let Some(sp) = cur.take() {
        if sp.points.len() > 1 {
            out.push(sp);
        }
    }
    out
}

#[derive(Debug, Clone, PartialEq)]
pub struct Subpath {
    pub points: Vec<Point>,
    pub closed: bool,
}

impl Subpath {
    pub fn length(&self) -> f64 {
        let mut l = 0.0;
        for w in self.points.windows(2) {
            l += w[0].distance(w[1]);
        }
        if self.closed && self.points.len() > 2 {
            l += self.points.last().unwrap().distance(self.points[0]);
        }
        l
    }

    /// Signed area, positive for a clockwise ring in SVG's y-down space.
    pub fn signed_area(&self) -> f64 {
        let n = self.points.len();
        if n < 3 {
            return 0.0;
        }
        let mut a = 0.0;
        for i in 0..n {
            let p = self.points[i];
            let q = self.points[(i + 1) % n];
            a += p.x * q.y - q.x * p.y;
        }
        a / 2.0
    }

    pub fn to_bez(&self) -> BezPath {
        let mut p = BezPath::new();
        if self.points.is_empty() {
            return p;
        }
        p.move_to(self.points[0]);
        for pt in &self.points[1..] {
            p.line_to(*pt);
        }
        if self.closed {
            p.close_path();
        }
        p
    }
}

pub fn subpaths_to_bez(sps: &[Subpath]) -> BezPath {
    let mut out = BezPath::new();
    for sp in sps {
        out.extend(sp.to_bez());
    }
    out
}

/// Split a path into its subpaths as `BezPath`s, preserving curvature.
pub fn split_subpaths(p: &BezPath) -> Vec<BezPath> {
    let mut out = Vec::new();
    let mut cur = BezPath::new();
    for el in p.elements() {
        if matches!(el, PathEl::MoveTo(_)) && !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
        cur.push(*el);
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Total arc length of every segment.
pub fn length(p: &BezPath) -> f64 {
    p.segments().map(|s| s.arclen(1e-6)).sum()
}

/// Signed-area magnitude of the closed regions, computed exactly from the Béziers.
pub fn area(p: &BezPath) -> f64 {
    p.area().abs()
}

pub fn bbox(p: &BezPath) -> Option<KRect> {
    if p.elements().is_empty() {
        return None;
    }
    let b = p.bounding_box();
    if b.width().is_nan() || b.height().is_nan() {
        None
    } else {
        Some(b)
    }
}

/// Point and unit tangent at normalized position `t` along the whole path by arc length.
pub fn sample(p: &BezPath, t: f64) -> Option<(Point, Vec2)> {
    let segs: Vec<PathSeg> = p.segments().collect();
    if segs.is_empty() {
        return None;
    }
    let lens: Vec<f64> = segs.iter().map(|s| s.arclen(1e-6)).collect();
    let total: f64 = lens.iter().sum();
    if total <= 0.0 {
        let s = segs[0];
        return Some((s.eval(0.0), Vec2::new(1.0, 0.0)));
    }
    let want = (t.clamp(0.0, 1.0)) * total;
    let mut acc = 0.0;
    for (s, l) in segs.iter().zip(&lens) {
        if acc + l >= want || (acc + l - want).abs() < 1e-12 {
            let local = if *l > 0.0 { (want - acc) / l } else { 0.0 };
            let u = s.inv_arclen(local * l, 1e-6);
            let d = match s {
                PathSeg::Line(x) => x.deriv().eval(u).to_vec2(),
                PathSeg::Quad(x) => x.deriv().eval(u).to_vec2(),
                PathSeg::Cubic(x) => x.deriv().eval(u).to_vec2(),
            };
            let d = if d.hypot() < 1e-12 {
                Vec2::new(1.0, 0.0)
            } else {
                d.normalize()
            };
            return Some((s.eval(u), d));
        }
        acc += l;
    }
    let s = *segs.last().unwrap();
    Some((s.eval(1.0), Vec2::new(1.0, 0.0)))
}

/// Every intersection between two paths, as document-space points, sorted and deduplicated.
pub fn intersections(a: &BezPath, b: &BezPath) -> Vec<Point> {
    let mut pts: Vec<Point> = Vec::new();
    for sa in a.segments() {
        for sb in b.segments() {
            for hit in sa.intersect_line_or_curve(&sb) {
                let p = sa.eval(hit);
                if !pts.iter().any(|q| q.distance(p) < 1e-6) {
                    pts.push(p);
                }
            }
        }
    }
    pts.sort_by(|p, q| {
        p.x.partial_cmp(&q.x)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(p.y.partial_cmp(&q.y).unwrap_or(std::cmp::Ordering::Equal))
    });
    pts
}

/// `kurbo` only ships a line/curve intersector, so curve/curve falls back to recursive
/// bounding-box subdivision — exact to 1e-7 and free of the false positives a flattened
/// intersector produces on tangential contact.
trait SegIntersect {
    fn intersect_line_or_curve(&self, other: &PathSeg) -> Vec<f64>;
}

impl SegIntersect for PathSeg {
    fn intersect_line_or_curve(&self, other: &PathSeg) -> Vec<f64> {
        if let PathSeg::Line(l) = other {
            return self
                .intersect_line(*l)
                .iter()
                .map(|h| h.segment_t)
                .collect();
        }
        let mut out = Vec::new();
        subdivide_intersect(self, (0.0, 1.0), other, (0.0, 1.0), 0, &mut out);
        out
    }
}

fn subdivide_intersect(
    a: &PathSeg,
    ar: (f64, f64),
    b: &PathSeg,
    br: (f64, f64),
    depth: u32,
    out: &mut Vec<f64>,
) {
    let sa = a.subsegment(ar.0..ar.1);
    let sb = b.subsegment(br.0..br.1);
    let (ba, bb) = (sa.bounding_box(), sb.bounding_box());
    if ba.x1 < bb.x0 - 1e-9 || bb.x1 < ba.x0 - 1e-9 || ba.y1 < bb.y0 - 1e-9 || bb.y1 < ba.y0 - 1e-9
    {
        return;
    }
    if depth >= 24 || (ba.width().max(ba.height()) < 1e-7 && bb.width().max(bb.height()) < 1e-7) {
        let t = (ar.0 + ar.1) / 2.0;
        if !out.iter().any(|u| (u - t).abs() < 1e-5) {
            out.push(t);
        }
        return;
    }
    let am = (ar.0 + ar.1) / 2.0;
    let bm = (br.0 + br.1) / 2.0;
    subdivide_intersect(a, (ar.0, am), b, (br.0, bm), depth + 1, out);
    subdivide_intersect(a, (ar.0, am), b, (bm, br.1), depth + 1, out);
    subdivide_intersect(a, (am, ar.1), b, (br.0, bm), depth + 1, out);
    subdivide_intersect(a, (am, ar.1), b, (bm, br.1), depth + 1, out);
}

/// Nearest distance from a point to a path outline.
pub fn distance_to(p: &BezPath, pt: Point) -> f64 {
    p.segments()
        .map(|s| s.nearest(pt, 1e-6).distance_sq.sqrt())
        .fold(f64::INFINITY, f64::min)
}

/// Convert a `kurbo` path to a `tiny_skia` path, applying `at`.
pub fn to_skia(p: &BezPath, at: Affine) -> Option<tiny_skia::Path> {
    let mut b = tiny_skia::PathBuilder::new();
    let mut open = false;
    for el in (at * p.clone()).elements() {
        match el {
            PathEl::MoveTo(p) => {
                b.move_to(p.x as f32, p.y as f32);
                open = true;
            }
            PathEl::LineTo(p) => {
                if open {
                    b.line_to(p.x as f32, p.y as f32)
                }
            }
            PathEl::QuadTo(a, c) => {
                if open {
                    b.quad_to(a.x as f32, a.y as f32, c.x as f32, c.y as f32)
                }
            }
            PathEl::CurveTo(a, c, d) => {
                if open {
                    b.cubic_to(
                        a.x as f32, a.y as f32, c.x as f32, c.y as f32, d.x as f32, d.y as f32,
                    )
                }
            }
            PathEl::ClosePath => {
                if open {
                    b.close()
                }
            }
        }
    }
    b.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use dpaint_core::doc::common::Rect;

    fn doc() -> VectorDoc {
        VectorDoc::new(DocId::from("doc_v"), "v", 100.0, 100.0)
    }

    #[test]
    fn a_rect_object_resolves_to_its_four_corners_in_document_space() {
        let mut d = doc();
        let mut o = VObject::new(
            ObjectId::from("obj_r"),
            "r",
            VKind::Rect {
                rect: Rect::new(0.0, 0.0, 10.0, 20.0),
                radius: 0.0,
            },
        );
        o.transform = dpaint_core::doc::common::Transform::translate(5.0, 7.0);
        d.objects.push(o);
        let p = path_in_doc(&d, &ObjectId::from("obj_r")).unwrap();
        let b = bbox(&p).unwrap();
        assert!((b.x0 - 5.0).abs() < 1e-9 && (b.y0 - 7.0).abs() < 1e-9);
        assert!((b.width() - 10.0).abs() < 1e-9 && (b.height() - 20.0).abs() < 1e-9);
        assert!((area(&p) - 200.0).abs() < 1e-6);
    }

    #[test]
    fn group_transforms_compose_onto_children() {
        let mut d = doc();
        let child = VObject::new(
            ObjectId::from("obj_c"),
            "c",
            VKind::Rect {
                rect: Rect::new(0.0, 0.0, 10.0, 10.0),
                radius: 0.0,
            },
        );
        let mut g = VObject::new(
            ObjectId::from("obj_g"),
            "g",
            VKind::Group {
                objects: vec![child],
            },
        );
        g.transform = dpaint_core::doc::common::Transform::scale(2.0, 3.0);
        d.objects.push(g);
        let p = path_in_doc(&d, &ObjectId::from("obj_c")).unwrap();
        let b = bbox(&p).unwrap();
        assert!(
            (b.width() - 20.0).abs() < 1e-9,
            "x scale applies, got {}",
            b.width()
        );
        assert!((b.height() - 30.0).abs() < 1e-9);
    }

    #[test]
    fn a_ring_keeps_its_hole_as_a_second_subpath() {
        let mut outer = KRect::new(0.0, 0.0, 10.0, 10.0).to_path(1e-3);
        outer.extend(KRect::new(3.0, 3.0, 7.0, 7.0).to_path(1e-3));
        assert_eq!(split_subpaths(&outer).len(), 2);
        let sps = flatten(&outer, 0.01);
        assert_eq!(sps.len(), 2);
        assert!(sps.iter().all(|s| s.closed));
    }

    #[test]
    fn sampling_walks_the_path_by_arc_length() {
        let mut p = BezPath::new();
        p.move_to(Point::new(0.0, 0.0));
        p.line_to(Point::new(10.0, 0.0));
        p.line_to(Point::new(10.0, 10.0));
        let (mid, tan) = sample(&p, 0.5).unwrap();
        assert!(
            (mid.x - 10.0).abs() < 1e-6 && mid.y.abs() < 1e-6,
            "midpoint at the corner: {mid:?}"
        );
        assert!(tan.x.abs() < 1e-6 || tan.y.abs() < 1e-6);
        let (end, _) = sample(&p, 1.0).unwrap();
        assert!((end.y - 10.0).abs() < 1e-6);
    }

    #[test]
    fn two_crossing_rectangles_report_their_four_crossings() {
        let a = KRect::new(0.0, 0.0, 10.0, 4.0).to_path(1e-3);
        let b = KRect::new(3.0, -5.0, 6.0, 9.0).to_path(1e-3);
        let hits = intersections(&a, &b);
        assert_eq!(hits.len(), 4, "got {hits:?}");
    }

    #[test]
    fn path_data_round_trips_through_text() {
        let p = Ellipse::new(Point::new(5.0, 5.0), (4.0, 3.0), 0.0).to_path(1e-4);
        let back = parse_d(&to_d(&p)).unwrap();
        assert!((area(&back) - area(&p)).abs() < 0.01);
    }
}
