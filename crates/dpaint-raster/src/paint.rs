//! Painting: gradients, brush strokes, bucket fill, pattern tiling, erase.
//!
//! Gradients interpolate in linear light, so a black-to-white ramp is a ramp in *light*
//! and a red-to-green ramp does not dip through mud in the middle.

use crate::adjust::Curve;
use crate::blend::{composite, hash01, Coverage};
use crate::canvas::Canvas;
use crate::geom;
use dpaint_core::color::Color;
use dpaint_core::doc::common::{GradientStop, Paint};
use dpaint_core::doc::raster::BlendMode;
use dpaint_core::kurbo::{BezPath, ParamCurve, PathSeg, Point, Shape};
use dpaint_core::{DocId, Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum GradientKind {
    #[default]
    Linear,
    Radial,
    /// Sweep around the start point; the angle to the end point is the 0 position.
    Angular,
    Diamond,
}

fn sample_stops(stops: &[GradientStop], t: f32) -> [f32; 4] {
    if stops.is_empty() {
        return [0.0; 4];
    }
    let t = t.clamp(0.0, 1.0) as f64;
    let first = &stops[0];
    if t <= first.offset {
        return first.color.to_linear();
    }
    let last = &stops[stops.len() - 1];
    if t >= last.offset {
        return last.color.to_linear();
    }
    for pair in stops.windows(2) {
        let (a, b) = (&pair[0], &pair[1]);
        if t >= a.offset && t <= b.offset {
            let span = b.offset - a.offset;
            let f = if span.abs() < 1e-12 {
                0.0
            } else {
                (t - a.offset) / span
            } as f32;
            let (ca, cb) = (a.color.to_linear(), b.color.to_linear());
            let mut out = [0.0f32; 4];
            for c in 0..4 {
                out[c] = ca[c] + (cb[c] - ca[c]) * f;
            }
            return out;
        }
    }
    last.color.to_linear()
}

/// Render a gradient across a whole device-space buffer. `from`/`to` are in document
/// coordinates; `scale` maps them to device pixels.
pub fn gradient_canvas(
    w: u32,
    h: u32,
    kind: GradientKind,
    from: [f64; 2],
    to: [f64; 2],
    stops: &[GradientStop],
    scale: f64,
) -> Canvas {
    let mut sorted: Vec<GradientStop> = stops.to_vec();
    sorted.sort_by(|a, b| {
        a.offset
            .partial_cmp(&b.offset)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let p0 = (from[0] * scale, from[1] * scale);
    let p1 = (to[0] * scale, to[1] * scale);
    let (dx, dy) = (p1.0 - p0.0, p1.1 - p0.1);
    let len2 = dx * dx + dy * dy;
    let len = len2.sqrt();
    let mut out = Canvas::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let px = x as f64 + 0.5;
            let py = y as f64 + 0.5;
            let (vx, vy) = (px - p0.0, py - p0.1);
            let t = match kind {
                GradientKind::Linear => {
                    if len2 < 1e-9 {
                        0.0
                    } else {
                        (vx * dx + vy * dy) / len2
                    }
                }
                GradientKind::Radial => {
                    if len < 1e-9 {
                        0.0
                    } else {
                        (vx * vx + vy * vy).sqrt() / len
                    }
                }
                GradientKind::Angular => {
                    let base = dy.atan2(dx);
                    let a = vy.atan2(vx) - base;
                    a.rem_euclid(std::f64::consts::TAU) / std::f64::consts::TAU
                }
                GradientKind::Diamond => {
                    if len < 1e-9 {
                        0.0
                    } else {
                        let (ux, uy) = (dx / len, dy / len);
                        let a = vx * ux + vy * uy;
                        let b = -vx * uy + vy * ux;
                        (a.abs() + b.abs()) / len
                    }
                }
            };
            let straight = sample_stops(&sorted, t as f32);
            let i = out.idx(x, y);
            out.set_straight(i, straight);
        }
    }
    out
}

/// Realize a `Paint` as a full-buffer canvas. `link` renders a referenced document when the
/// paint is `Paint::Document`.
pub fn paint_canvas(
    paint: &Paint,
    w: u32,
    h: u32,
    scale: f64,
    link: &dyn Fn(&DocId, u32, u32) -> Result<tiny_skia::Pixmap>,
) -> Result<Canvas> {
    Ok(match paint {
        Paint::Solid { color } => {
            let mut c = Canvas::new(w, h);
            let s = color.to_linear();
            for i in (0..c.data.len()).step_by(4) {
                c.set_straight(i, s);
            }
            c
        }
        Paint::Linear { stops, from, to } => {
            gradient_canvas(w, h, GradientKind::Linear, *from, *to, stops, scale)
        }
        Paint::Radial {
            stops,
            center,
            radius,
            focal,
        } => {
            // A focal point offsets where offset 0 sits; the radius still sets the span, so
            // the ramp runs from the focal point outward over `radius` pixels.
            let origin = focal.unwrap_or(*center);
            let to = [origin[0] + radius, origin[1]];
            gradient_canvas(w, h, GradientKind::Radial, origin, to, stops, scale)
        }
        Paint::Document { document } => {
            let pm = link(document, w, h)?;
            Canvas::from_pixmap(pm.as_ref()).resized(w, h)
        }
        Paint::None => Canvas::new(w, h),
    })
}

/// Fill `dst` through a coverage buffer with a solid color.
pub fn fill_cov(dst: &mut Canvas, cov: &[f32], color: Color, mode: BlendMode, opacity: f32) {
    let mut src = Canvas::new(dst.width, dst.height);
    let s = color.to_linear();
    for i in (0..src.data.len()).step_by(4) {
        src.set_straight(i, s);
    }
    composite(dst, &src, mode, opacity, &Coverage::Buffer(cov), 0);
}

/// Erase through a coverage buffer: coverage 1 with flow 1 clears the pixel completely.
pub fn erase_cov(dst: &mut Canvas, cov: &[f32], flow: f32) {
    for (i, px) in dst.data.chunks_exact_mut(4).enumerate() {
        let m = cov.get(i).copied().unwrap_or(0.0).clamp(0.0, 1.0) * flow.clamp(0.0, 1.0);
        if m <= 0.0 {
            continue;
        }
        let keep = 1.0 - m;
        for v in px.iter_mut() {
            *v *= keep;
        }
    }
}

/// Brush parameters. `size` is the full diameter in document px, `hardness` the fraction of
/// the radius that stays fully opaque, `spacing` the stamp step as a fraction of the size.
#[derive(Debug, Clone, Copy)]
pub struct Brush {
    pub size: f64,
    pub hardness: f64,
    pub spacing: f64,
    pub flow: f64,
    pub jitter: f64,
    pub seed: u64,
}

impl Default for Brush {
    fn default() -> Self {
        Self {
            size: 12.0,
            hardness: 0.8,
            spacing: 0.25,
            flow: 1.0,
            jitter: 0.0,
            seed: 0,
        }
    }
}

/// Accumulate a stroke's coverage along `path`, stamping a soft round tip.
///
/// Coverage composites stamp over stamp (`c + f*(1-c)`) rather than summing, so a slow
/// stroke over itself saturates instead of ringing, which is what flow means in GIMP.
///
/// `pressure` maps stroke progress (0 at the start, 1 at the end) to a multiplier on both
/// the tip size and the flow — that is how a tapered stroke is expressed without a tablet.
pub fn stroke_coverage(
    w: u32,
    h: u32,
    path: &BezPath,
    brush: &Brush,
    scale: f64,
    pressure: Option<&Curve>,
) -> Result<Vec<f32>> {
    let base_radius = (brush.size * scale / 2.0).max(0.5);
    let hardness = brush.hardness.clamp(0.0, 1.0);
    let flow = brush.flow.clamp(0.0, 1.0) as f32;
    let step = (brush.spacing.max(0.01) * brush.size * scale).max(0.5);
    let mut cov = vec![0.0f32; w as usize * h as usize];
    let mut stamps: Vec<Point> = Vec::new();
    let mut carry = 0.0f64;
    let mut stamped_any = false;
    for seg in path.segments() {
        let len = match seg {
            PathSeg::Line(l) => (l.p1 - l.p0).hypot(),
            _ => seg
                .to_path(0.05)
                .segments()
                .map(|s| s.as_line().map(|l| (l.p1 - l.p0).hypot()).unwrap_or(0.0))
                .sum(),
        };
        let len = len * scale;
        if len <= 0.0 {
            if !stamped_any {
                stamps.push(seg.eval(0.0));
                stamped_any = true;
            }
            continue;
        }
        let mut t = if stamped_any { carry / len } else { 0.0 };
        if !stamped_any {
            stamps.push(seg.eval(0.0));
            stamped_any = true;
            t = step / len;
        }
        while t <= 1.0 {
            stamps.push(seg.eval(t));
            t += step / len;
        }
        carry = (t - 1.0) * len;
    }
    if stamps.is_empty() {
        return Err(Error::DegenerateGeometry("brush path has no points".into()));
    }
    let last = (stamps.len().saturating_sub(1)).max(1) as f64;
    for (n, p) in stamps.iter().enumerate() {
        // Pressure taper: 0 at the first stamp, 1 at the last.
        let press = match pressure {
            Some(c) => c.eval(n as f64 / last).clamp(0.0, 4.0),
            None => 1.0,
        };
        if press <= 0.0 {
            continue;
        }
        let radius = (base_radius * press).max(0.5);
        let (mut cx, mut cy) = (p.x * scale, p.y * scale);
        if brush.jitter > 0.0 {
            let j = brush.jitter * brush.size * scale;
            cx += (hash01(n as u32, 0, brush.seed) as f64 - 0.5) * 2.0 * j;
            cy += (hash01(n as u32, 1, brush.seed) as f64 - 0.5) * 2.0 * j;
        }
        let x0 = ((cx - radius).floor() as i64).max(0);
        let x1 = ((cx + radius).ceil() as i64).min(w as i64 - 1);
        let y0 = ((cy - radius).floor() as i64).max(0);
        let y1 = ((cy + radius).ceil() as i64).min(h as i64 - 1);
        for y in y0..=y1 {
            for x in x0..=x1 {
                let dx = x as f64 + 0.5 - cx;
                let dy = y as f64 + 0.5 - cy;
                let d = (dx * dx + dy * dy).sqrt() / radius;
                let a = if d <= hardness {
                    1.0
                } else if d >= 1.0 {
                    0.0
                } else {
                    let t = (1.0 - d) / (1.0 - hardness).max(1e-6);
                    t * t * (3.0 - 2.0 * t)
                };
                if a <= 0.0 {
                    continue;
                }
                let i = y as usize * w as usize + x as usize;
                let f = a as f32 * flow * (press as f32).min(1.0);
                cov[i] += f * (1.0 - cov[i]);
            }
        }
    }
    Ok(cov)
}

/// Tile a pattern image across a coverage region.
pub fn pattern_cov(dst: &mut Canvas, cov: &[f32], tile: &Canvas, offset: (i64, i64), opacity: f32) {
    let mut src = Canvas::new(dst.width, dst.height);
    for y in 0..dst.height {
        for x in 0..dst.width {
            let tx = (x as i64 - offset.0).rem_euclid(tile.width as i64) as u32;
            let ty = (y as i64 - offset.1).rem_euclid(tile.height as i64) as u32;
            src.set(x, y, tile.get(tx, ty));
        }
    }
    composite(
        dst,
        &src,
        BlendMode::Normal,
        opacity,
        &Coverage::Buffer(cov),
        0,
    );
}

/// Coverage of a filled shape, used by bucket fill with an explicit region and by
/// `layer.add` for shape layers.
pub fn shape_cov(
    path: &BezPath,
    w: u32,
    h: u32,
    scale: f64,
    rule: dpaint_core::doc::common::FillRule,
) -> Result<Vec<f32>> {
    let sk = geom::to_sk(path, dpaint_core::kurbo::Affine::scale(scale))
        .ok_or_else(|| Error::DegenerateGeometry("path is empty".into()))?;
    Ok(geom::fill_coverage(&sk, w, h, rule))
}

#[cfg(test)]
mod tests {
    use super::*;
    use dpaint_core::color::linear_to_srgb;

    #[test]
    fn linear_gradient_interpolates_in_light_not_in_bytes() {
        let stops = vec![
            GradientStop {
                offset: 0.0,
                color: Color::BLACK,
            },
            GradientStop {
                offset: 1.0,
                color: Color::WHITE,
            },
        ];
        let g = gradient_canvas(
            101,
            1,
            GradientKind::Linear,
            [0.0, 0.0],
            [101.0, 0.0],
            &stops,
            1.0,
        );
        let mid = g.get(50, 0);
        assert!(
            (mid[0] - 0.5).abs() < 0.02,
            "midpoint should be half the light: {mid:?}"
        );
        // Which shows up as ~0.735 once encoded for display.
        assert!((linear_to_srgb(mid[0]) - 0.735).abs() < 0.02);
    }

    #[test]
    fn brush_stroke_covers_the_path_and_fades_at_the_rim() {
        let mut path = BezPath::new();
        path.move_to((5.0, 10.0));
        path.line_to((35.0, 10.0));
        let brush = Brush {
            size: 8.0,
            hardness: 0.5,
            spacing: 0.2,
            ..Default::default()
        };
        let cov = stroke_coverage(40, 20, &path, &brush, 1.0, None).unwrap();
        let at = |x: usize, y: usize| cov[y * 40 + x];
        assert!(at(20, 10) > 0.99, "center of the stroke must be solid");
        assert!(at(20, 13) > 0.0 && at(20, 13) < at(20, 10), "soft rim");
        assert_eq!(at(20, 17), 0.0, "nothing outside the brush radius");
    }
}
