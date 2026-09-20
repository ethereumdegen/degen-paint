//! Non-destructive layer effects, evaluated on a layer's rendered content every composite.
//!
//! Effects are applied in the order they appear on the layer, so an agent can put a blur
//! before a stroke (soft edge, crisp outline) or after it (both blurred) and get what it
//! asked for.

use crate::blend::{composite, Coverage};
use crate::canvas::Canvas;
use crate::filters::{gaussian_blur, morphology, MorphOp, MorphShape};
use dpaint_core::color::Color;
use dpaint_core::doc::raster::{BlendMode, Effect, StrokeAlign};

fn alpha_of(c: &Canvas) -> Vec<f32> {
    (0..c.pixel_count()).map(|i| c.data[i * 4 + 3]).collect()
}

/// A flat color silhouette with the given coverage — the base of every shadow, glow and stroke.
fn silhouette(w: u32, h: u32, cov: &[f32], color: Color) -> Canvas {
    let s = color.to_linear();
    let mut c = Canvas::new(w, h);
    for i in 0..c.pixel_count() {
        let a = cov[i].clamp(0.0, 1.0) * s[3];
        if a <= 0.0 {
            continue;
        }
        let o = i * 4;
        c.data[o] = s[0] * a;
        c.data[o + 1] = s[1] * a;
        c.data[o + 2] = s[2] * a;
        c.data[o + 3] = a;
    }
    c
}

fn shifted(c: &Canvas, dx: f64, dy: f64) -> Canvas {
    if dx == 0.0 && dy == 0.0 {
        return c.clone();
    }
    let mut out = Canvas::new(c.width, c.height);
    for y in 0..c.height {
        for x in 0..c.width {
            let p = c
                .sample_outside_transparent(x as f32 + 0.5 - dx as f32, y as f32 + 0.5 - dy as f32);
            out.set(x, y, p);
        }
    }
    out
}

fn blur_cov(w: u32, h: u32, cov: &[f32], sigma: f32) -> Vec<f32> {
    if sigma <= 0.0 {
        return cov.to_vec();
    }
    let mut c = Canvas::new(w, h);
    for i in 0..cov.len() {
        c.data[i * 4 + 3] = cov[i];
    }
    let b = gaussian_blur(&c, sigma);
    alpha_of(&b)
}

/// Apply a layer's effect stack to its rendered content, returning the decorated content.
pub fn apply(content: &Canvas, effects: &[Effect], scale: f64) -> Canvas {
    let (w, h) = (content.width, content.height);
    let mut cur = content.clone();
    for effect in effects {
        match effect {
            Effect::Blur { radius } => {
                cur = gaussian_blur(&cur, (radius * scale / 2.0).max(0.0) as f32);
            }
            Effect::DropShadow {
                dx,
                dy,
                blur,
                color,
            } => {
                let cov = blur_cov(w, h, &alpha_of(&cur), (blur * scale / 2.0) as f32);
                let shadow = shifted(&silhouette(w, h, &cov, *color), dx * scale, dy * scale);
                // Shadow goes *under* the content: composite content over the shadow.
                let mut base = shadow;
                composite(&mut base, &cur, BlendMode::Normal, 1.0, &Coverage::Full, 0);
                cur = base;
            }
            Effect::OuterGlow { blur, color } => {
                let a = alpha_of(&cur);
                let cov = blur_cov(w, h, &a, (blur * scale / 2.0).max(0.5) as f32);
                // Only the part that spills outside the shape glows.
                let outside: Vec<f32> = cov
                    .iter()
                    .zip(&a)
                    .map(|(g, s)| (g * (1.0 - s)).clamp(0.0, 1.0))
                    .collect();
                let mut base = silhouette(w, h, &outside, *color);
                composite(&mut base, &cur, BlendMode::Normal, 1.0, &Coverage::Full, 0);
                cur = base;
            }
            Effect::InnerShadow {
                dx,
                dy,
                blur,
                color,
            } => {
                let a = alpha_of(&cur);
                // Shadow of the *hole*: invert coverage, offset, blur, clip to the shape.
                let inverse: Vec<f32> = a.iter().map(|v| 1.0 - v).collect();
                let mut inv_canvas = Canvas::new(w, h);
                for (i, v) in inverse.iter().enumerate() {
                    inv_canvas.data[i * 4 + 3] = *v;
                }
                let moved = shifted(&inv_canvas, dx * scale, dy * scale);
                let cov = blur_cov(w, h, &alpha_of(&moved), (blur * scale / 2.0) as f32);
                let inner: Vec<f32> = cov
                    .iter()
                    .zip(&a)
                    .map(|(g, s)| (g * s).clamp(0.0, 1.0))
                    .collect();
                let shadow = silhouette(w, h, &inner, *color);
                composite(
                    &mut cur,
                    &shadow,
                    BlendMode::Normal,
                    1.0,
                    &Coverage::Full,
                    0,
                );
            }
            Effect::Stroke {
                width,
                color,
                align,
            } => {
                let a = alpha_of(&cur);
                let mut src = Canvas::new(w, h);
                for (i, v) in a.iter().enumerate() {
                    src.data[i * 4 + 3] = *v;
                }
                let px = (width * scale).max(1.0);
                let band = match align {
                    StrokeAlign::Outside => {
                        let grown = alpha_of(&morphology(
                            &src,
                            MorphOp::Dilate,
                            px.round() as u32,
                            MorphShape::Disk,
                        ));
                        grown
                            .iter()
                            .zip(&a)
                            .map(|(g, s)| (g - s).max(0.0))
                            .collect::<Vec<f32>>()
                    }
                    StrokeAlign::Inside => {
                        let shrunk = alpha_of(&morphology(
                            &src,
                            MorphOp::Erode,
                            px.round() as u32,
                            MorphShape::Disk,
                        ));
                        a.iter()
                            .zip(&shrunk)
                            .map(|(s, e)| (s - e).max(0.0))
                            .collect::<Vec<f32>>()
                    }
                    StrokeAlign::Center => {
                        let r = (px / 2.0).round().max(1.0) as u32;
                        let grown =
                            alpha_of(&morphology(&src, MorphOp::Dilate, r, MorphShape::Disk));
                        let shrunk =
                            alpha_of(&morphology(&src, MorphOp::Erode, r, MorphShape::Disk));
                        grown
                            .iter()
                            .zip(&shrunk)
                            .map(|(g, e)| (g - e).max(0.0))
                            .collect::<Vec<f32>>()
                    }
                };
                let stroke = silhouette(w, h, &band, *color);
                composite(
                    &mut cur,
                    &stroke,
                    BlendMode::Normal,
                    1.0,
                    &Coverage::Full,
                    0,
                );
            }
        }
    }
    cur
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square(w: u32, h: u32) -> Canvas {
        let mut c = Canvas::new(w, h);
        for y in h / 4..h * 3 / 4 {
            for x in w / 4..w * 3 / 4 {
                c.set(x, y, [1.0, 1.0, 1.0, 1.0]);
            }
        }
        c
    }

    #[test]
    fn drop_shadow_adds_coverage_down_right_and_leaves_the_shape_lit() {
        let c = square(40, 40);
        let out = apply(
            &c,
            &[Effect::DropShadow {
                dx: 6.0,
                dy: 6.0,
                blur: 4.0,
                color: Color::BLACK,
            }],
            1.0,
        );
        // Just outside the bottom-right corner was empty, now it is shadowed.
        assert_eq!(c.get(33, 33)[3], 0.0);
        assert!(
            out.get(33, 33)[3] > 0.2,
            "shadow missing: {:?}",
            out.get(33, 33)
        );
        // The shape itself is still white, not darkened.
        assert!(out.get(20, 20)[0] > 0.99);
        // And nothing leaked up-left.
        assert_eq!(out.get(4, 4)[3], 0.0);
    }

    #[test]
    fn outside_stroke_rings_the_shape_without_covering_it() {
        let c = square(40, 40);
        let out = apply(
            &c,
            &[Effect::Stroke {
                width: 2.0,
                color: Color::rgba(1.0, 0.0, 0.0, 1.0),
                align: StrokeAlign::Outside,
            }],
            1.0,
        );
        let ring = out.get(9, 20);
        assert!(
            ring[3] > 0.5 && ring[0] > ring[1],
            "red ring outside the shape: {ring:?}"
        );
        assert!(out.get(20, 20)[1] > 0.9, "interior stays white");
    }

    #[test]
    fn effect_blur_softens_the_edge() {
        let c = square(40, 40);
        let out = apply(&c, &[Effect::Blur { radius: 6.0 }], 1.0);
        let a = out.get(10, 20)[3];
        assert!(a > 0.0 && a < 1.0, "edge should be partially covered: {a}");
    }
}
