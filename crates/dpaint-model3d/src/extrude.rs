//! Turning 2D outlines into solids: extrude, revolve and loft.
//!
//! Vector space is y-down, 3D space is y-up, so every profile point `(x, y)` becomes
//! `(x, -y)` before anything else happens. Extrusions run along `Z` and are centred on the
//! origin, so a node transform is all that is needed to place them.

use crate::geom::{cross, length, normalize, sub, MeshData};
use dpaint_core::doc::common::FillRule;
use dpaint_core::doc::model::{Bevel, Caps};
use dpaint_core::kurbo::{BezPath, PathEl, Point};
use dpaint_core::{Error, Result};
use lyon_tessellation::math::point as lpoint;
use lyon_tessellation::path::Path as LPath;
use lyon_tessellation::{
    BuffersBuilder, FillOptions, FillRule as LFillRule, FillTessellator, FillVertex, VertexBuffers,
};

/// A closed polygon in profile space (already y-flipped).
pub type Ring = Vec<[f64; 2]>;

/// Flatten a path into closed rings, dropping the repeated closing point and any ring with
/// fewer than three distinct points.
pub fn rings_of(path: &BezPath, tolerance: f64) -> Vec<Ring> {
    let tol = tolerance.clamp(1e-4, 100.0);
    let mut rings: Vec<Ring> = Vec::new();
    let mut cur: Ring = Vec::new();
    let push = |cur: &mut Ring, rings: &mut Vec<Ring>| {
        dedup_ring(cur);
        if cur.len() >= 3 {
            rings.push(std::mem::take(cur));
        } else {
            cur.clear();
        }
    };
    dpaint_core::kurbo::flatten(path.iter(), tol, |el| match el {
        PathEl::MoveTo(p) => {
            push(&mut cur, &mut rings);
            cur.push(flip(p));
        }
        PathEl::LineTo(p) => cur.push(flip(p)),
        PathEl::ClosePath => push(&mut cur, &mut rings),
        _ => {}
    });
    push(&mut cur, &mut rings);
    rings
}

/// Flatten a path into open polylines — what a lathe profile needs.
pub fn polylines_of(path: &BezPath, tolerance: f64) -> Vec<Vec<[f64; 2]>> {
    let tol = tolerance.clamp(1e-4, 100.0);
    let mut out: Vec<Vec<[f64; 2]>> = Vec::new();
    let mut cur: Vec<[f64; 2]> = Vec::new();
    dpaint_core::kurbo::flatten(path.iter(), tol, |el| match el {
        PathEl::MoveTo(p) => {
            if cur.len() >= 2 {
                out.push(std::mem::take(&mut cur));
            } else {
                cur.clear();
            }
            cur.push(flip(p));
        }
        PathEl::LineTo(p) => cur.push(flip(p)),
        PathEl::ClosePath => {
            if let Some(first) = cur.first().copied() {
                cur.push(first);
            }
        }
        _ => {}
    });
    if cur.len() >= 2 {
        out.push(cur);
    }
    for pl in &mut out {
        pl.dedup_by(|a, b| (a[0] - b[0]).abs() < 1e-9 && (a[1] - b[1]).abs() < 1e-9);
    }
    out.retain(|p| p.len() >= 2);
    out
}

#[inline]
fn flip(p: Point) -> [f64; 2] {
    [p.x, -p.y]
}

fn dedup_ring(r: &mut Ring) {
    r.dedup_by(|a, b| (a[0] - b[0]).abs() < 1e-9 && (a[1] - b[1]).abs() < 1e-9);
    if r.len() >= 2 {
        let (first, last) = (r[0], r[r.len() - 1]);
        if (first[0] - last[0]).abs() < 1e-9 && (first[1] - last[1]).abs() < 1e-9 {
            r.pop();
        }
    }
}

pub fn ring_area(r: &Ring) -> f64 {
    let mut a = 0.0;
    for i in 0..r.len() {
        let p = r[i];
        let q = r[(i + 1) % r.len()];
        a += p[0] * q[1] - q[0] * p[1];
    }
    a * 0.5
}

/// Is `p` inside the filled region described by `rings` under `rule`?
fn filled(p: [f64; 2], rings: &[Ring], rule: FillRule) -> bool {
    let mut winding = 0i32;
    let mut crossings = 0u32;
    for r in rings {
        for i in 0..r.len() {
            let a = r[i];
            let b = r[(i + 1) % r.len()];
            if (a[1] <= p[1]) != (b[1] <= p[1]) {
                let t = (p[1] - a[1]) / (b[1] - a[1]);
                let x = a[0] + t * (b[0] - a[0]);
                if x > p[0] {
                    crossings += 1;
                    winding += if b[1] > a[1] { 1 } else { -1 };
                }
            }
        }
    }
    match rule {
        FillRule::Nonzero => winding != 0,
        FillRule::Evenodd => crossings % 2 == 1,
    }
}

/// Orient every ring so the filled material lies to the **left** of the travel direction.
/// Outer rings come out counter-clockwise and holes clockwise regardless of how the source
/// path was authored, which is what makes the side walls face outward.
fn orient_rings(rings: &mut [Ring], rule: FillRule) {
    let snapshot: Vec<Ring> = rings.to_vec();
    for r in rings.iter_mut() {
        let Some(probe) = probe_sides(r) else {
            continue;
        };
        let (left, right) = probe;
        let l = filled(left, &snapshot, rule);
        let rr = filled(right, &snapshot, rule);
        if rr && !l {
            r.reverse();
        } else if l == rr {
            // Ambiguous (degenerate or coincident geometry): fall back to signed area,
            // treating a positive area as "material inside".
            if ring_area(r) < 0.0 && !rr {
                r.reverse();
            }
        }
    }
}

/// Points a hair to each side of the ring's longest edge midpoint.
fn probe_sides(r: &Ring) -> Option<([f64; 2], [f64; 2])> {
    let mut best = (0usize, 0.0f64);
    for i in 0..r.len() {
        let a = r[i];
        let b = r[(i + 1) % r.len()];
        let d = (b[0] - a[0]).hypot(b[1] - a[1]);
        if d > best.1 {
            best = (i, d);
        }
    }
    if best.1 <= 0.0 {
        return None;
    }
    let a = r[best.0];
    let b = r[(best.0 + 1) % r.len()];
    let m = [(a[0] + b[0]) * 0.5, (a[1] + b[1]) * 0.5];
    let d = [(b[0] - a[0]) / best.1, (b[1] - a[1]) / best.1];
    let eps = (best.1 * 1e-3).clamp(1e-9, 1e-3);
    let n = [-d[1], d[0]]; // left of travel
    Some((
        [m[0] + n[0] * eps, m[1] + n[1] * eps],
        [m[0] - n[0] * eps, m[1] - n[1] * eps],
    ))
}

/// Miter-offset a ring inward (material side) by `dist`. Returns `None` if the ring
/// collapses, which is how an over-large bevel reports itself.
fn offset_ring(r: &Ring, dist: f64) -> Option<Ring> {
    if dist.abs() < 1e-12 {
        return Some(r.clone());
    }
    let n = r.len();
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let prev = r[(i + n - 1) % n];
        let cur = r[i];
        let next = r[(i + 1) % n];
        let d0 = unit([cur[0] - prev[0], cur[1] - prev[1]])?;
        let d1 = unit([next[0] - cur[0], next[1] - cur[1]])?;
        let n0 = [-d0[1], d0[0]];
        let n1 = [-d1[1], d1[0]];
        let denom = 1.0 + n0[0] * n1[0] + n0[1] * n1[1];
        if denom < 1e-6 {
            return None; // 180-degree spike, miter runs to infinity
        }
        let m = [(n0[0] + n1[0]) / denom, (n0[1] + n1[1]) / denom];
        let miter = (m[0] * m[0] + m[1] * m[1]).sqrt();
        if miter > 8.0 {
            return None;
        }
        out.push([cur[0] + m[0] * dist, cur[1] + m[1] * dist]);
    }
    // An inset larger than the local half-width folds the contour: edges reverse before
    // the area sign does, so compare directions edge by edge.
    for i in 0..n {
        let j = (i + 1) % n;
        let a = [r[j][0] - r[i][0], r[j][1] - r[i][1]];
        let b = [out[j][0] - out[i][0], out[j][1] - out[i][1]];
        if a[0] * b[0] + a[1] * b[1] <= 0.0 {
            return None;
        }
    }
    Some(out)
}

fn unit(v: [f64; 2]) -> Option<[f64; 2]> {
    let l = v[0].hypot(v[1]);
    if l < 1e-12 {
        None
    } else {
        Some([v[0] / l, v[1] / l])
    }
}

/// Triangulate the filled region of `rings` at height `z`, facing `+Z` when `front`.
fn cap(rings: &[Ring], z: f64, front: bool, rule: FillRule, tol: f64) -> Result<MeshData> {
    let mut builder = LPath::builder();
    let mut any = false;
    for r in rings {
        if r.len() < 3 {
            continue;
        }
        any = true;
        builder.begin(lpoint(r[0][0] as f32, r[0][1] as f32));
        for p in &r[1..] {
            builder.line_to(lpoint(p[0] as f32, p[1] as f32));
        }
        builder.end(true);
    }
    if !any {
        return Ok(MeshData::default());
    }
    let path = builder.build();
    let mut buffers: VertexBuffers<lyon_tessellation::math::Point, u32> = VertexBuffers::new();
    {
        let mut builder = BuffersBuilder::new(&mut buffers, |v: FillVertex| v.position());
        let options = FillOptions::default()
            .with_fill_rule(match rule {
                FillRule::Nonzero => LFillRule::NonZero,
                FillRule::Evenodd => LFillRule::EvenOdd,
            })
            .with_tolerance((tol as f32).clamp(1e-4, 100.0));
        FillTessellator::new()
            .tessellate_path(&path, &options, &mut builder)
            .map_err(|e| Error::DegenerateGeometry(format!("cap tessellation failed: {e:?}")))?;
    }
    let normal = if front {
        [0.0, 0.0, 1.0]
    } else {
        [0.0, 0.0, -1.0]
    };
    let mut m = MeshData::default();
    let (mut lo, mut hi) = ([f64::MAX; 2], [f64::MIN; 2]);
    for v in &buffers.vertices {
        lo[0] = lo[0].min(v.x as f64);
        lo[1] = lo[1].min(v.y as f64);
        hi[0] = hi[0].max(v.x as f64);
        hi[1] = hi[1].max(v.y as f64);
    }
    let ext = [(hi[0] - lo[0]).max(1e-9), (hi[1] - lo[1]).max(1e-9)];
    for v in &buffers.vertices {
        m.positions.push([v.x, v.y, z as f32]);
        m.normals.push(normal);
        m.uvs.push([
            ((v.x as f64 - lo[0]) / ext[0]) as f32,
            ((v.y as f64 - lo[1]) / ext[1]) as f32,
        ]);
    }
    for t in buffers.indices.chunks_exact(3) {
        let (a, b, c) = (
            m.positions[t[0] as usize],
            m.positions[t[1] as usize],
            m.positions[t[2] as usize],
        );
        let ccw = (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0]) > 0.0;
        if ccw == front {
            m.indices.extend_from_slice(&[t[0], t[1], t[2]]);
        } else {
            m.indices.extend_from_slice(&[t[0], t[2], t[1]]);
        }
    }
    Ok(m)
}

/// Stitch a wall between two rings of equal length; `b` must sit at the larger `z`.
/// Assumes material to the left of travel, which `orient_rings` guarantees.
fn wall(a: &[[f64; 3]], b: &[[f64; 3]], u0: &[f32], v0: f32, v1: f32, out: &mut MeshData) {
    debug_assert_eq!(a.len(), b.len());
    let n = a.len();
    let base = out.positions.len() as u32;
    for i in 0..=n {
        let k = i % n;
        let u = if i == n { 1.0 } else { u0[k] };
        out.positions
            .push([a[k][0] as f32, a[k][1] as f32, a[k][2] as f32]);
        out.normals.push([0.0, 0.0, 1.0]);
        out.uvs.push([u, v0]);
        out.positions
            .push([b[k][0] as f32, b[k][1] as f32, b[k][2] as f32]);
        out.normals.push([0.0, 0.0, 1.0]);
        out.uvs.push([u, v1]);
    }
    for i in 0..n {
        let p = base + (i as u32) * 2;
        out.indices
            .extend_from_slice(&[p, p + 2, p + 1, p + 2, p + 3, p + 1]);
    }
}

/// Normalized cumulative perimeter position of each ring vertex, for wall UVs.
fn ring_u(r: &Ring) -> Vec<f32> {
    let mut acc = vec![0.0f32; r.len()];
    let mut total = 0.0f64;
    for i in 0..r.len() {
        let a = r[i];
        let b = r[(i + 1) % r.len()];
        acc[i] = total as f32;
        total += (b[0] - a[0]).hypot(b[1] - a[1]);
    }
    if total > 0.0 {
        for u in &mut acc {
            *u /= total as f32;
        }
    }
    acc
}

/// Extrude closed rings into a solid along `Z`, centred on the origin.
///
/// With a bevel of size `b` the straight wall still spans `depth`, and each bevel adds a
/// quarter-round of radius `b` beyond it, so the solid measures `depth + 2*b` on `Z` and
/// never grows in `X`/`Y`.
pub fn extrude_rings(
    mut rings: Vec<Ring>,
    depth: f32,
    bevel: Option<Bevel>,
    caps: Caps,
    rule: FillRule,
    tolerance: f64,
) -> Result<MeshData> {
    if rings.is_empty() {
        return Err(Error::DegenerateGeometry(
            "path has no closed subpath to extrude".into(),
        ));
    }
    if !depth.is_finite() || depth <= 0.0 {
        return Err(Error::DegenerateGeometry(format!(
            "extrude depth must be positive, got {depth}"
        )));
    }
    orient_rings(&mut rings, rule);
    let half = depth as f64 * 0.5;

    // (z, inset) levels from back to front.
    let mut levels: Vec<(f64, f64)> = Vec::new();
    match bevel {
        Some(b) if b.size > 0.0 => {
            let k = b.segments.clamp(1, 64);
            let bs = b.size as f64;
            for i in 0..=k {
                let phi = std::f64::consts::FRAC_PI_2 * (i as f64 / k as f64);
                levels.push((-half - bs * (1.0 - phi.sin()), bs * phi.cos()));
            }
            for i in 0..=k {
                let phi = std::f64::consts::FRAC_PI_2 * (1.0 - i as f64 / k as f64);
                levels.push((half + bs * (1.0 - phi.sin()), bs * phi.cos()));
            }
        }
        _ => {
            levels.push((-half, 0.0));
            levels.push((half, 0.0));
        }
    }

    // Offset every ring once per distinct inset.
    let mut out = MeshData::default();
    let zmin = levels.first().unwrap().0;
    let zmax = levels.last().unwrap().0;
    let zext = (zmax - zmin).max(1e-9);
    for r in &rings {
        let u = ring_u(r);
        let mut prev: Option<(Vec<[f64; 3]>, f64)> = None;
        for &(z, inset) in &levels {
            let offset = if inset > 0.0 {
                offset_ring(r, inset).ok_or_else(|| {
                    Error::DegenerateGeometry(format!(
                        "bevel of {inset:.4} collapses a contour; reduce bevel.size"
                    ))
                })?
            } else {
                r.clone()
            };
            let lifted: Vec<[f64; 3]> = offset.iter().map(|p| [p[0], p[1], z]).collect();
            if let Some((pa, pz)) = prev.take() {
                wall(
                    &pa,
                    &lifted,
                    &u,
                    ((pz - zmin) / zext) as f32,
                    ((z - zmin) / zext) as f32,
                    &mut out,
                );
            }
            prev = Some((lifted, z));
        }
    }

    let cap_inset = levels.last().unwrap().1;
    let capped_rings: Vec<Ring> = if cap_inset > 0.0 {
        rings
            .iter()
            .map(|r| {
                offset_ring(r, cap_inset).ok_or_else(|| {
                    Error::DegenerateGeometry("bevel collapses a contour at the cap".into())
                })
            })
            .collect::<Result<_>>()?
    } else {
        rings.clone()
    };
    if matches!(caps, Caps::Both | Caps::Front) {
        out.append(&cap(&capped_rings, zmax, true, rule, tolerance)?);
    }
    if matches!(caps, Caps::Both | Caps::Back) {
        out.append(&cap(&capped_rings, zmin, false, rule, tolerance)?);
    }
    out.drop_degenerate(1e-12);
    if out.indices.is_empty() {
        return Err(Error::DegenerateGeometry(
            "extrusion produced no triangles".into(),
        ));
    }
    out.recompute_normals(35.0);
    Ok(out)
}

/// Lathe an open profile around the `Y` axis. `x` is the radius, `-y` the height; run the
/// profile bottom to top for outward normals.
pub fn revolve_profile(profile: &[[f64; 2]], angle_deg: f32, segments: u32) -> Result<MeshData> {
    if profile.len() < 2 {
        return Err(Error::DegenerateGeometry(
            "revolve needs a profile with at least two points".into(),
        ));
    }
    if !angle_deg.is_finite() || angle_deg.abs() < 1e-3 {
        return Err(Error::DegenerateGeometry(format!(
            "revolve angle must be non-zero, got {angle_deg}"
        )));
    }
    let seg = segments.clamp(3, 512);
    let total = (angle_deg as f64).to_radians();
    let closed = (angle_deg.abs() - 360.0).abs() < 1e-3;

    // Profile arc length for V coordinates.
    let mut vs = vec![0.0f32; profile.len()];
    let mut acc = 0.0f64;
    for i in 1..profile.len() {
        acc += (profile[i][0] - profile[i - 1][0]).hypot(profile[i][1] - profile[i - 1][1]);
        vs[i] = acc as f32;
    }
    if acc > 0.0 {
        for v in &mut vs {
            *v /= acc as f32;
        }
    }

    let mut m = MeshData::default();
    let rings = seg + 1;
    for s in 0..rings {
        let u = s as f64 / seg as f64;
        let theta = total * u;
        let (st, ct) = (theta.sin(), theta.cos());
        for (i, p) in profile.iter().enumerate() {
            let r = p[0];
            m.positions
                .push([(r * ct) as f32, p[1] as f32, (r * st) as f32]);
            m.normals.push([0.0, 1.0, 0.0]);
            m.uvs.push([u as f32, vs[i]]);
        }
    }
    let stride = profile.len() as u32;
    for s in 0..seg {
        let s1 = if closed && s + 1 == seg { 0 } else { s + 1 };
        for t in 0..stride - 1 {
            let a = s * stride + t;
            let b = a + 1;
            let d = s1 * stride + t;
            let c = d + 1;
            m.indices.extend_from_slice(&[a, b, d, b, c, d]);
        }
    }
    m.drop_degenerate(1e-14);
    if m.indices.is_empty() {
        return Err(Error::DegenerateGeometry(
            "revolve produced no triangles (profile lies on the axis?)".into(),
        ));
    }
    m.recompute_normals(35.0);
    Ok(m)
}

/// Skin a surface through ordered closed sections, one unit apart along `Z` and centred on
/// the origin. Sections are resampled to a common vertex count and phase-aligned so the skin
/// does not twist.
pub fn loft_sections(sections: Vec<Vec<Ring>>, rule: FillRule, tolerance: f64) -> Result<MeshData> {
    if sections.len() < 2 {
        return Err(Error::DegenerateGeometry(
            "loft needs at least two sections".into(),
        ));
    }
    let mut outer: Vec<Ring> = Vec::with_capacity(sections.len());
    for mut rings in sections {
        if rings.is_empty() {
            return Err(Error::DegenerateGeometry(
                "loft section has no closed subpath".into(),
            ));
        }
        orient_rings(&mut rings, rule);
        // Largest ring by |area| carries the skin; holes cannot be skinned coherently.
        rings.sort_by(|a, b| {
            ring_area(b)
                .abs()
                .partial_cmp(&ring_area(a).abs())
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        outer.push(rings.remove(0));
    }
    let n = outer
        .iter()
        .map(|r| r.len())
        .max()
        .unwrap_or(3)
        .clamp(3, 4096);
    let resampled: Vec<Ring> = outer.iter().map(|r| resample_ring(r, n)).collect();

    let count = resampled.len();
    let mut m = MeshData::default();
    let u = ring_u(&resampled[0]);
    for i in 0..count - 1 {
        let za = i as f64 - (count - 1) as f64 * 0.5;
        let zb = za + 1.0;
        let a: Vec<[f64; 3]> = resampled[i].iter().map(|p| [p[0], p[1], za]).collect();
        let b: Vec<[f64; 3]> = resampled[i + 1].iter().map(|p| [p[0], p[1], zb]).collect();
        let v0 = i as f32 / (count - 1) as f32;
        let v1 = (i + 1) as f32 / (count - 1) as f32;
        wall(&a, &b, &u, v0, v1, &mut m);
    }
    let zmin = -((count - 1) as f64) * 0.5;
    let zmax = ((count - 1) as f64) * 0.5;
    m.append(&cap(
        std::slice::from_ref(resampled.last().unwrap()),
        zmax,
        true,
        rule,
        tolerance,
    )?);
    m.append(&cap(
        std::slice::from_ref(&resampled[0]),
        zmin,
        false,
        rule,
        tolerance,
    )?);
    m.drop_degenerate(1e-12);
    if m.indices.is_empty() {
        return Err(Error::DegenerateGeometry(
            "loft produced no triangles".into(),
        ));
    }
    m.recompute_normals(35.0);
    Ok(m)
}

/// Resample a closed ring to `n` points at uniform arc length, starting from the vertex
/// whose direction from the centroid is closest to `+X`.
fn resample_ring(r: &Ring, n: usize) -> Ring {
    let centroid = r
        .iter()
        .fold([0.0f64; 2], |a, p| [a[0] + p[0], a[1] + p[1]]);
    let centroid = [centroid[0] / r.len() as f64, centroid[1] / r.len() as f64];
    let start = (0..r.len())
        .min_by(|&a, &b| {
            let ang = |i: usize| (r[i][1] - centroid[1]).atan2(r[i][0] - centroid[0]).abs();
            ang(a)
                .partial_cmp(&ang(b))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .unwrap_or(0);
    let rot: Ring = (0..r.len()).map(|i| r[(start + i) % r.len()]).collect();

    let mut lens = Vec::with_capacity(rot.len());
    let mut total = 0.0;
    for i in 0..rot.len() {
        let a = rot[i];
        let b = rot[(i + 1) % rot.len()];
        let d = (b[0] - a[0]).hypot(b[1] - a[1]);
        lens.push(d);
        total += d;
    }
    if total <= 0.0 {
        return rot;
    }
    let mut out = Vec::with_capacity(n);
    let step = total / n as f64;
    let (mut seg, mut walked) = (0usize, 0.0f64);
    for i in 0..n {
        let target = i as f64 * step;
        while seg + 1 < lens.len() && walked + lens[seg] < target {
            walked += lens[seg];
            seg += 1;
        }
        let t = if lens[seg] > 0.0 {
            (target - walked) / lens[seg]
        } else {
            0.0
        };
        let a = rot[seg];
        let b = rot[(seg + 1) % rot.len()];
        out.push([a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t]);
    }
    out
}

/// Surface normal helper shared with the validator.
pub fn triangle_normal(a: [f32; 3], b: [f32; 3], c: [f32; 3]) -> [f32; 3] {
    let n = cross(sub(b, a), sub(c, a));
    if length(n) <= f32::MIN_POSITIVE {
        [0.0, 0.0, 0.0]
    } else {
        normalize(n)
    }
}
