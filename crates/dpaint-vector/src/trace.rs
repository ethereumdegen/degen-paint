//! Local, offline raster → vector tracing.
//!
//! Marching squares over a thresholded or colour-quantised image produces closed pixel
//! contours; those are decimated with Douglas–Peucker, corner-detected, and refitted as
//! cubics. No network, no model, no randomness — the same pixels always trace to the same
//! paths.

use crate::pathops::douglas_peucker;
use dpaint_core::color::Color;
use dpaint_core::error::{Error, Result};
use dpaint_core::kurbo::{BezPath, Point, Vec2};
use serde::Deserialize;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, schemars::JsonSchema, Default)]
#[serde(rename_all = "kebab-case")]
pub enum TraceMode {
    /// One shape set, everything darker (or more opaque) than `threshold`.
    #[default]
    Binary,
    /// Quantise to `colors` flat colours and trace each one.
    Color,
}

#[derive(Debug, Clone, Copy)]
pub struct TraceOptions {
    pub mode: TraceMode,
    /// Palette size for `color` mode, 2..=64.
    pub colors: u32,
    /// Luminance cut for `binary` mode, 0..=1.
    pub threshold: f64,
    /// Contours smaller than this many pixels of area are discarded.
    pub speckle: f64,
    /// Turn angle in degrees above which a vertex stays a hard corner.
    pub corner_threshold: f64,
    /// Decimation tolerance in pixels.
    pub tolerance: f64,
}

impl Default for TraceOptions {
    fn default() -> Self {
        Self {
            mode: TraceMode::Binary,
            colors: 8,
            threshold: 0.5,
            speckle: 4.0,
            corner_threshold: 60.0,
            tolerance: 1.0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct TracedShape {
    pub path: BezPath,
    pub color: Color,
    /// Filled area in source pixels, used to order shapes back to front.
    pub area: f64,
}

/// Trace a premultiplied RGBA pixmap into filled paths in pixel coordinates.
pub fn trace(pm: &tiny_skia::Pixmap, opts: &TraceOptions) -> Result<Vec<TracedShape>> {
    let (w, h) = (pm.width() as usize, pm.height() as usize);
    if w == 0 || h == 0 {
        return Err(Error::DegenerateGeometry(
            "cannot trace an empty image".into(),
        ));
    }
    let px: Vec<[f32; 4]> = pm
        .pixels()
        .iter()
        .map(|p| {
            let a = p.alpha() as f32 / 255.0;
            if a <= 0.0 {
                [0.0, 0.0, 0.0, 0.0]
            } else {
                // tiny-skia stores premultiplied; undo it so colours quantise sanely.
                [
                    p.red() as f32 / 255.0 / a,
                    p.green() as f32 / 255.0 / a,
                    p.blue() as f32 / 255.0 / a,
                    a,
                ]
            }
        })
        .collect();

    let mut out = Vec::new();
    match opts.mode {
        TraceMode::Binary => {
            let t = opts.threshold.clamp(0.0, 1.0) as f32;
            let mask: Vec<bool> = px.iter().map(|c| c[3] > 0.5 && luma(c) < t).collect();
            let mut avg = [0.0f32; 3];
            let mut n = 0.0f32;
            for (c, m) in px.iter().zip(&mask) {
                if *m {
                    avg[0] += c[0];
                    avg[1] += c[1];
                    avg[2] += c[2];
                    n += 1.0;
                }
            }
            let color = if n > 0.0 {
                Color::rgba(avg[0] / n, avg[1] / n, avg[2] / n, 1.0)
            } else {
                Color::BLACK
            };
            out.extend(shapes_from_mask(&mask, w, h, color, opts));
        }
        TraceMode::Color => {
            let k = opts.colors.clamp(2, 64) as usize;
            let palette = median_cut(&px, k);
            if palette.is_empty() {
                return Err(Error::DegenerateGeometry(
                    "the image is fully transparent; nothing to trace".into(),
                ));
            }
            let assign: Vec<Option<usize>> = px
                .iter()
                .map(|c| {
                    if c[3] <= 0.5 {
                        None
                    } else {
                        Some(nearest(&palette, c))
                    }
                })
                .collect();
            for (i, color) in palette.iter().enumerate() {
                let mask: Vec<bool> = assign.iter().map(|a| *a == Some(i)).collect();
                out.extend(shapes_from_mask(&mask, w, h, *color, opts));
            }
        }
    }
    // Largest first: the painter's-algorithm order an editor expects.
    out.sort_by(|a, b| {
        b.area
            .partial_cmp(&a.area)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    if out.is_empty() {
        return Err(Error::DegenerateGeometry(
            "tracing found no regions; adjust the threshold or speckle filter".into(),
        ));
    }
    Ok(out)
}

fn luma(c: &[f32; 4]) -> f32 {
    0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]
}

fn nearest(palette: &[Color], c: &[f32; 4]) -> usize {
    let mut best = 0;
    let mut bd = f32::INFINITY;
    for (i, p) in palette.iter().enumerate() {
        let d = (p.r - c[0]).powi(2) + (p.g - c[1]).powi(2) + (p.b - c[2]).powi(2);
        if d < bd {
            bd = d;
            best = i;
        }
    }
    best
}

/// Median cut: deterministic, no seeds, no iteration count to tune.
fn median_cut(px: &[[f32; 4]], k: usize) -> Vec<Color> {
    let mut buckets: Vec<Vec<[f32; 4]>> = vec![px.iter().filter(|c| c[3] > 0.5).copied().collect()];
    if buckets[0].is_empty() {
        return Vec::new();
    }
    while buckets.len() < k {
        // Split the bucket with the widest channel spread.
        let Some((idx, ch)) = buckets
            .iter()
            .enumerate()
            .filter(|(_, b)| b.len() > 1)
            .map(|(i, b)| {
                let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
                for c in b {
                    for j in 0..3 {
                        lo[j] = lo[j].min(c[j]);
                        hi[j] = hi[j].max(c[j]);
                    }
                }
                let spans = [hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2]];
                let ch = (0..3)
                    .max_by(|a, b| spans[*a].total_cmp(&spans[*b]))
                    .unwrap();
                (i, ch, spans[ch])
            })
            .max_by(|a, b| a.2.total_cmp(&b.2))
            .map(|(i, ch, _)| (i, ch))
        else {
            break;
        };
        let mut b = buckets.swap_remove(idx);
        b.sort_by(|x, y| x[ch].total_cmp(&y[ch]));
        let half = b.len() / 2;
        let right = b.split_off(half.max(1));
        buckets.push(b);
        if !right.is_empty() {
            buckets.push(right);
        }
    }
    let mut out: Vec<Color> = buckets
        .iter()
        .filter(|b| !b.is_empty())
        .map(|b| {
            let n = b.len() as f32;
            let s = b.iter().fold([0.0f32; 3], |mut a, c| {
                a[0] += c[0];
                a[1] += c[1];
                a[2] += c[2];
                a
            });
            Color::rgba(s[0] / n, s[1] / n, s[2] / n, 1.0)
        })
        .collect();
    out.sort_by(|a, b| {
        a.luminance()
            .total_cmp(&b.luminance())
            .then(a.r.total_cmp(&b.r))
    });
    out.dedup_by(|a, b| a.delta_e(*b) < 0.5);
    out
}

fn shapes_from_mask(
    mask: &[bool],
    w: usize,
    h: usize,
    color: Color,
    opts: &TraceOptions,
) -> Vec<TracedShape> {
    let rings = marching_squares(mask, w, h);
    let mut path = BezPath::new();
    let mut total = 0.0;
    for ring in rings {
        let a = polygon_area(&ring);
        if a.abs() < opts.speckle.max(0.0) {
            continue;
        }
        total += a;
        let keep = douglas_peucker(&ring, opts.tolerance.max(0.01), true);
        if keep.len() < 3 {
            continue;
        }
        path.extend(fit_with_corners(&keep, opts.corner_threshold));
    }
    if path.elements().is_empty() {
        return Vec::new();
    }
    vec![TracedShape {
        path,
        color,
        area: total.abs(),
    }]
}

fn polygon_area(pts: &[Point]) -> f64 {
    let n = pts.len();
    if n < 3 {
        return 0.0;
    }
    let mut a = 0.0;
    for i in 0..n {
        let p = pts[i];
        let q = pts[(i + 1) % n];
        a += p.x * q.y - q.x * p.y;
    }
    a / 2.0
}

/// Marching squares over the pixel-centre grid, returning closed contours.
/// The grid is padded by one cell so regions touching the border still close.
fn marching_squares(mask: &[bool], w: usize, h: usize) -> Vec<Vec<Point>> {
    let at = |x: i64, y: i64| -> bool {
        if x < 0 || y < 0 || x >= w as i64 || y >= h as i64 {
            false
        } else {
            mask[y as usize * w + x as usize]
        }
    };
    // key = (x*2, y*2) so every half-pixel midpoint has an exact integer key.
    let mut links: BTreeMap<(i64, i64), Vec<(i64, i64)>> = BTreeMap::new();
    let key = |p: Point| ((p.x * 2.0).round() as i64, (p.y * 2.0).round() as i64);
    let mut add = |a: Point, b: Point| {
        links.entry(key(a)).or_default().push(key(b));
    };
    for cy in -1..h as i64 {
        for cx in -1..w as i64 {
            let (tl, tr, br, bl) = (
                at(cx, cy),
                at(cx + 1, cy),
                at(cx + 1, cy + 1),
                at(cx, cy + 1),
            );
            let case = (tl as u8) << 3 | (tr as u8) << 2 | (br as u8) << 1 | bl as u8;
            let x = cx as f64;
            let y = cy as f64;
            let t = Point::new(x + 1.0, y + 0.5);
            let r = Point::new(x + 1.5, y + 1.0);
            let b = Point::new(x + 1.0, y + 1.5);
            let l = Point::new(x + 0.5, y + 1.0);
            match case {
                1 => add(l, b),
                2 => add(b, r),
                3 => add(l, r),
                4 => add(r, t),
                5 => {
                    add(l, t);
                    add(r, b);
                }
                6 => add(b, t),
                7 => add(l, t),
                8 => add(t, l),
                9 => add(t, b),
                10 => {
                    add(t, r);
                    add(b, l);
                }
                11 => add(t, r),
                12 => add(r, l),
                13 => add(r, b),
                14 => add(b, l),
                _ => {}
            }
        }
    }
    let mut out = Vec::new();
    let starts: Vec<(i64, i64)> = links.keys().copied().collect();
    for s in starts {
        while links.get(&s).map(|v| !v.is_empty()).unwrap_or(false) {
            let mut ring = Vec::new();
            let mut cur = s;
            while let Some(nexts) = links.get_mut(&cur) {
                let Some(next) = nexts.pop() else { break };
                ring.push(Point::new(cur.0 as f64 / 2.0, cur.1 as f64 / 2.0));
                if next == s {
                    break;
                }
                cur = next;
                if ring.len() > 4 * (w + 2) * (h + 2) {
                    break;
                }
            }
            if ring.len() >= 3 {
                out.push(ring);
            }
        }
    }
    out
}

/// Refit a decimated ring as cubics, keeping hard corners where the turn exceeds
/// `corner_deg`.
fn fit_with_corners(pts: &[Point], corner_deg: f64) -> BezPath {
    let n = pts.len();
    let mut p = BezPath::new();
    if n < 3 {
        return p;
    }
    let cos_limit = corner_deg.clamp(0.0, 180.0).to_radians().cos();
    let corner: Vec<bool> = (0..n)
        .map(|i| {
            let a = pts[(i + n - 1) % n];
            let b = pts[i];
            let c = pts[(i + 1) % n];
            let (u, v) = (b - a, c - b);
            if u.hypot() < 1e-9 || v.hypot() < 1e-9 {
                return true;
            }
            u.normalize().dot(v.normalize()) < cos_limit
        })
        .collect();
    let tangent = |i: usize| -> Vec2 {
        if corner[i] {
            Vec2::ZERO
        } else {
            (pts[(i + 1) % n] - pts[(i + n - 1) % n]) / 6.0
        }
    };
    p.move_to(pts[0]);
    for i in 0..n {
        let j = (i + 1) % n;
        let (a, b) = (pts[i], pts[j]);
        let (ta, tb) = (tangent(i), tangent(j));
        if ta == Vec2::ZERO && tb == Vec2::ZERO {
            p.line_to(b);
        } else {
            p.curve_to(a + ta, b - tb, b);
        }
    }
    p.close_path();
    p
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom;
    use tiny_skia::{Pixmap, PixmapPaint, Transform};

    fn square_image(w: u32, h: u32, rect: (u32, u32, u32, u32), c: [u8; 4]) -> Pixmap {
        let mut pm = Pixmap::new(w, h).unwrap();
        let mut fill = Pixmap::new(rect.2, rect.3).unwrap();
        fill.fill(tiny_skia::Color::from_rgba8(c[0], c[1], c[2], c[3]));
        pm.draw_pixmap(
            rect.0 as i32,
            rect.1 as i32,
            fill.as_ref(),
            &PixmapPaint::default(),
            Transform::identity(),
            None,
        );
        pm
    }

    #[test]
    fn a_black_square_traces_to_one_region_of_the_right_area() {
        let pm = square_image(64, 64, (16, 16, 32, 32), [0, 0, 0, 255]);
        let shapes = trace(&pm, &TraceOptions::default()).unwrap();
        assert_eq!(shapes.len(), 1);
        let a = geom::area(&shapes[0].path);
        assert!((a - 1024.0).abs() < 80.0, "traced area {a} ~ 32x32");
        let b = geom::bbox(&shapes[0].path).unwrap();
        assert!(
            (b.x0 - 15.5).abs() < 1.5 && (b.x1 - 47.5).abs() < 1.5,
            "bbox {b:?}"
        );
    }

    #[test]
    fn a_ring_traces_to_an_outer_contour_and_a_hole() {
        let mut pm = square_image(64, 64, (8, 8, 48, 48), [0, 0, 0, 255]);
        let hole = square_image(64, 64, (24, 24, 16, 16), [255, 255, 255, 255]);
        pm.draw_pixmap(
            0,
            0,
            hole.as_ref(),
            &PixmapPaint::default(),
            Transform::identity(),
            None,
        );
        let shapes = trace(&pm, &TraceOptions::default()).unwrap();
        let path = &shapes[0].path;
        assert_eq!(geom::split_subpaths(path).len(), 2, "outer plus hole");
        let a = geom::area(path);
        assert!(
            (a - (48.0 * 48.0 - 16.0 * 16.0)).abs() < 150.0,
            "ring area {a}"
        );
    }

    #[test]
    fn the_speckle_filter_drops_tiny_blobs() {
        let mut pm = square_image(64, 64, (10, 10, 20, 20), [0, 0, 0, 255]);
        let dot = square_image(64, 64, (50, 50, 2, 2), [0, 0, 0, 255]);
        pm.draw_pixmap(
            0,
            0,
            dot.as_ref(),
            &PixmapPaint::default(),
            Transform::identity(),
            None,
        );
        let mut opts = TraceOptions {
            speckle: 0.0,
            ..Default::default()
        };
        let all = trace(&pm, &opts).unwrap();
        opts.speckle = 20.0;
        let filtered = trace(&pm, &opts).unwrap();
        assert!(
            geom::split_subpaths(&all[0].path).len()
                > geom::split_subpaths(&filtered[0].path).len(),
            "the 2x2 dot is filtered out"
        );
    }

    #[test]
    fn colour_mode_separates_two_flat_colours() {
        let mut pm = square_image(40, 20, (0, 0, 20, 20), [255, 0, 0, 255]);
        let blue = square_image(40, 20, (20, 0, 20, 20), [0, 0, 255, 255]);
        pm.draw_pixmap(
            0,
            0,
            blue.as_ref(),
            &PixmapPaint::default(),
            Transform::identity(),
            None,
        );
        let opts = TraceOptions {
            mode: TraceMode::Color,
            colors: 2,
            ..Default::default()
        };
        let shapes = trace(&pm, &opts).unwrap();
        assert_eq!(shapes.len(), 2, "one shape per colour");
        assert!(shapes.iter().any(|s| s.color.r > 0.8 && s.color.b < 0.2));
        assert!(shapes.iter().any(|s| s.color.b > 0.8 && s.color.r < 0.2));
    }

    #[test]
    fn tracing_is_deterministic() {
        let pm = square_image(32, 32, (4, 4, 10, 18), [0, 0, 0, 255]);
        let a = trace(&pm, &TraceOptions::default()).unwrap();
        let b = trace(&pm, &TraceOptions::default()).unwrap();
        assert_eq!(geom::to_d(&a[0].path), geom::to_d(&b[0].path));
    }
}
