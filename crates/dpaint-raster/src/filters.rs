//! Pixel filters. Everything runs on premultiplied linear light, which is what makes a blur
//! of a hard edge stay the same average brightness instead of growing a bright rim.
//!
//! Parallelism is per destination row: rows are disjoint and each reads an immutable
//! source, so results are bit-identical regardless of thread count — or of whether
//! threads exist at all. The `parallel` feature is on natively and off for `wasm32`,
//! where there is no thread to spawn; the only difference is how long a blur takes.

use crate::blend::hash01;
use crate::canvas::Canvas;
use dpaint_core::color::{linear_to_srgb, srgb_to_linear};
use dpaint_core::{Error, Result};
#[cfg(feature = "parallel")]
use rayon::prelude::*;

/// The disjoint destination rows of a canvas, as a parallel iterator where rayon is
/// available and a plain one where it is not.
#[cfg(feature = "parallel")]
#[inline]
fn rows_mut(data: &mut [f32], stride: usize) -> rayon::slice::ChunksMut<'_, f32> {
    data.par_chunks_mut(stride)
}

#[cfg(not(feature = "parallel"))]
#[inline]
fn rows_mut(data: &mut [f32], stride: usize) -> std::slice::ChunksMut<'_, f32> {
    data.chunks_mut(stride)
}

#[inline]
fn clamp_premul(px: &mut [f32]) {
    let a = px[3].clamp(0.0, 1.0);
    px[3] = a;
    for c in 0..3 {
        px[c] = px[c].clamp(0.0, a);
    }
}

pub fn gauss_kernel(sigma: f32) -> Vec<f32> {
    let sigma = sigma.max(1e-3);
    let r = (sigma * 3.0).ceil().max(1.0) as i32;
    let mut k = Vec::with_capacity((2 * r + 1) as usize);
    let two_s2 = 2.0 * sigma * sigma;
    for i in -r..=r {
        k.push((-(i * i) as f32 / two_s2).exp());
    }
    let sum: f32 = k.iter().sum();
    for v in k.iter_mut() {
        *v /= sum;
    }
    k
}

/// One separable pass of a normalized 1D kernel, with edge clamping.
fn pass(src: &Canvas, k: &[f32], horizontal: bool) -> Canvas {
    let (w, h) = (src.width, src.height);
    let r = (k.len() / 2) as i64;
    let mut out = Canvas::new(w, h);
    let stride = w as usize * 4;
    rows_mut(&mut out.data, stride)
        .enumerate()
        .for_each(|(y, row)| {
            let y = y as i64;
            for x in 0..w as i64 {
                let mut acc = [0.0f32; 4];
                for (ki, kv) in k.iter().enumerate() {
                    let d = ki as i64 - r;
                    let p = if horizontal {
                        src.clamped(x + d, y)
                    } else {
                        src.clamped(x, y + d)
                    };
                    acc[0] += p[0] * kv;
                    acc[1] += p[1] * kv;
                    acc[2] += p[2] * kv;
                    acc[3] += p[3] * kv;
                }
                let o = x as usize * 4;
                clamp_premul(&mut acc);
                row[o..o + 4].copy_from_slice(&acc);
            }
        });
    out
}

/// Separable gaussian blur. `sigma` is in pixels; the kernel spans ±3σ, which captures
/// 99.7% of the weight, so the visual radius matches what a user asks for.
pub fn gaussian_blur(src: &Canvas, sigma: f32) -> Canvas {
    if sigma <= 0.0 {
        return src.clone();
    }
    let k = gauss_kernel(sigma);
    let tmp = pass(src, &k, true);
    pass(&tmp, &k, false)
}

pub fn box_blur(src: &Canvas, radius: u32, iterations: u32) -> Canvas {
    if radius == 0 || iterations == 0 {
        return src.clone();
    }
    let n = (2 * radius + 1) as f32;
    let k = vec![1.0 / n; (2 * radius + 1) as usize];
    let mut cur = src.clone();
    for _ in 0..iterations.min(8) {
        let tmp = pass(&cur, &k, true);
        cur = pass(&tmp, &k, false);
    }
    cur
}

/// Directional blur: average of samples along a line of `distance` px at `angle` degrees.
pub fn motion_blur(src: &Canvas, distance: f32, angle_deg: f32) -> Canvas {
    if distance <= 0.0 {
        return src.clone();
    }
    let (sin, cos) = angle_deg.to_radians().sin_cos();
    let steps = distance.ceil().max(1.0) as i32;
    let mut out = Canvas::new(src.width, src.height);
    let stride = src.width as usize * 4;
    rows_mut(&mut out.data, stride)
        .enumerate()
        .for_each(|(y, row)| {
            for x in 0..src.width {
                let mut acc = [0.0f32; 4];
                let mut n = 0.0f32;
                for s in -steps..=steps {
                    let t = s as f32 / steps as f32 * distance * 0.5;
                    let sx = x as f32 + 0.5 + cos * t;
                    let sy = y as f32 + 0.5 + sin * t;
                    let p = src.sample(sx, sy);
                    for c in 0..4 {
                        acc[c] += p[c];
                    }
                    n += 1.0;
                }
                for c in 0..4 {
                    acc[c] /= n;
                }
                clamp_premul(&mut acc);
                let o = x as usize * 4;
                row[o..o + 4].copy_from_slice(&acc);
            }
        });
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum RadialMode {
    /// Streaks radiate out from the center.
    Zoom,
    /// Streaks rotate around the center.
    Spin,
}

pub fn radial_blur(src: &Canvas, mode: RadialMode, amount: f32, center: [f32; 2]) -> Canvas {
    if amount <= 0.0 {
        return src.clone();
    }
    let steps = 12i32;
    let mut out = Canvas::new(src.width, src.height);
    let stride = src.width as usize * 4;
    rows_mut(&mut out.data, stride)
        .enumerate()
        .for_each(|(y, row)| {
            for x in 0..src.width {
                let px = x as f32 + 0.5 - center[0];
                let py = y as f32 + 0.5 - center[1];
                let mut acc = [0.0f32; 4];
                for s in 0..=steps {
                    let t = s as f32 / steps as f32 - 0.5;
                    let (sx, sy) = match mode {
                        RadialMode::Zoom => {
                            let k = 1.0 + t * amount * 0.02;
                            (center[0] + px * k, center[1] + py * k)
                        }
                        RadialMode::Spin => {
                            let a = t * amount.to_radians();
                            let (s, c) = a.sin_cos();
                            (center[0] + px * c - py * s, center[1] + px * s + py * c)
                        }
                    };
                    let p = src.sample(sx, sy);
                    for c in 0..4 {
                        acc[c] += p[c];
                    }
                }
                for c in 0..4 {
                    acc[c] /= (steps + 1) as f32;
                }
                clamp_premul(&mut acc);
                let o = x as usize * 4;
                row[o..o + 4].copy_from_slice(&acc);
            }
        });
    out
}

/// Unsharp mask: `src + amount * (src - blur(src))`, skipping differences below `threshold`
/// so flat areas (and their noise) are left alone.
pub fn unsharp(src: &Canvas, sigma: f32, amount: f32, threshold: f32) -> Canvas {
    let blurred = gaussian_blur(src, sigma);
    let mut out = src.clone();
    for i in (0..out.data.len()).step_by(4) {
        let s = src.straight(i);
        let b = blurred.straight(i);
        let mut c = s;
        for ch in 0..3 {
            let d = s[ch] - b[ch];
            if d.abs() >= threshold {
                c[ch] = (s[ch] + d * amount).clamp(0.0, 1.0);
            }
        }
        out.set_straight(i, c);
    }
    out
}

pub fn sharpen(src: &Canvas, amount: f32) -> Canvas {
    let a = amount.max(0.0);
    let k = [0.0, -a, 0.0, -a, 1.0 + 4.0 * a, -a, 0.0, -a, 0.0];
    convolve(src, 3, 3, &k, 1.0, 0.0).expect("3x3 kernel is valid")
}

/// Arbitrary kernel convolution on premultiplied linear light.
pub fn convolve(
    src: &Canvas,
    kw: usize,
    kh: usize,
    kernel: &[f32],
    divisor: f32,
    bias: f32,
) -> Result<Canvas> {
    if kw == 0 || kh == 0 || kw % 2 == 0 || kh % 2 == 0 {
        return Err(Error::Invalid(
            "convolution kernel must have odd width and height".into(),
        ));
    }
    if kernel.len() != kw * kh {
        return Err(Error::Invalid(format!(
            "kernel has {} values but {kw}x{kh} needs {}",
            kernel.len(),
            kw * kh
        )));
    }
    let div = if divisor.abs() < 1e-9 {
        let s: f32 = kernel.iter().sum();
        if s.abs() < 1e-9 {
            1.0
        } else {
            s
        }
    } else {
        divisor
    };
    let (rx, ry) = ((kw / 2) as i64, (kh / 2) as i64);
    let mut out = Canvas::new(src.width, src.height);
    let stride = src.width as usize * 4;
    rows_mut(&mut out.data, stride)
        .enumerate()
        .for_each(|(y, row)| {
            let y = y as i64;
            for x in 0..src.width as i64 {
                let mut acc = [0.0f32; 4];
                for ky in 0..kh as i64 {
                    for kx in 0..kw as i64 {
                        let kv = kernel[(ky as usize) * kw + kx as usize];
                        if kv == 0.0 {
                            continue;
                        }
                        let p = src.clamped(x + kx - rx, y + ky - ry);
                        for c in 0..4 {
                            acc[c] += p[c] * kv;
                        }
                    }
                }
                for c in 0..4 {
                    acc[c] = acc[c] / div + bias;
                }
                clamp_premul(&mut acc);
                let o = x as usize * 4;
                row[o..o + 4].copy_from_slice(&acc);
            }
        });
    Ok(out)
}

pub fn add_noise(src: &Canvas, amount: f32, monochrome: bool, seed: u64) -> Canvas {
    let mut out = src.clone();
    let w = src.width;
    for y in 0..src.height {
        for x in 0..w {
            let i = src.idx(x, y);
            if src.data[i + 3] <= 0.0 {
                continue;
            }
            let s = src.straight(i);
            let mut c = s;
            if monochrome {
                let n = (hash01(x, y, seed) - 0.5) * 2.0 * amount;
                for ch in 0..3 {
                    let d = linear_to_srgb(s[ch].clamp(0.0, 1.0)) + n;
                    c[ch] = srgb_to_linear(d.clamp(0.0, 1.0));
                }
            } else {
                for ch in 0..3 {
                    let n =
                        (hash01(x, y, seed ^ ((ch as u64 + 1) * 0x5DEE_CE66)) - 0.5) * 2.0 * amount;
                    let d = linear_to_srgb(s[ch].clamp(0.0, 1.0)) + n;
                    c[ch] = srgb_to_linear(d.clamp(0.0, 1.0));
                }
            }
            out.set_straight(i, c);
        }
    }
    out
}

/// Edge-preserving median denoise: a pixel adopts the window median only when it is within
/// `threshold` of it, so speckles are removed while real edges survive.
pub fn noise_reduce(src: &Canvas, radius: u32, threshold: f32) -> Canvas {
    if radius == 0 {
        return src.clone();
    }
    let r = radius as i64;
    let mut out = Canvas::new(src.width, src.height);
    let stride = src.width as usize * 4;
    rows_mut(&mut out.data, stride)
        .enumerate()
        .for_each(|(y, row)| {
            let y = y as i64;
            let mut buf: Vec<f32> = Vec::with_capacity(((2 * r + 1) * (2 * r + 1)) as usize);
            for x in 0..src.width as i64 {
                let o = x as usize * 4;
                let orig = src.clamped(x, y);
                let mut res = orig;
                for c in 0..4 {
                    buf.clear();
                    for dy in -r..=r {
                        for dx in -r..=r {
                            buf.push(src.clamped(x + dx, y + dy)[c]);
                        }
                    }
                    buf.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                    let med = buf[buf.len() / 2];
                    if (med - orig[c]).abs() <= threshold {
                        res[c] = med;
                    }
                }
                clamp_premul(&mut res);
                row[o..o + 4].copy_from_slice(&res);
            }
        });
    out
}

pub fn pixelate(src: &Canvas, size: u32) -> Canvas {
    let s = size.max(1);
    if s == 1 {
        return src.clone();
    }
    let mut out = Canvas::new(src.width, src.height);
    let mut by = 0;
    while by < src.height {
        let mut bx = 0;
        while bx < src.width {
            let x1 = (bx + s).min(src.width);
            let y1 = (by + s).min(src.height);
            let mut acc = [0.0f64; 4];
            let mut n = 0.0f64;
            for y in by..y1 {
                for x in bx..x1 {
                    let p = src.get(x, y);
                    for c in 0..4 {
                        acc[c] += p[c] as f64;
                    }
                    n += 1.0;
                }
            }
            let avg = [
                (acc[0] / n) as f32,
                (acc[1] / n) as f32,
                (acc[2] / n) as f32,
                (acc[3] / n) as f32,
            ];
            for y in by..y1 {
                for x in bx..x1 {
                    out.set(x, y, avg);
                }
            }
            bx += s;
        }
        by += s;
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum MorphOp {
    /// Grow coverage: each pixel takes the most opaque neighbour in the window.
    Dilate,
    /// Shrink coverage: each pixel takes the least opaque neighbour in the window.
    Erode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum MorphShape {
    #[default]
    Disk,
    Square,
}

pub fn morphology(src: &Canvas, op: MorphOp, radius: u32, shape: MorphShape) -> Canvas {
    if radius == 0 {
        return src.clone();
    }
    let r = radius as i64;
    let r2 = (r * r) as f32;
    let mut out = Canvas::new(src.width, src.height);
    let stride = src.width as usize * 4;
    rows_mut(&mut out.data, stride)
        .enumerate()
        .for_each(|(y, row)| {
            let y = y as i64;
            for x in 0..src.width as i64 {
                let mut best = src.clamped(x, y);
                for dy in -r..=r {
                    for dx in -r..=r {
                        if shape == MorphShape::Disk && (dx * dx + dy * dy) as f32 > r2 {
                            continue;
                        }
                        let p = src.clamped(x + dx, y + dy);
                        let take = match op {
                            MorphOp::Dilate => p[3] > best[3],
                            MorphOp::Erode => p[3] < best[3],
                        };
                        if take {
                            best = p;
                        }
                    }
                }
                let o = x as usize * 4;
                row[o..o + 4].copy_from_slice(&best);
            }
        });
    out
}

/// Displacement map: the map's red channel drives x, green drives y, 0.5 meaning "no shift".
pub fn displace(src: &Canvas, map: &Canvas, scale_x: f32, scale_y: f32) -> Canvas {
    let mut out = Canvas::new(src.width, src.height);
    let stride = src.width as usize * 4;
    let mx = map.width as f32 / src.width as f32;
    let my = map.height as f32 / src.height as f32;
    rows_mut(&mut out.data, stride)
        .enumerate()
        .for_each(|(y, row)| {
            for x in 0..src.width {
                let m = map.sample((x as f32 + 0.5) * mx, (y as f32 + 0.5) * my);
                let ma = if m[3] > 0.0 { m[3] } else { 1.0 };
                let dx = (m[0] / ma - 0.5) * 2.0 * scale_x;
                let dy = (m[1] / ma - 0.5) * 2.0 * scale_y;
                let p = src.sample(x as f32 + 0.5 + dx, y as f32 + 0.5 + dy);
                let o = x as usize * 4;
                row[o..o + 4].copy_from_slice(&p);
            }
        });
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum ChannelSlot {
    Red,
    Green,
    Blue,
    Alpha,
    /// Luminance of the source pixel, useful as a mask source.
    Luma,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum ChannelVerb {
    /// `dst = src`
    Copy,
    /// Exchange the two channels.
    Swap,
    /// `dst = 1 - dst`
    Invert,
    /// `dst = value`
    Set,
    /// `dst = min(dst, src)`
    Min,
    /// `dst = max(dst, src)`
    Max,
    /// `dst = dst * src`
    Multiply,
}

fn slot_get(c: [f32; 4], s: ChannelSlot) -> f32 {
    match s {
        ChannelSlot::Red => c[0],
        ChannelSlot::Green => c[1],
        ChannelSlot::Blue => c[2],
        ChannelSlot::Alpha => c[3],
        ChannelSlot::Luma => 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2],
    }
}

fn slot_set(c: &mut [f32; 4], s: ChannelSlot, v: f32) {
    let v = v.clamp(0.0, 1.0);
    match s {
        ChannelSlot::Red => c[0] = v,
        ChannelSlot::Green => c[1] = v,
        ChannelSlot::Blue => c[2] = v,
        ChannelSlot::Alpha => c[3] = v,
        ChannelSlot::Luma => {
            c[0] = v;
            c[1] = v;
            c[2] = v;
        }
    }
}

pub fn channel_op(
    src: &Canvas,
    verb: ChannelVerb,
    source: ChannelSlot,
    dest: ChannelSlot,
    value: f32,
) -> Canvas {
    let mut out = src.clone();
    for i in (0..out.data.len()).step_by(4) {
        let s = src.straight(i);
        let mut c = s;
        let sv = slot_get(s, source);
        let dv = slot_get(s, dest);
        match verb {
            ChannelVerb::Copy => slot_set(&mut c, dest, sv),
            ChannelVerb::Swap => {
                slot_set(&mut c, dest, sv);
                slot_set(&mut c, source, dv);
            }
            ChannelVerb::Invert => slot_set(&mut c, dest, 1.0 - dv),
            ChannelVerb::Set => slot_set(&mut c, dest, value),
            ChannelVerb::Min => slot_set(&mut c, dest, dv.min(sv)),
            ChannelVerb::Max => slot_set(&mut c, dest, dv.max(sv)),
            ChannelVerb::Multiply => slot_set(&mut c, dest, dv * sv),
        }
        out.set_straight(i, c);
    }
    out
}

const BAYER2: [[f32; 2]; 2] = [[0.0, 2.0], [3.0, 1.0]];

fn bayer(matrix: u32, x: u32, y: u32) -> f32 {
    // Recursive Bayer construction from the 2x2 base, normalized to 0..1.
    let n = match matrix {
        2 => 1,
        4 => 2,
        _ => 3,
    };
    let mut v = 0.0f32;
    let mut scale = 1.0f32;
    let mut denom = 0.0f32;
    for level in 0..n {
        let shift = (n - 1 - level) as u32;
        let bx = ((x >> shift) & 1) as usize;
        let by = ((y >> shift) & 1) as usize;
        v += BAYER2[by][bx] * scale;
        denom += 3.0 * scale;
        scale /= 4.0;
    }
    if denom <= 0.0 {
        0.5
    } else {
        v / denom
    }
}

/// Ordered dithering to `levels` steps per channel using a Bayer matrix (2, 4 or 8).
pub fn dither(src: &Canvas, levels: u32, matrix: u32) -> Canvas {
    let l = levels.max(2) as f32;
    let mut out = src.clone();
    for y in 0..src.height {
        for x in 0..src.width {
            let i = src.idx(x, y);
            let s = src.straight(i);
            let t = bayer(matrix, x, y);
            let mut c = s;
            for ch in 0..3 {
                let d = linear_to_srgb(s[ch].clamp(0.0, 1.0)) * (l - 1.0);
                let q = (d + t - 0.5).round().clamp(0.0, l - 1.0) / (l - 1.0);
                c[ch] = srgb_to_linear(q);
            }
            out.set_straight(i, c);
        }
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum EdgeKernel {
    Sobel,
    Prewitt,
    Laplace,
}

/// Edge magnitude on luminance, written back as a gray image with the original alpha.
pub fn edge_detect(src: &Canvas, kernel: EdgeKernel, amount: f32) -> Canvas {
    let lum = |p: [f32; 4]| -> f32 {
        let a = if p[3] > 0.0 { p[3] } else { 1.0 };
        (0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2]) / a
    };
    let mut out = src.clone();
    let stride = src.width as usize * 4;
    rows_mut(&mut out.data, stride)
        .enumerate()
        .for_each(|(y, row)| {
            let y = y as i64;
            for x in 0..src.width as i64 {
                let mut n = [[0.0f32; 3]; 3];
                for dy in 0..3i64 {
                    for dx in 0..3i64 {
                        n[dy as usize][dx as usize] = lum(src.clamped(x + dx - 1, y + dy - 1));
                    }
                }
                let mag = match kernel {
                    EdgeKernel::Sobel | EdgeKernel::Prewitt => {
                        let c = if kernel == EdgeKernel::Sobel {
                            2.0
                        } else {
                            1.0
                        };
                        let gx =
                            (n[0][2] + c * n[1][2] + n[2][2]) - (n[0][0] + c * n[1][0] + n[2][0]);
                        let gy =
                            (n[2][0] + c * n[2][1] + n[2][2]) - (n[0][0] + c * n[0][1] + n[0][2]);
                        (gx * gx + gy * gy).sqrt()
                    }
                    EdgeKernel::Laplace => {
                        (4.0 * n[1][1] - n[0][1] - n[1][0] - n[1][2] - n[2][1]).abs()
                    }
                };
                let v = (mag * amount).clamp(0.0, 1.0);
                let a = src.clamped(x, y)[3];
                let o = x as usize * 4;
                row[o..o + 4].copy_from_slice(&[v * a, v * a, v * a, a]);
            }
        });
    out
}

/// Variance of display-encoded luminance — the statistic the blur tests assert on.
pub fn luma_variance(c: &Canvas) -> f32 {
    let mut vals = Vec::with_capacity(c.pixel_count());
    for i in (0..c.data.len()).step_by(4) {
        let s = c.straight(i);
        vals.push(linear_to_srgb(
            (0.2126 * s[0] + 0.7152 * s[1] + 0.0722 * s[2]).clamp(0.0, 1.0),
        ));
    }
    let n = vals.len() as f32;
    let mean = vals.iter().sum::<f32>() / n;
    vals.iter().map(|v| (v - mean) * (v - mean)).sum::<f32>() / n
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checkerboard(w: u32, h: u32) -> Canvas {
        let mut c = Canvas::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let v = if (x / 2 + y / 2) % 2 == 0 { 1.0 } else { 0.0 };
                c.set(x, y, [v, v, v, 1.0]);
            }
        }
        c
    }

    #[test]
    fn gaussian_blur_reduces_variance_and_keeps_mean_light() {
        let src = checkerboard(32, 32);
        let out = gaussian_blur(&src, 2.0);
        assert!(
            luma_variance(&out) < luma_variance(&src) * 0.25,
            "variance {} did not drop from {}",
            luma_variance(&out),
            luma_variance(&src)
        );
        let mean_lin = |c: &Canvas| {
            c.data.chunks_exact(4).map(|p| p[0] as f64).sum::<f64>() / c.pixel_count() as f64
        };
        assert!(
            (mean_lin(&src) - mean_lin(&out)).abs() < 0.01,
            "mean light changed: {} -> {}",
            mean_lin(&src),
            mean_lin(&out)
        );
    }

    #[test]
    fn sharpen_increases_variance() {
        let src = gaussian_blur(&checkerboard(32, 32), 1.5);
        let out = sharpen(&src, 1.0);
        assert!(luma_variance(&out) > luma_variance(&src));
    }

    #[test]
    fn dilate_grows_coverage_and_erode_shrinks_it() {
        let mut c = Canvas::new(21, 21);
        for y in 8..13 {
            for x in 8..13 {
                c.set(x, y, [1.0, 1.0, 1.0, 1.0]);
            }
        }
        let area = |c: &Canvas| c.data.chunks_exact(4).filter(|p| p[3] > 0.5).count();
        let base = area(&c);
        assert!(area(&morphology(&c, MorphOp::Dilate, 2, MorphShape::Disk)) > base);
        assert!(area(&morphology(&c, MorphOp::Erode, 1, MorphShape::Disk)) < base);
    }
}
