//! All 27 blend modes, plus the source-over compositing loop they plug into.
//!
//! Blend functions take **straight** (non-premultiplied) linear colors and return a straight
//! color; the compositor applies the W3C/PDF mixing formula
//! `co = as*(1-ab)*Cs + as*ab*B(Cb,Cs) + (1-as)*ab*Cb` so partial alpha on either side
//! behaves like every other compositing engine.

use crate::canvas::Canvas;
use dpaint_core::doc::raster::BlendMode;

/// Non-separable modes use the PDF luminosity coefficients, not WCAG's — that is what
/// Photoshop, GIMP, Cairo and browsers all agree on for `hue`/`color`/`luminosity`.
#[inline]
fn lum(c: [f32; 3]) -> f32 {
    0.3 * c[0] + 0.59 * c[1] + 0.11 * c[2]
}

#[inline]
fn clip_color(mut c: [f32; 3]) -> [f32; 3] {
    let l = lum(c);
    let n = c[0].min(c[1]).min(c[2]);
    let x = c[0].max(c[1]).max(c[2]);
    if n < 0.0 {
        let d = l - n;
        if d > 1e-6 {
            for v in c.iter_mut() {
                *v = l + (*v - l) * l / d;
            }
        } else {
            c = [l, l, l];
        }
    }
    if x > 1.0 {
        let d = x - l;
        if d > 1e-6 {
            for v in c.iter_mut() {
                *v = l + (*v - l) * (1.0 - l) / d;
            }
        } else {
            c = [l, l, l];
        }
    }
    c
}

#[inline]
fn set_lum(c: [f32; 3], l: f32) -> [f32; 3] {
    let d = l - lum(c);
    clip_color([c[0] + d, c[1] + d, c[2] + d])
}

#[inline]
fn sat(c: [f32; 3]) -> f32 {
    c[0].max(c[1]).max(c[2]) - c[0].min(c[1]).min(c[2])
}

#[inline]
fn set_sat(c: [f32; 3], s: f32) -> [f32; 3] {
    // Order-preserving rescale of min/mid/max, per the PDF spec's SetSat.
    let mut idx = [0usize, 1, 2];
    idx.sort_by(|&a, &b| c[a].partial_cmp(&c[b]).unwrap_or(std::cmp::Ordering::Equal));
    let (imin, imid, imax) = (idx[0], idx[1], idx[2]);
    let mut out = [0.0f32; 3];
    if c[imax] > c[imin] {
        out[imid] = (c[imid] - c[imin]) * s / (c[imax] - c[imin]);
        out[imax] = s;
    }
    out[imin] = 0.0;
    out
}

#[inline]
fn sep(f: impl Fn(f32, f32) -> f32, cb: [f32; 3], cs: [f32; 3]) -> [f32; 3] {
    [f(cb[0], cs[0]), f(cb[1], cs[1]), f(cb[2], cs[2])]
}

/// `B(Cb, Cs)` for every mode except `Dissolve`, which is resolved by the compositor
/// because it is a stochastic coverage decision rather than a color function.
pub fn blend(mode: BlendMode, cb: [f32; 3], cs: [f32; 3]) -> [f32; 3] {
    use BlendMode::*;
    match mode {
        Normal | Dissolve => cs,
        Multiply => sep(|b, s| b * s, cb, cs),
        Screen => sep(|b, s| b + s - b * s, cb, cs),
        Overlay => sep(hard_light_swapped, cb, cs),
        Darken => sep(|b, s| b.min(s), cb, cs),
        Lighten => sep(|b, s| b.max(s), cb, cs),
        ColorDodge => sep(
            |b, s| {
                if b <= 0.0 {
                    0.0
                } else if s >= 1.0 {
                    1.0
                } else {
                    (b / (1.0 - s)).min(1.0)
                }
            },
            cb,
            cs,
        ),
        ColorBurn => sep(
            |b, s| {
                if b >= 1.0 {
                    1.0
                } else if s <= 0.0 {
                    0.0
                } else {
                    1.0 - ((1.0 - b) / s).min(1.0)
                }
            },
            cb,
            cs,
        ),
        HardLight => sep(|b, s| hard_light_swapped(s, b), cb, cs),
        SoftLight => sep(soft_light, cb, cs),
        Difference => sep(|b, s| (b - s).abs(), cb, cs),
        Exclusion => sep(|b, s| b + s - 2.0 * b * s, cb, cs),
        LinearBurn => sep(|b, s| (b + s - 1.0).clamp(0.0, 1.0), cb, cs),
        LinearDodge => sep(|b, s| (b + s).clamp(0.0, 1.0), cb, cs),
        VividLight => sep(
            |b, s| {
                if s <= 0.5 {
                    let d = 2.0 * s;
                    if d <= 0.0 {
                        if b >= 1.0 { 1.0 } else { 0.0 }
                    } else {
                        1.0 - ((1.0 - b) / d).min(1.0)
                    }
                } else {
                    let d = 1.0 - (s - 0.5) * 2.0;
                    if d <= 0.0 {
                        if b <= 0.0 { 0.0 } else { 1.0 }
                    } else {
                        (b / d).min(1.0)
                    }
                }
            },
            cb,
            cs,
        ),
        LinearLight => sep(|b, s| (b + 2.0 * s - 1.0).clamp(0.0, 1.0), cb, cs),
        PinLight => sep(
            |b, s| {
                if s <= 0.5 {
                    b.min(2.0 * s)
                } else {
                    b.max(2.0 * s - 1.0)
                }
            },
            cb,
            cs,
        ),
        HardMix => sep(
            |b, s| {
                if b + s >= 1.0 {
                    1.0
                } else {
                    0.0
                }
            },
            cb,
            cs,
        ),
        Subtract => sep(|b, s| (b - s).max(0.0), cb, cs),
        Divide => sep(
            |b, s| {
                if s <= 0.0 {
                    if b > 0.0 { 1.0 } else { 0.0 }
                } else {
                    (b / s).min(1.0)
                }
            },
            cb,
            cs,
        ),
        DarkerColor => {
            if lum(cs) <= lum(cb) {
                cs
            } else {
                cb
            }
        }
        LighterColor => {
            if lum(cs) >= lum(cb) {
                cs
            } else {
                cb
            }
        }
        Hue => set_lum(set_sat(cs, sat(cb)), lum(cb)),
        Saturation => set_lum(set_sat(cb, sat(cs)), lum(cb)),
        Color => set_lum(cs, lum(cb)),
        Luminosity => set_lum(cb, lum(cs)),
    }
}

#[inline]
fn hard_light_swapped(b: f32, s: f32) -> f32 {
    // HardLight(Cb, Cs) with arguments already swapped for Overlay.
    if b <= 0.5 {
        2.0 * b * s
    } else {
        let (b2, s2) = (2.0 * b - 1.0, s);
        b2 + s2 - b2 * s2
    }
}

#[inline]
fn soft_light(b: f32, s: f32) -> f32 {
    if s <= 0.5 {
        b - (1.0 - 2.0 * s) * b * (1.0 - b)
    } else {
        let d = if b <= 0.25 {
            ((16.0 * b - 12.0) * b + 4.0) * b
        } else {
            b.sqrt()
        };
        b + (2.0 * s - 1.0) * (d - b)
    }
}

/// Deterministic per-pixel hash — `Dissolve` and the noise filters need randomness that
/// depends only on `(x, y, seed)`, so replay and parallel tiles give identical output.
#[inline]
pub fn hash01(x: u32, y: u32, seed: u64) -> f32 {
    let mut h = seed
        ^ (x as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ (y as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F);
    h ^= h >> 33;
    h = h.wrapping_mul(0xFF51_AFD7_ED55_8CCD);
    h ^= h >> 33;
    h = h.wrapping_mul(0xC4CE_B9FE_1A85_EC53);
    h ^= h >> 33;
    (h >> 40) as f32 / 16_777_216.0
}

/// Extra per-pixel coverage applied to the source: layer mask, clip alpha, selection.
pub enum Coverage<'a> {
    Full,
    Buffer(&'a [f32]),
}

impl Coverage<'_> {
    #[inline]
    fn at(&self, i: usize) -> f32 {
        match self {
            Coverage::Full => 1.0,
            Coverage::Buffer(b) => b.get(i).copied().unwrap_or(0.0),
        }
    }
}

/// Composite `src` onto `dst` in place. Both buffers are premultiplied linear RGBA of the
/// same size; `opacity` and `coverage` scale the source alpha before mixing.
pub fn composite(
    dst: &mut Canvas,
    src: &Canvas,
    mode: BlendMode,
    opacity: f32,
    coverage: &Coverage<'_>,
    seed: u64,
) {
    debug_assert_eq!(dst.width, src.width);
    debug_assert_eq!(dst.height, src.height);
    let opacity = opacity.clamp(0.0, 1.0);
    let w = dst.width;
    for i in 0..dst.pixel_count() {
        let cov = coverage.at(i);
        if cov <= 0.0 {
            continue;
        }
        let o = i * 4;
        let sa_raw = src.data[o + 3];
        let mut asrc = sa_raw * opacity * cov;
        if mode == BlendMode::Dissolve {
            // Dissolve is a per-pixel coin flip on coverage, not a color mix.
            let (x, y) = ((i as u32) % w, (i as u32) / w);
            asrc = if hash01(x, y, seed) < asrc { sa_raw } else { 0.0 };
            if asrc <= 0.0 {
                continue;
            }
            dst.data[o] = src.data[o];
            dst.data[o + 1] = src.data[o + 1];
            dst.data[o + 2] = src.data[o + 2];
            dst.data[o + 3] = asrc;
            continue;
        }
        if asrc <= 0.0 {
            continue;
        }
        let ab = dst.data[o + 3];
        let cs = if sa_raw > 0.0 {
            [src.data[o] / sa_raw, src.data[o + 1] / sa_raw, src.data[o + 2] / sa_raw]
        } else {
            [0.0; 3]
        };
        if ab <= 0.0 {
            dst.data[o] = cs[0] * asrc;
            dst.data[o + 1] = cs[1] * asrc;
            dst.data[o + 2] = cs[2] * asrc;
            dst.data[o + 3] = asrc;
            continue;
        }
        let cb = [dst.data[o] / ab, dst.data[o + 1] / ab, dst.data[o + 2] / ab];
        let b = if mode == BlendMode::Normal { cs } else { blend(mode, cb, cs) };
        let ao = asrc + ab * (1.0 - asrc);
        for c in 0..3 {
            let co = asrc * (1.0 - ab) * cs[c] + asrc * ab * b[c] + (1.0 - asrc) * ab * cb[c];
            dst.data[o + c] = co;
        }
        dst.data[o + 3] = ao;
    }
}

/// Multiply a canvas' alpha by a coverage buffer in place — layer masks and clipping.
pub fn mask_alpha(c: &mut Canvas, cov: &[f32]) {
    for (i, px) in c.data.chunks_exact_mut(4).enumerate() {
        let m = cov.get(i).copied().unwrap_or(0.0).clamp(0.0, 1.0);
        px[0] *= m;
        px[1] *= m;
        px[2] *= m;
        px[3] *= m;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multiply_and_screen_are_duals() {
        let cb = [0.3, 0.6, 0.9];
        let cs = [0.2, 0.5, 0.8];
        let m = blend(BlendMode::Multiply, cb, cs);
        let s = blend(BlendMode::Screen, cb, cs);
        for c in 0..3 {
            let dual = 1.0 - (1.0 - cb[c]) * (1.0 - cs[c]);
            assert!((s[c] - dual).abs() < 1e-6);
            assert!((m[c] - cb[c] * cs[c]).abs() < 1e-6);
        }
    }

    #[test]
    fn luminosity_takes_source_light_and_keeps_backdrop_chroma() {
        let cb = [0.8, 0.2, 0.2];
        let out = blend(BlendMode::Luminosity, cb, [0.5, 0.5, 0.5]);
        assert!((lum(out) - 0.5).abs() < 1e-4, "luminosity must adopt source light: {out:?}");
        // Red is still dominant: the hue/chroma of the backdrop survived.
        assert!(out[0] > out[1] && out[0] > out[2], "{out:?}");
    }

    #[test]
    fn hue_mode_adopts_source_hue_and_backdrop_light() {
        let cb = [0.8, 0.2, 0.2];
        let out = blend(BlendMode::Hue, cb, [0.2, 0.2, 0.8]);
        assert!((lum(out) - lum(cb)).abs() < 1e-4);
        assert!(out[2] > out[0], "source hue (blue) must win: {out:?}");
    }
}
