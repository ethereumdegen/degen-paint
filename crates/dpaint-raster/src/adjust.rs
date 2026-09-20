//! Every `Adjustment` variant, applied to a linear-light canvas.
//!
//! Tone controls (curves, levels, brightness/contrast, posterize, threshold) are defined on
//! **display-encoded** values, because that is where a user's mental model lives: pulling the
//! curve midpoint to 0.75 should look like "three quarters bright". Exposure is the exception
//! — it is a physical stop change, so it multiplies linear light directly.

use crate::canvas::Canvas;
use dpaint_core::color::{linear_to_srgb, srgb_to_linear};
use dpaint_core::doc::raster::{Adjustment, Channel, DesaturateMode};
use dpaint_core::{AssetStore, Error, Result};

/// Monotone cubic interpolation (Fritsch–Carlson), so a curve through user points never
/// overshoots into a halo the way a natural cubic spline does.
#[derive(Debug, Clone)]
pub struct Curve {
    xs: Vec<f64>,
    ys: Vec<f64>,
    m: Vec<f64>,
}

impl Curve {
    pub fn new(points: &[[f64; 2]]) -> Result<Self> {
        let mut pts: Vec<[f64; 2]> = points.to_vec();
        pts.sort_by(|a, b| a[0].partial_cmp(&b[0]).unwrap_or(std::cmp::Ordering::Equal));
        pts.dedup_by(|a, b| (a[0] - b[0]).abs() < 1e-9);
        if pts.len() < 2 {
            return Err(Error::Invalid(
                "curves needs at least two control points with distinct x".into(),
            ));
        }
        let xs: Vec<f64> = pts.iter().map(|p| p[0]).collect();
        let ys: Vec<f64> = pts.iter().map(|p| p[1]).collect();
        let n = xs.len();
        let mut d = vec![0.0; n - 1];
        for i in 0..n - 1 {
            d[i] = (ys[i + 1] - ys[i]) / (xs[i + 1] - xs[i]);
        }
        let mut m = vec![0.0; n];
        m[0] = d[0];
        m[n - 1] = d[n - 2];
        for i in 1..n - 1 {
            m[i] = if d[i - 1] * d[i] <= 0.0 {
                0.0
            } else {
                (d[i - 1] + d[i]) / 2.0
            };
        }
        // Fritsch–Carlson limiter: keeps the interpolant monotone between samples.
        for i in 0..n - 1 {
            if d[i].abs() < 1e-12 {
                m[i] = 0.0;
                m[i + 1] = 0.0;
                continue;
            }
            let a = m[i] / d[i];
            let b = m[i + 1] / d[i];
            let s = a * a + b * b;
            if s > 9.0 {
                let t = 3.0 / s.sqrt();
                m[i] = t * a * d[i];
                m[i + 1] = t * b * d[i];
            }
        }
        Ok(Curve { xs, ys, m })
    }

    pub fn eval(&self, x: f64) -> f64 {
        let n = self.xs.len();
        if x <= self.xs[0] {
            return self.ys[0];
        }
        if x >= self.xs[n - 1] {
            return self.ys[n - 1];
        }
        let mut i = 0;
        while i + 1 < n && self.xs[i + 1] < x {
            i += 1;
        }
        let h = self.xs[i + 1] - self.xs[i];
        let t = (x - self.xs[i]) / h;
        let t2 = t * t;
        let t3 = t2 * t;
        let h00 = 2.0 * t3 - 3.0 * t2 + 1.0;
        let h10 = t3 - 2.0 * t2 + t;
        let h01 = -2.0 * t3 + 3.0 * t2;
        let h11 = t3 - t2;
        h00 * self.ys[i] + h10 * h * self.m[i] + h01 * self.ys[i + 1] + h11 * h * self.m[i + 1]
    }
}

/// Precomputed adjustment: built once, then applied to every pixel (and reused by the
/// adjustment-layer path in the compositor).
pub enum Prepared {
    /// Per-channel transfer curve on display-encoded values.
    Transfer {
        lut: [Vec<f32>; 4],
        channel: Channel,
    },
    /// Multiply linear light, then offset.
    Exposure {
        gain: f32,
        offset: f32,
    },
    Matrix([[f32; 3]; 3]),
    Hsl {
        hue: f32,
        sat: f32,
        light: f32,
    },
    Balance {
        shadows: [f32; 3],
        midtones: [f32; 3],
        highlights: [f32; 3],
    },
    Threshold(f32),
    Desaturate(DesaturateMode),
    Cube {
        n: usize,
        data: Vec<f32>,
        amount: f32,
    },
}

const LUT_N: usize = 1024;

fn transfer(channel: Channel, f: impl Fn(f64) -> f64) -> Prepared {
    let mut lut = [Vec::new(), Vec::new(), Vec::new(), Vec::new()];
    let identity: Vec<f32> = (0..LUT_N).map(|i| i as f32 / (LUT_N - 1) as f32).collect();
    let mapped: Vec<f32> = (0..LUT_N)
        .map(|i| f(i as f64 / (LUT_N - 1) as f64).clamp(0.0, 1.0) as f32)
        .collect();
    for (c, slot) in lut.iter_mut().enumerate() {
        let hit = match channel {
            Channel::Rgb => c < 3,
            Channel::Red => c == 0,
            Channel::Green => c == 1,
            Channel::Blue => c == 2,
            Channel::Alpha => c == 3,
        };
        *slot = if hit {
            mapped.clone()
        } else {
            identity.clone()
        };
    }
    Prepared::Transfer { lut, channel }
}

pub fn prepare(adj: &Adjustment, assets: &AssetStore) -> Result<Prepared> {
    Ok(match adj {
        Adjustment::Curves { channel, points } => {
            let c = Curve::new(points)?;
            transfer(*channel, move |v| c.eval(v))
        }
        Adjustment::Levels {
            channel,
            in_black,
            in_white,
            gamma,
            out_black,
            out_white,
        } => {
            if in_white <= in_black {
                return Err(Error::Invalid("levels needs in_white > in_black".into()));
            }
            if *gamma <= 0.0 {
                return Err(Error::Invalid("levels gamma must be positive".into()));
            }
            let (ib, iw, g, ob, ow) = (*in_black, *in_white, *gamma, *out_black, *out_white);
            transfer(*channel, move |v| {
                let t = ((v - ib) / (iw - ib)).clamp(0.0, 1.0);
                ob + (ow - ob) * t.powf(1.0 / g)
            })
        }
        Adjustment::BrightnessContrast {
            brightness,
            contrast,
        } => {
            let (b, k) = (*brightness, 1.0 + contrast.clamp(-1.0, 8.0));
            transfer(Channel::Rgb, move |v| (v - 0.5) * k + 0.5 + b)
        }
        Adjustment::Posterize { levels } => {
            let n = (*levels).max(2) as f64;
            transfer(Channel::Rgb, move |v| (v * (n - 1.0)).round() / (n - 1.0))
        }
        Adjustment::Invert => transfer(Channel::Rgb, |v| 1.0 - v),
        Adjustment::Exposure { stops, offset } => Prepared::Exposure {
            gain: 2f32.powf(*stops as f32),
            offset: *offset as f32,
        },
        Adjustment::ChannelMixer { matrix } => {
            let mut m = [[0.0f32; 3]; 3];
            for r in 0..3 {
                for c in 0..3 {
                    m[r][c] = matrix[r][c] as f32;
                }
            }
            Prepared::Matrix(m)
        }
        Adjustment::Hsl {
            hue,
            saturation,
            lightness,
        } => Prepared::Hsl {
            hue: *hue as f32,
            sat: saturation.clamp(-1.0, 1.0) as f32,
            light: lightness.clamp(-1.0, 1.0) as f32,
        },
        Adjustment::ColorBalance {
            shadows,
            midtones,
            highlights,
        } => Prepared::Balance {
            shadows: f3(shadows),
            midtones: f3(midtones),
            highlights: f3(highlights),
        },
        Adjustment::Threshold { level } => Prepared::Threshold(level.clamp(0.0, 1.0) as f32),
        Adjustment::Desaturate { mode } => Prepared::Desaturate(*mode),
        Adjustment::Lut { asset, amount } => {
            let bytes = assets.get(asset)?;
            let (n, data) = load_hald(&bytes)?;
            Prepared::Cube {
                n,
                data,
                amount: amount.clamp(0.0, 1.0) as f32,
            }
        }
    })
}

fn f3(v: &[f64; 3]) -> [f32; 3] {
    [
        v[0].clamp(-1.0, 1.0) as f32,
        v[1].clamp(-1.0, 1.0) as f32,
        v[2].clamp(-1.0, 1.0) as f32,
    ]
}

/// Decode a HALD-style 2D color lookup PNG: a square image holding an `n x n x n` cube laid
/// out as `n` slices of `n x n`, blue varying slowest. `n` is derived from the side length.
fn load_hald(bytes: &[u8]) -> Result<(usize, Vec<f32>)> {
    let img = image::load_from_memory_with_format(bytes, image::ImageFormat::Png)
        .map_err(|e| Error::AssetDecode(e.to_string()))?
        .to_rgb8();
    let (w, h) = img.dimensions();
    if w != h {
        return Err(Error::AssetDecode(format!(
            "a HALD LUT must be square, got {w}x{h}"
        )));
    }
    let total = (w as usize) * (h as usize);
    // side^2 == n^3, so n = cbrt(side^2).
    let n = (total as f64).cbrt().round() as usize;
    if n < 2 || n * n * n != total {
        return Err(Error::AssetDecode(format!(
            "{w}x{h} is not a HALD cube: side^2 must be a perfect cube"
        )));
    }
    let mut data = vec![0.0f32; total * 3];
    for (i, px) in img.pixels().enumerate() {
        // The cube is stored in display encoding; convert to linear for interpolation.
        data[i * 3] = srgb_to_linear(px.0[0] as f32 / 255.0);
        data[i * 3 + 1] = srgb_to_linear(px.0[1] as f32 / 255.0);
        data[i * 3 + 2] = srgb_to_linear(px.0[2] as f32 / 255.0);
    }
    Ok((n, data))
}

#[inline]
fn lut_lookup(lut: &[f32], v: f32) -> f32 {
    let t = v.clamp(0.0, 1.0) * (LUT_N - 1) as f32;
    let i = t.floor() as usize;
    let f = t - i as f32;
    let a = lut[i];
    let b = lut[(i + 1).min(LUT_N - 1)];
    a + (b - a) * f
}

fn rgb_to_hsl(c: [f32; 3]) -> [f32; 3] {
    let max = c[0].max(c[1]).max(c[2]);
    let min = c[0].min(c[1]).min(c[2]);
    let l = (max + min) / 2.0;
    if (max - min).abs() < 1e-9 {
        return [0.0, 0.0, l];
    }
    let d = max - min;
    let s = if l > 0.5 {
        d / (2.0 - max - min)
    } else {
        d / (max + min)
    };
    let h = if max == c[0] {
        ((c[1] - c[2]) / d).rem_euclid(6.0)
    } else if max == c[1] {
        (c[2] - c[0]) / d + 2.0
    } else {
        (c[0] - c[1]) / d + 4.0
    };
    [h * 60.0, s, l]
}

fn hsl_to_rgb(hsl: [f32; 3]) -> [f32; 3] {
    let (h, s, l) = (
        hsl[0].rem_euclid(360.0) / 60.0,
        hsl[1].clamp(0.0, 1.0),
        hsl[2].clamp(0.0, 1.0),
    );
    if s <= 0.0 {
        return [l, l, l];
    }
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - (h % 2.0 - 1.0).abs());
    let m = l - c / 2.0;
    let (r, g, b) = match h as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    [r + m, g + m, b + m]
}

#[inline]
fn sample_cube(n: usize, data: &[f32], c: [f32; 3]) -> [f32; 3] {
    let fetch = |ri: usize, gi: usize, bi: usize| -> [f32; 3] {
        let idx = (ri + gi * n + bi * n * n) * 3;
        [data[idx], data[idx + 1], data[idx + 2]]
    };
    let scale = (n - 1) as f32;
    let f = [
        c[0].clamp(0.0, 1.0) * scale,
        c[1].clamp(0.0, 1.0) * scale,
        c[2].clamp(0.0, 1.0) * scale,
    ];
    let i0 = [
        f[0].floor() as usize,
        f[1].floor() as usize,
        f[2].floor() as usize,
    ];
    let i1 = [
        (i0[0] + 1).min(n - 1),
        (i0[1] + 1).min(n - 1),
        (i0[2] + 1).min(n - 1),
    ];
    let t = [
        f[0] - i0[0] as f32,
        f[1] - i0[1] as f32,
        f[2] - i0[2] as f32,
    ];
    let mut out = [0.0f32; 3];
    for (bi, bw) in [(i0[2], 1.0 - t[2]), (i1[2], t[2])] {
        if bw <= 0.0 {
            continue;
        }
        for (gi, gw) in [(i0[1], 1.0 - t[1]), (i1[1], t[1])] {
            if gw <= 0.0 {
                continue;
            }
            for (ri, rw) in [(i0[0], 1.0 - t[0]), (i1[0], t[0])] {
                if rw <= 0.0 {
                    continue;
                }
                let s = fetch(ri, gi, bi);
                let w = bw * gw * rw;
                out[0] += s[0] * w;
                out[1] += s[1] * w;
                out[2] += s[2] * w;
            }
        }
    }
    out
}

/// Apply a prepared adjustment to one straight linear-light RGBA pixel.
pub fn apply_pixel(p: &Prepared, mut c: [f32; 4]) -> [f32; 4] {
    match p {
        Prepared::Transfer { lut, channel } => {
            if *channel == Channel::Alpha {
                c[3] = lut_lookup(&lut[3], c[3]);
                return c;
            }
            for ch in 0..3 {
                let d = linear_to_srgb(c[ch].clamp(0.0, 1.0));
                c[ch] = srgb_to_linear(lut_lookup(&lut[ch], d));
            }
        }
        Prepared::Exposure { gain, offset } => {
            for v in c.iter_mut().take(3) {
                *v = (*v * gain + offset).clamp(0.0, 1.0);
            }
        }
        Prepared::Matrix(m) => {
            let s = [c[0], c[1], c[2]];
            for ch in 0..3 {
                c[ch] = (m[ch][0] * s[0] + m[ch][1] * s[1] + m[ch][2] * s[2]).clamp(0.0, 1.0);
            }
        }
        Prepared::Hsl { hue, sat, light } => {
            let d = [
                linear_to_srgb(c[0].clamp(0.0, 1.0)),
                linear_to_srgb(c[1].clamp(0.0, 1.0)),
                linear_to_srgb(c[2].clamp(0.0, 1.0)),
            ];
            let mut hsl = rgb_to_hsl(d);
            hsl[0] += hue;
            hsl[1] = if *sat >= 0.0 {
                hsl[1] + (1.0 - hsl[1]) * sat
            } else {
                hsl[1] * (1.0 + sat)
            };
            hsl[2] = if *light >= 0.0 {
                hsl[2] + (1.0 - hsl[2]) * light
            } else {
                hsl[2] * (1.0 + light)
            };
            let rgb = hsl_to_rgb(hsl);
            for ch in 0..3 {
                c[ch] = srgb_to_linear(rgb[ch].clamp(0.0, 1.0));
            }
        }
        Prepared::Balance {
            shadows,
            midtones,
            highlights,
        } => {
            let d = [
                linear_to_srgb(c[0].clamp(0.0, 1.0)),
                linear_to_srgb(c[1].clamp(0.0, 1.0)),
                linear_to_srgb(c[2].clamp(0.0, 1.0)),
            ];
            let v = 0.299 * d[0] + 0.587 * d[1] + 0.114 * d[2];
            let ws = (1.0 - 2.0 * v).max(0.0);
            let wh = (2.0 * v - 1.0).max(0.0);
            let wm = 1.0 - ws - wh;
            for ch in 0..3 {
                let shift = shadows[ch] * ws + midtones[ch] * wm + highlights[ch] * wh;
                c[ch] = srgb_to_linear((d[ch] + shift).clamp(0.0, 1.0));
            }
        }
        Prepared::Threshold(level) => {
            let y = linear_to_srgb((0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]).clamp(0.0, 1.0));
            let v = if y >= *level { 1.0 } else { 0.0 };
            c[0] = v;
            c[1] = v;
            c[2] = v;
        }
        Prepared::Desaturate(mode) => {
            let d = [
                linear_to_srgb(c[0].clamp(0.0, 1.0)),
                linear_to_srgb(c[1].clamp(0.0, 1.0)),
                linear_to_srgb(c[2].clamp(0.0, 1.0)),
            ];
            let g = match mode {
                DesaturateMode::Luminosity => 0.2126 * d[0] + 0.7152 * d[1] + 0.0722 * d[2],
                DesaturateMode::Average => (d[0] + d[1] + d[2]) / 3.0,
                DesaturateMode::Lightness => {
                    (d[0].max(d[1]).max(d[2]) + d[0].min(d[1]).min(d[2])) / 2.0
                }
            };
            let l = srgb_to_linear(g.clamp(0.0, 1.0));
            c[0] = l;
            c[1] = l;
            c[2] = l;
        }
        Prepared::Cube { n, data, amount } => {
            let s = sample_cube(*n, data, [c[0], c[1], c[2]]);
            for ch in 0..3 {
                c[ch] = c[ch] + (s[ch] - c[ch]) * amount;
            }
        }
    }
    c
}

/// Apply an adjustment across a whole canvas, straight-color in and out so premultiplied
/// storage never leaks into the tone math.
pub fn apply_prepared(canvas: &mut Canvas, p: &Prepared) {
    for i in (0..canvas.data.len()).step_by(4) {
        if canvas.data[i + 3] <= 0.0 {
            continue;
        }
        let s = canvas.straight(i);
        let out = apply_pixel(p, s);
        canvas.set_straight(i, out);
    }
}

pub fn apply(canvas: &mut Canvas, adj: &Adjustment, assets: &AssetStore) -> Result<()> {
    let p = prepare(adj, assets)?;
    apply_prepared(canvas, &p);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monotone_curve_never_overshoots_its_control_points() {
        let c = Curve::new(&[[0.0, 0.0], [0.5, 0.9], [1.0, 1.0]]).unwrap();
        let mut prev = -1.0;
        for i in 0..=100 {
            let x = i as f64 / 100.0;
            let y = c.eval(x);
            assert!((0.0..=1.0).contains(&y), "overshoot at {x}: {y}");
            assert!(y >= prev - 1e-9, "not monotone at {x}");
            prev = y;
        }
        assert!((c.eval(0.5) - 0.9).abs() < 1e-9);
    }

    #[test]
    fn levels_gamma_brightens_midtones_without_moving_endpoints() {
        let adj = Adjustment::Levels {
            channel: Channel::Rgb,
            in_black: 0.0,
            in_white: 1.0,
            gamma: 2.0,
            out_black: 0.0,
            out_white: 1.0,
        };
        let store = AssetStore::new(std::env::temp_dir());
        let p = prepare(&adj, &store).unwrap();
        let mid = apply_pixel(&p, [srgb_to_linear(0.5); 4]);
        assert!(
            linear_to_srgb(mid[0]) > 0.6,
            "gamma 2 must lift 0.5 toward 0.707"
        );
        let white = apply_pixel(&p, [1.0, 1.0, 1.0, 1.0]);
        assert!((white[0] - 1.0).abs() < 1e-3);
    }
}
