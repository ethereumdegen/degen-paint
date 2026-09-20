//! Path surgery: stroking to outline, offsetting, simplifying, corner rounding and
//! node-level editing.
//!
//! The stroker builds the outline as a union of per-segment quads, join wedges and cap
//! shapes, then resolves it with the boolean engine. That is slower than a marching
//! stroker but it is unconditionally correct on self-intersecting input, which is exactly
//! where hand-rolled strokers produce garbage.

use crate::boolean;
use crate::geom::{flatten, Subpath};
use dpaint_core::doc::common::{FillRule, LineCap, LineJoin};
use dpaint_core::error::{Error, Result};
use dpaint_core::kurbo::{BezPath, CubicBez, ParamCurve, PathEl, PathSeg, Point, Vec2};

// These are internal helpers whose parameters are genuinely independent; bundling them
// into a struct at a couple of call sites would add indirection, not clarity.
#[allow(clippy::too_many_arguments)]
/// Convert a stroke into a fillable outline.
pub fn outline_stroke(
    path: &BezPath,
    width: f64,
    cap: LineCap,
    join: LineJoin,
    miter_limit: f64,
    dash: &[f64],
    dash_offset: f64,
    tol: f64,
) -> Result<BezPath> {
    // `!(a < b)` is deliberate: it is true when the values are incomparable, which is the
    // branch degenerate geometry needs. Rewriting it as `a >= b` would silently drop NaN.
    #[allow(clippy::neg_cmp_op_on_partial_ord)]
    if !(width > 0.0) {
        return Err(Error::DegenerateGeometry(
            "stroke width must be greater than zero".into(),
        ));
    }
    let h = width / 2.0;
    let mut sps = flatten(path, tol);
    if !dash.is_empty() && dash.iter().any(|d| *d > 0.0) {
        sps = apply_dash(&sps, dash, dash_offset);
    }
    let mut rings: Vec<Vec<[f64; 2]>> = Vec::new();
    let circle_steps = arc_steps(h, tol);
    for sp in &sps {
        let mut pts = sp.points.clone();
        if sp.closed && pts.len() > 2 {
            pts.push(pts[0]);
        }
        if pts.len() < 2 {
            // A lone point still paints a dot under a round cap.
            if cap == LineCap::Round && !pts.is_empty() {
                rings.push(ring(&disc(pts[0], h, circle_steps)));
            }
            continue;
        }
        for w in pts.windows(2) {
            let (a, b) = (w[0], w[1]);
            let d = b - a;
            if d.hypot() < 1e-12 {
                continue;
            }
            let n = Vec2::new(-d.y, d.x).normalize() * h;
            rings.push(ring(&[a + n, b + n, b - n, a - n]));
        }
        // Joins at every interior vertex (and at the seam of a closed subpath).
        let last = pts.len() - 1;
        let joints: Vec<usize> = if sp.closed {
            (0..=last).collect()
        } else {
            (1..last).collect()
        };
        for i in joints {
            let prev = if i == 0 { pts[last - 1] } else { pts[i - 1] };
            let next = if i == last { pts[1] } else { pts[i + 1] };
            let v = pts[i];
            if let Some(r) = join_ring(prev, v, next, h, join, miter_limit, circle_steps) {
                rings.push(ring(&r));
            }
        }
        if !sp.closed {
            let (a, b) = (pts[0], pts[1]);
            let (y, z) = (pts[last - 1], pts[last]);
            match cap {
                LineCap::Butt => {}
                LineCap::Round => {
                    rings.push(ring(&disc(a, h, circle_steps)));
                    rings.push(ring(&disc(z, h, circle_steps)));
                }
                LineCap::Square => {
                    rings.push(ring(&square_cap(a, (a - b).normalize(), h)));
                    rings.push(ring(&square_cap(z, (z - y).normalize(), h)));
                }
            }
        }
    }
    if rings.is_empty() {
        return Err(Error::DegenerateGeometry(
            "nothing to stroke: the path has no length".into(),
        ));
    }
    for r in rings.iter_mut() {
        orient_ccw(r);
    }
    Ok(boolean::union_contours(vec![rings], tol))
}

fn ring(pts: &[Point]) -> Vec<[f64; 2]> {
    pts.iter().map(|p| [p.x, p.y]).collect()
}

/// The boolean union is winding-sensitive, so every contributed ring gets the same
/// orientation before it goes in.
fn orient_ccw(r: &mut Vec<[f64; 2]>) {
    let n = r.len();
    let mut a = 0.0;
    for i in 0..n {
        let p = r[i];
        let q = r[(i + 1) % n];
        a += p[0] * q[1] - q[0] * p[1];
    }
    if a < 0.0 {
        r.reverse();
    }
}

fn arc_steps(radius: f64, tol: f64) -> usize {
    let tol = tol.clamp(1e-4, radius.max(1e-4));
    let steps = (std::f64::consts::PI / (1.0 - tol / radius.max(1e-9)).clamp(-1.0, 1.0).acos())
        .ceil()
        .max(8.0);
    (steps as usize).clamp(8, 256)
}

fn disc(c: Point, r: f64, steps: usize) -> Vec<Point> {
    (0..steps)
        .map(|i| {
            let a = std::f64::consts::TAU * i as f64 / steps as f64;
            Point::new(c.x + r * a.cos(), c.y + r * a.sin())
        })
        .collect()
}

fn square_cap(end: Point, outward: Vec2, h: f64) -> Vec<Point> {
    let n = Vec2::new(-outward.y, outward.x) * h;
    let e = outward * h;
    vec![end + n, end + n + e, end - n + e, end - n]
}

fn join_ring(
    prev: Point,
    v: Point,
    next: Point,
    h: f64,
    join: LineJoin,
    miter_limit: f64,
    steps: usize,
) -> Option<Vec<Point>> {
    let d1 = v - prev;
    let d2 = next - v;
    if d1.hypot() < 1e-12 || d2.hypot() < 1e-12 {
        return None;
    }
    let (d1, d2) = (d1.normalize(), d2.normalize());
    let cross = d1.x * d2.y - d1.y * d2.x;
    if cross.abs() < 1e-12 {
        return None; // collinear: the two quads already meet flush
    }
    let s = if cross > 0.0 { -1.0 } else { 1.0 };
    let n1 = Vec2::new(-d1.y, d1.x) * (h * s);
    let n2 = Vec2::new(-d2.y, d2.x) * (h * s);
    let (p1, p2) = (v + n1, v + n2);
    match join {
        LineJoin::Round => Some(disc(v, h, steps)),
        LineJoin::Bevel => Some(vec![v, p1, p2]),
        LineJoin::Miter => {
            let dot = (n1.dot(n2)) / (h * h);
            let cos_half = ((1.0 + dot.clamp(-1.0, 1.0)) / 2.0).sqrt();
            if cos_half < 1e-9 {
                return Some(vec![v, p1, p2]);
            }
            let ratio = 1.0 / cos_half;
            if ratio > miter_limit.max(1.0) {
                return Some(vec![v, p1, p2]);
            }
            let bis = (n1 + n2).normalize() * (h * ratio);
            Some(vec![v, p1, v + bis, p2])
        }
    }
}

/// Split flattened subpaths into dash runs.
pub fn apply_dash(sps: &[Subpath], dash: &[f64], offset: f64) -> Vec<Subpath> {
    let pattern: Vec<f64> = if dash.len() % 2 == 1 {
        dash.iter().chain(dash.iter()).copied().collect()
    } else {
        dash.to_vec()
    };
    // A zero-length entry contributes nothing and would stall the walk below.
    if pattern.iter().any(|d| *d <= 0.0) {
        return sps.to_vec();
    }
    let cycle: f64 = pattern.iter().sum();
    if cycle <= 0.0 {
        return sps.to_vec();
    }
    let mut out = Vec::new();
    for sp in sps {
        let mut pts = sp.points.clone();
        if sp.closed && pts.len() > 1 {
            pts.push(pts[0]);
        }
        // Where in the pattern do we start?
        let mut idx = 0usize;
        let mut rem = {
            let mut o = offset.rem_euclid(cycle);
            loop {
                if o < pattern[idx] {
                    break pattern[idx] - o;
                }
                o -= pattern[idx];
                idx = (idx + 1) % pattern.len();
            }
        };
        let mut on = idx % 2 == 0;
        let mut cur: Vec<Point> = if on { vec![pts[0]] } else { Vec::new() };
        for w in pts.windows(2) {
            let (a, b) = (w[0], w[1]);
            let seg = a.distance(b);
            if seg <= 0.0 {
                continue;
            }
            let mut t = 0.0;
            while seg - t > rem {
                t += rem;
                let p = a.lerp(b, t / seg);
                if on {
                    cur.push(p);
                    if cur.len() > 1 {
                        out.push(Subpath {
                            points: std::mem::take(&mut cur),
                            closed: false,
                        });
                    } else {
                        cur.clear();
                    }
                } else {
                    cur = vec![p];
                }
                on = !on;
                idx = (idx + 1) % pattern.len();
                rem = pattern[idx];
            }
            rem -= seg - t;
            if on {
                cur.push(b);
            }
        }
        if cur.len() > 1 {
            out.push(Subpath {
                points: cur,
                closed: false,
            });
        }
    }
    out
}

/// Grow (`d > 0`) or shrink (`d < 0`) the filled region of a path by `d`.
///
/// Closed regions go through the boolean engine so holes and self-intersections stay
/// correct; open subpaths are displaced along their normal, which is what an agent asking
/// to offset a stroke centreline expects.
pub fn offset(path: &BezPath, d: f64, rule: FillRule, tol: f64) -> Result<BezPath> {
    if d == 0.0 {
        return Ok(path.clone());
    }
    let sps = flatten(path, tol);
    if sps.is_empty() {
        return Err(Error::DegenerateGeometry("cannot offset an empty path".into()));
    }
    let closed: Vec<Subpath> = sps.iter().filter(|s| s.closed).cloned().collect();
    let open: Vec<Subpath> = sps.iter().filter(|s| !s.closed).cloned().collect();
    let mut out = BezPath::new();
    if !closed.is_empty() {
        let base = crate::geom::subpaths_to_bez(&closed);
        let band = outline_stroke(
            &base,
            2.0 * d.abs(),
            LineCap::Butt,
            LineJoin::Round,
            4.0,
            &[],
            0.0,
            tol,
        )?;
        let op = if d > 0.0 {
            boolean::BoolOp::Union
        } else {
            boolean::BoolOp::Subtract
        };
        out.extend(boolean::boolean(&[base, band], op, rule, tol)?);
    }
    for sp in &open {
        let mut pts = Vec::with_capacity(sp.points.len());
        for i in 0..sp.points.len() {
            let a = sp.points[i.saturating_sub(1)];
            let b = sp.points[(i + 1).min(sp.points.len() - 1)];
            let dir = b - a;
            let dir = if dir.hypot() < 1e-12 {
                Vec2::new(1.0, 0.0)
            } else {
                dir.normalize()
            };
            let n = Vec2::new(-dir.y, dir.x) * d;
            pts.push(sp.points[i] + n);
        }
        out.extend(
            Subpath {
                points: pts,
                closed: false,
            }
            .to_bez(),
        );
    }
    if out.is_empty() {
        return Err(Error::DegenerateGeometry(format!(
            "offsetting by {d} collapsed the path to nothing"
        )));
    }
    Ok(out)
}

/// Douglas–Peucker on the flattened form, then refit cubics through the surviving points.
/// The result is guaranteed to stay within `tol` of the flattened original: the refit is
/// measured and the decimation is tightened until it does.
pub fn simplify(path: &BezPath, tol: f64) -> BezPath {
    let tol = tol.max(1e-9);
    let sps = flatten(path, (tol / 10.0).clamp(1e-4, 0.5));
    let mut out = BezPath::new();
    for sp in &sps {
        let mut eps = tol;
        // Decimate, refit, measure; tighten until the refit is inside the tolerance. If
        // six halvings still cannot meet it the original polyline is kept, so the
        // guarantee "within `tol` of the input" always holds.
        let mut best = sp.to_bez();
        for _ in 0..6 {
            let keep = douglas_peucker(&sp.points, eps, sp.closed);
            let fit = fit_cubics(&keep, sp.closed);
            if max_deviation(&fit, &sp.points) <= tol {
                best = fit;
                break;
            }
            eps *= 0.4;
        }
        out.extend(best);
    }
    out
}

/// Classic Douglas–Peucker. On a closed ring the two extreme points anchor the recursion.
pub fn douglas_peucker(points: &[Point], eps: f64, closed: bool) -> Vec<Point> {
    if points.len() < 3 {
        return points.to_vec();
    }
    if closed {
        // Anchor on the two points furthest apart so the ring is not biased by its seam.
        let (mut i1, mut best) = (0usize, -1.0);
        for i in 0..points.len() {
            let d = points[0].distance(points[i]);
            if d > best {
                best = d;
                i1 = i;
            }
        }
        let a = &points[0..=i1];
        let mut b: Vec<Point> = points[i1..].to_vec();
        b.push(points[0]);
        let mut out = dp(a, eps);
        out.pop();
        let tail = dp(&b, eps);
        out.extend(tail[..tail.len() - 1].iter().copied());
        out
    } else {
        dp(points, eps)
    }
}

fn dp(points: &[Point], eps: f64) -> Vec<Point> {
    if points.len() < 3 {
        return points.to_vec();
    }
    let (a, b) = (points[0], *points.last().unwrap());
    let mut worst = 0.0;
    let mut idx = 0;
    for (i, p) in points.iter().enumerate().take(points.len() - 1).skip(1) {
        let d = point_line_distance(*p, a, b);
        if d > worst {
            worst = d;
            idx = i;
        }
    }
    if worst <= eps || idx == 0 {
        return vec![a, b];
    }
    let mut left = dp(&points[..=idx], eps);
    let right = dp(&points[idx..], eps);
    left.pop();
    left.extend(right);
    left
}

fn point_line_distance(p: Point, a: Point, b: Point) -> f64 {
    let ab = b - a;
    let len = ab.hypot();
    if len < 1e-12 {
        return p.distance(a);
    }
    let t = ((p - a).dot(ab) / (len * len)).clamp(0.0, 1.0);
    p.distance(a + ab * t)
}

/// Catmull–Rom through the retained points, converted to cubic Béziers.
fn fit_cubics(pts: &[Point], closed: bool) -> BezPath {
    let mut p = BezPath::new();
    if pts.len() < 2 {
        return p;
    }
    let n = pts.len();
    p.move_to(pts[0]);
    let idx = |i: isize| -> Point {
        if closed {
            pts[((i % n as isize) + n as isize) as usize % n]
        } else {
            pts[i.clamp(0, n as isize - 1) as usize]
        }
    };
    let segments = if closed { n } else { n - 1 };
    for i in 0..segments {
        let p0 = idx(i as isize - 1);
        let p1 = idx(i as isize);
        let p2 = idx(i as isize + 1);
        let p3 = idx(i as isize + 2);
        let c1 = p1 + (p2 - p0) / 6.0;
        let c2 = p2 - (p3 - p1) / 6.0;
        p.curve_to(c1, c2, p2);
    }
    if closed {
        p.close_path();
    }
    p
}

/// Largest distance from the reference polyline's vertices to the candidate path.
fn max_deviation(candidate: &BezPath, reference: &[Point]) -> f64 {
    if candidate.elements().is_empty() {
        return f64::INFINITY;
    }
    reference
        .iter()
        .map(|p| crate::geom::distance_to(candidate, *p))
        .fold(0.0, f64::max)
}

/// Reverse every subpath's direction, which flips the winding for nonzero fills.
pub fn reverse(path: &BezPath) -> BezPath {
    let mut out = BezPath::new();
    for sub in crate::geom::split_subpaths(path) {
        let closed = sub.elements().iter().any(|e| matches!(e, PathEl::ClosePath));
        let segs: Vec<PathSeg> = sub.segments().collect();
        if segs.is_empty() {
            out.extend(sub);
            continue;
        }
        out.move_to(segs.last().unwrap().end());
        for seg in segs.iter().rev() {
            match seg.reverse() {
                PathSeg::Line(l) => out.line_to(l.p1),
                PathSeg::Quad(q) => out.quad_to(q.p1, q.p2),
                PathSeg::Cubic(c) => out.curve_to(c.p1, c.p2, c.p3),
            }
        }
        if closed {
            out.close_path();
        }
    }
    out
}

/// Close every open subpath.
pub fn close_all(path: &BezPath) -> BezPath {
    let mut out = BezPath::new();
    for sub in crate::geom::split_subpaths(path) {
        let already = sub.elements().iter().any(|e| matches!(e, PathEl::ClosePath));
        out.extend(sub.clone());
        if !already && sub.segments().next().is_some() {
            out.close_path();
        }
    }
    out
}

/// Concatenate paths, optionally joining the end of each to the start of the next with a
/// straight segment instead of starting a new subpath.
pub fn append(paths: &[BezPath], connect: bool) -> BezPath {
    let mut out = BezPath::new();
    for p in paths {
        if out.is_empty() || !connect {
            out.extend(p.clone());
            continue;
        }
        for (i, el) in p.elements().iter().enumerate() {
            match (i, el) {
                (0, PathEl::MoveTo(pt)) => out.line_to(*pt),
                _ => out.push(*el),
            }
        }
    }
    out
}

/// Replace sharp corners between straight segments with circular arcs of radius `r`.
pub fn round_corners(path: &BezPath, r: f64) -> Result<BezPath> {
    // `!(a < b)` is deliberate: it is true when the values are incomparable, which is the
    // branch degenerate geometry needs. Rewriting it as `a >= b` would silently drop NaN.
    #[allow(clippy::neg_cmp_op_on_partial_ord)]
    if !(r > 0.0) {
        return Err(Error::DegenerateGeometry(
            "corner radius must be greater than zero".into(),
        ));
    }
    let mut out = BezPath::new();
    for sub in crate::geom::split_subpaths(path) {
        let closed = sub.elements().iter().any(|e| matches!(e, PathEl::ClosePath));
        let segs: Vec<PathSeg> = sub.segments().collect();
        if segs.len() < 2 {
            out.extend(sub);
            continue;
        }
        let mut pieces: Vec<PathSeg> = segs.clone();
        let count = if closed { pieces.len() } else { pieces.len() - 1 };
        // Trim each pair of adjacent straight segments and remember the arc to insert.
        let mut arcs: Vec<Option<CubicBez>> = vec![None; pieces.len()];
        for i in 0..count {
            let j = (i + 1) % pieces.len();
            let (PathSeg::Line(a), PathSeg::Line(b)) = (pieces[i], pieces[j]) else {
                continue;
            };
            let v = a.p1;
            let d1 = a.p0 - v;
            let d2 = b.p1 - v;
            if d1.hypot() < 1e-9 || d2.hypot() < 1e-9 {
                continue;
            }
            let (u1, u2) = (d1.normalize(), d2.normalize());
            let cosang = u1.dot(u2).clamp(-1.0, 1.0);
            let ang = cosang.acos();
            if ang < 1e-6 || (std::f64::consts::PI - ang).abs() < 1e-6 {
                continue;
            }
            let max_cut = (d1.hypot() / 2.0).min(d2.hypot() / 2.0);
            let cut = (r / (ang / 2.0).tan()).min(max_cut);
            if cut <= 1e-9 {
                continue;
            }
            let s1 = v + u1 * cut;
            let s2 = v + u2 * cut;
            // A single cubic approximates the fillet to well under a tenth of a pixel.
            let k = 4.0 / 3.0 * ((std::f64::consts::PI - ang) / 4.0).tan();
            let c1 = s1 - u1 * (cut * k);
            let c2 = s2 - u2 * (cut * k);
            pieces[i] = PathSeg::Line(dpaint_core::kurbo::Line::new(a.p0, s1));
            pieces[j] = PathSeg::Line(dpaint_core::kurbo::Line::new(s2, b.p1));
            arcs[i] = Some(CubicBez::new(s1, c1, c2, s2));
        }
        out.move_to(pieces[0].start());
        for (i, seg) in pieces.iter().enumerate() {
            match seg {
                PathSeg::Line(l) => out.line_to(l.p1),
                PathSeg::Quad(q) => out.quad_to(q.p1, q.p2),
                PathSeg::Cubic(c) => out.curve_to(c.p1, c.p2, c.p3),
            }
            if let Some(a) = arcs[i] {
                out.curve_to(a.p1, a.p2, a.p3);
            }
        }
        if closed {
            out.close_path();
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------------
// Node-level editing
// ---------------------------------------------------------------------------------------

/// One on-curve point with its two handles. This is the model a node editor manipulates;
/// it round-trips losslessly through `BezPath` (quadratics are elevated to cubics).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Node {
    pub pt: Point,
    pub in_ctrl: Option<Point>,
    pub out_ctrl: Option<Point>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NodeSub {
    pub nodes: Vec<Node>,
    pub closed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum NodeType {
    /// Handles are independent.
    Corner,
    /// Handles stay collinear, lengths independent.
    Smooth,
    /// Handles stay collinear and equal in length.
    Symmetric,
}

pub fn to_nodes(path: &BezPath) -> Vec<NodeSub> {
    let mut out = Vec::new();
    for sub in crate::geom::split_subpaths(path) {
        let mut nodes: Vec<Node> = Vec::new();
        let mut closed = false;
        for el in sub.elements() {
            match *el {
                PathEl::MoveTo(p) => nodes.push(Node {
                    pt: p,
                    in_ctrl: None,
                    out_ctrl: None,
                }),
                PathEl::LineTo(p) => nodes.push(Node {
                    pt: p,
                    in_ctrl: None,
                    out_ctrl: None,
                }),
                PathEl::QuadTo(c, p) => {
                    let start = nodes.last().map(|n| n.pt).unwrap_or(c);
                    let c1 = start + (c - start) * (2.0 / 3.0);
                    let c2 = p + (c - p) * (2.0 / 3.0);
                    if let Some(l) = nodes.last_mut() {
                        l.out_ctrl = Some(c1);
                    }
                    nodes.push(Node {
                        pt: p,
                        in_ctrl: Some(c2),
                        out_ctrl: None,
                    });
                }
                PathEl::CurveTo(c1, c2, p) => {
                    if let Some(l) = nodes.last_mut() {
                        l.out_ctrl = Some(c1);
                    }
                    nodes.push(Node {
                        pt: p,
                        in_ctrl: Some(c2),
                        out_ctrl: None,
                    });
                }
                PathEl::ClosePath => closed = true,
            }
        }
        if closed && nodes.len() > 1 {
            let last = *nodes.last().unwrap();
            if last.pt.distance(nodes[0].pt) < 1e-9 {
                nodes[0].in_ctrl = last.in_ctrl;
                nodes.pop();
            }
        }
        if !nodes.is_empty() {
            out.push(NodeSub { nodes, closed });
        }
    }
    out
}

pub fn from_nodes(subs: &[NodeSub]) -> BezPath {
    let mut p = BezPath::new();
    for sub in subs {
        if sub.nodes.is_empty() {
            continue;
        }
        p.move_to(sub.nodes[0].pt);
        let n = sub.nodes.len();
        let count = if sub.closed { n } else { n.saturating_sub(1) };
        for i in 0..count {
            let a = sub.nodes[i];
            let b = sub.nodes[(i + 1) % n];
            match (a.out_ctrl, b.in_ctrl) {
                (None, None) => p.line_to(b.pt),
                (c1, c2) => p.curve_to(c1.unwrap_or(a.pt), c2.unwrap_or(b.pt), b.pt),
            }
        }
        if sub.closed {
            p.close_path();
        }
    }
    p
}

fn sub_mut<'a>(subs: &'a mut [NodeSub], i: usize) -> Result<&'a mut NodeSub> {
    let len = subs.len();
    subs.get_mut(i).ok_or_else(|| {
        Error::Invalid(format!("subpath {i} is out of range; the path has {len}"))
    })
}

/// Insert a node partway along the segment that starts at node `index`.
pub fn node_insert(path: &BezPath, subpath: usize, index: usize, t: f64) -> Result<BezPath> {
    let mut subs = to_nodes(path);
    let s = sub_mut(&mut subs, subpath)?;
    let n = s.nodes.len();
    if n < 2 {
        return Err(Error::DegenerateGeometry(
            "a subpath needs two nodes before one can be inserted".into(),
        ));
    }
    if index >= n || (!s.closed && index + 1 >= n) {
        return Err(Error::Invalid(format!(
            "node {index} has no following segment in subpath {subpath}"
        )));
    }
    let t = t.clamp(1e-6, 1.0 - 1e-6);
    let j = (index + 1) % n;
    let (a, b) = (s.nodes[index], s.nodes[j]);
    let c = CubicBez::new(
        a.pt,
        a.out_ctrl.unwrap_or(a.pt),
        b.in_ctrl.unwrap_or(b.pt),
        b.pt,
    );
    let (left, right) = (c.subsegment(0.0..t), c.subsegment(t..1.0));
    let straight = a.out_ctrl.is_none() && b.in_ctrl.is_none();
    s.nodes[index].out_ctrl = if straight { None } else { Some(left.p1) };
    s.nodes[j].in_ctrl = if straight { None } else { Some(right.p2) };
    let mid = Node {
        pt: left.p3,
        in_ctrl: if straight { None } else { Some(left.p2) },
        out_ctrl: if straight { None } else { Some(right.p1) },
    };
    s.nodes.insert(index + 1, mid);
    Ok(from_nodes(&subs))
}

/// Remove a node; its neighbours join directly.
pub fn node_remove(path: &BezPath, subpath: usize, index: usize) -> Result<BezPath> {
    let mut subs = to_nodes(path);
    let s = sub_mut(&mut subs, subpath)?;
    if index >= s.nodes.len() {
        return Err(Error::Invalid(format!(
            "node {index} is out of range; subpath {subpath} has {}",
            s.nodes.len()
        )));
    }
    let min = if s.closed { 4 } else { 3 };
    if s.nodes.len() < min {
        return Err(Error::DegenerateGeometry(format!(
            "removing node {index} would leave subpath {subpath} degenerate"
        )));
    }
    s.nodes.remove(index);
    Ok(from_nodes(&subs))
}

/// Move a node and drag its handles with it.
pub fn node_move(path: &BezPath, subpath: usize, index: usize, to: Point, relative: bool) -> Result<BezPath> {
    let mut subs = to_nodes(path);
    let s = sub_mut(&mut subs, subpath)?;
    let node = s
        .nodes
        .get_mut(index)
        .ok_or_else(|| Error::Invalid(format!("node {index} is out of range in subpath {subpath}")))?;
    let delta = if relative {
        to.to_vec2()
    } else {
        to - node.pt
    };
    node.pt += delta;
    if let Some(c) = node.in_ctrl.as_mut() {
        *c += delta;
    }
    if let Some(c) = node.out_ctrl.as_mut() {
        *c += delta;
    }
    Ok(from_nodes(&subs))
}

/// Reshape a node's handles into a corner, a smooth tangent, or a symmetric tangent.
pub fn node_set_type(path: &BezPath, subpath: usize, index: usize, kind: NodeType) -> Result<BezPath> {
    let mut subs = to_nodes(path);
    let s = sub_mut(&mut subs, subpath)?;
    let n = s.nodes.len();
    if index >= n {
        return Err(Error::Invalid(format!(
            "node {index} is out of range in subpath {subpath}"
        )));
    }
    let node = s.nodes[index];
    let prev = if index == 0 {
        if s.closed { Some(s.nodes[n - 1]) } else { None }
    } else {
        Some(s.nodes[index - 1])
    };
    let next = if index + 1 < n {
        Some(s.nodes[index + 1])
    } else if s.closed {
        Some(s.nodes[0])
    } else {
        None
    };
    let updated = match kind {
        NodeType::Corner => Node {
            in_ctrl: None,
            out_ctrl: None,
            ..node
        },
        NodeType::Smooth | NodeType::Symmetric => {
            let a = prev.map(|p| p.pt).unwrap_or(node.pt);
            let b = next.map(|p| p.pt).unwrap_or(node.pt);
            let dir = b - a;
            let dir = if dir.hypot() < 1e-12 {
                Vec2::new(1.0, 0.0)
            } else {
                dir.normalize()
            };
            let lin = node
                .in_ctrl
                .map(|c| (c - node.pt).hypot())
                .unwrap_or_else(|| node.pt.distance(a) / 3.0);
            let lout = node
                .out_ctrl
                .map(|c| (c - node.pt).hypot())
                .unwrap_or_else(|| node.pt.distance(b) / 3.0);
            let (lin, lout) = if kind == NodeType::Symmetric {
                let m = (lin + lout) / 2.0;
                (m, m)
            } else {
                (lin, lout)
            };
            Node {
                pt: node.pt,
                in_ctrl: Some(node.pt - dir * lin),
                out_ctrl: Some(node.pt + dir * lout),
            }
        }
    };
    s.nodes[index] = updated;
    // A handle only exists as part of a segment; propagate to the neighbours so the
    // rebuilt path actually uses it.
    Ok(from_nodes(&subs))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::{area, bbox};
    use dpaint_core::kurbo::{Circle, Rect, Shape};

    #[test]
    fn outlining_a_line_gives_a_closed_band_of_length_times_width() {
        let mut p = BezPath::new();
        p.move_to((0.0, 0.0));
        p.line_to((100.0, 0.0));
        let o = outline_stroke(&p, 10.0, LineCap::Butt, LineJoin::Miter, 4.0, &[], 0.0, 0.01).unwrap();
        assert!(
            o.elements().iter().any(|e| matches!(e, PathEl::ClosePath)),
            "the outline is a closed region"
        );
        assert!((area(&o) - 1000.0).abs() < 0.5, "area {} ~ 100x10", area(&o));
    }

    #[test]
    fn round_caps_add_a_half_disc_at_each_end() {
        let mut p = BezPath::new();
        p.move_to((0.0, 0.0));
        p.line_to((100.0, 0.0));
        let butt = outline_stroke(&p, 10.0, LineCap::Butt, LineJoin::Miter, 4.0, &[], 0.0, 0.005).unwrap();
        let round = outline_stroke(&p, 10.0, LineCap::Round, LineJoin::Miter, 4.0, &[], 0.0, 0.005).unwrap();
        let extra = area(&round) - area(&butt);
        let disc = std::f64::consts::PI * 25.0;
        assert!((extra - disc).abs() < 1.0, "two half discs = {disc}, got {extra}");
    }

    #[test]
    fn dashing_a_stroke_removes_the_gaps_from_the_outline() {
        let mut p = BezPath::new();
        p.move_to((0.0, 0.0));
        p.line_to((100.0, 0.0));
        let solid = outline_stroke(&p, 4.0, LineCap::Butt, LineJoin::Miter, 4.0, &[], 0.0, 0.01).unwrap();
        let dashed =
            outline_stroke(&p, 4.0, LineCap::Butt, LineJoin::Miter, 4.0, &[10.0, 10.0], 0.0, 0.01).unwrap();
        assert!((area(&solid) - 400.0).abs() < 0.5);
        assert!((area(&dashed) - 200.0).abs() < 1.0, "half the ink: {}", area(&dashed));
    }

    #[test]
    fn offsetting_outward_grows_the_bounding_box_on_every_side() {
        let sq = Rect::new(10.0, 20.0, 60.0, 90.0).to_path(1e-4);
        let o = offset(&sq, 5.0, FillRule::Nonzero, 0.01).unwrap();
        let b = bbox(&o).unwrap();
        assert!((b.x0 - 5.0).abs() < 0.05, "left {}", b.x0);
        assert!((b.y0 - 15.0).abs() < 0.05, "top {}", b.y0);
        assert!((b.x1 - 65.0).abs() < 0.05, "right {}", b.x1);
        assert!((b.y1 - 95.0).abs() < 0.05, "bottom {}", b.y1);
    }

    #[test]
    fn offsetting_inward_shrinks_the_area() {
        let sq = Rect::new(0.0, 0.0, 50.0, 50.0).to_path(1e-4);
        let o = offset(&sq, -5.0, FillRule::Nonzero, 0.01).unwrap();
        assert!((area(&o) - 1600.0).abs() < 2.0, "40x40 remains, got {}", area(&o));
    }

    #[test]
    fn simplify_drops_segments_and_stays_inside_the_tolerance() {
        let circle = Circle::new((0.0, 0.0), 100.0).to_path(1e-6);
        let dense = crate::geom::subpaths_to_bez(&flatten(&circle, 0.001));
        let before = dense.segments().count();
        let s = simplify(&dense, 1.0);
        let after = s.segments().count();
        assert!(after < before / 4, "{after} segments from {before}");
        let dev = flatten(&dense, 0.05)
            .iter()
            .flat_map(|sp| sp.points.clone())
            .map(|p| crate::geom::distance_to(&s, p))
            .fold(0.0, f64::max);
        assert!(dev <= 1.0, "max deviation {dev} within tolerance 1.0");
    }

    #[test]
    fn reversing_a_path_swaps_its_endpoints_and_preserves_area() {
        let c = Circle::new((0.0, 0.0), 10.0).to_path(1e-6);
        let r = reverse(&c);
        assert!((area(&r) - area(&c)).abs() < 1e-6);
        let first = c.segments().next().unwrap().start();
        let rev_end = r.segments().last().unwrap().end();
        assert!(first.distance(rev_end) < 1e-9, "the reversed path ends where the original began");
    }

    #[test]
    fn rounding_corners_shortens_the_perimeter_and_keeps_the_area_close() {
        let sq = Rect::new(0.0, 0.0, 100.0, 100.0).to_path(1e-6);
        let r = round_corners(&sq, 20.0).unwrap();
        let a = area(&r);
        assert!(a < 10_000.0 && a > 9_500.0, "corners cut a little area: {a}");
        assert!(crate::geom::length(&r) < crate::geom::length(&sq));
    }

    #[test]
    fn inserting_a_node_adds_a_point_without_moving_the_curve() {
        let c = Circle::new((0.0, 0.0), 10.0).to_path(1e-6);
        let before = to_nodes(&c)[0].nodes.len();
        let out = node_insert(&c, 0, 0, 0.5).unwrap();
        assert_eq!(to_nodes(&out)[0].nodes.len(), before + 1);
        assert!((area(&out) - area(&c)).abs() < 0.01, "geometry unchanged");
    }

    #[test]
    fn moving_a_node_moves_exactly_that_point() {
        let sq = Rect::new(0.0, 0.0, 10.0, 10.0).to_path(1e-6);
        let out = node_move(&sq, 0, 0, Point::new(-5.0, -5.0), true).unwrap();
        let b = bbox(&out).unwrap();
        assert!((b.x0 + 5.0).abs() < 1e-9 && (b.y0 + 5.0).abs() < 1e-9, "{b:?}");
    }

    #[test]
    fn a_smooth_node_gets_collinear_handles() {
        let mut p = BezPath::new();
        p.move_to((0.0, 0.0));
        p.line_to((10.0, 10.0));
        p.line_to((20.0, 0.0));
        let out = node_set_type(&p, 0, 1, NodeType::Symmetric).unwrap();
        let n = &to_nodes(&out)[0].nodes[1];
        let (i, o) = (n.in_ctrl.unwrap(), n.out_ctrl.unwrap());
        let v1 = n.pt - i;
        let v2 = o - n.pt;
        let cross = v1.x * v2.y - v1.y * v2.x;
        assert!(cross.abs() < 1e-9, "handles are collinear");
        assert!((v1.hypot() - v2.hypot()).abs() < 1e-9, "and equal length");
    }

    #[test]
    fn appending_with_connect_makes_one_subpath() {
        let mut a = BezPath::new();
        a.move_to((0.0, 0.0));
        a.line_to((10.0, 0.0));
        let mut b = BezPath::new();
        b.move_to((20.0, 0.0));
        b.line_to((30.0, 0.0));
        assert_eq!(crate::geom::split_subpaths(&append(&[a.clone(), b.clone()], false)).len(), 2);
        assert_eq!(crate::geom::split_subpaths(&append(&[a, b], true)).len(), 1);
    }

    #[test]
    fn closing_an_open_path_adds_the_missing_edge() {
        let mut p = BezPath::new();
        p.move_to((0.0, 0.0));
        p.line_to((10.0, 0.0));
        p.line_to((10.0, 10.0));
        assert!((area(&p) - 0.0).abs() < 1e-9 || area(&p) > 0.0);
        let c = close_all(&p);
        assert!((area(&c) - 50.0).abs() < 1e-9, "triangle closes to 50: {}", area(&c));
    }
}
