//! The working pixel buffer: premultiplied **linear-light** f32 RGBA.
//!
//! Every composite, blur and gradient in this crate runs on `Canvas`. Doing that math on
//! gamma-encoded sRGB bytes is simply wrong — a blur of black and white in sRGB keeps a
//! bright halo, and a 50% blend reads 188 instead of the 128 light actually produces — so
//! the buffer is converted once on the way in and once on the way out.

use dpaint_core::color::{linear_to_srgb, srgb_to_linear};
use dpaint_core::{Error, Result};
use std::sync::LazyLock;
use tiny_skia::{Pixmap, PixmapRef, PremultipliedColorU8};

/// sRGB byte -> linear float, tabulated because the transfer function is a `powf`.
static SRGB8_TO_LINEAR: LazyLock<[f32; 256]> = LazyLock::new(|| {
    let mut t = [0.0f32; 256];
    for (i, v) in t.iter_mut().enumerate() {
        *v = srgb_to_linear(i as f32 / 255.0);
    }
    t
});

/// Premultiplied linear-light RGBA, row-major, 4 floats per pixel.
#[derive(Clone, Debug, PartialEq)]
pub struct Canvas {
    pub width: u32,
    pub height: u32,
    pub data: Vec<f32>,
}

impl Canvas {
    pub fn new(width: u32, height: u32) -> Self {
        let (width, height) = (width.max(1), height.max(1));
        Self { width, height, data: vec![0.0; width as usize * height as usize * 4] }
    }

    pub fn filled(width: u32, height: u32, px: [f32; 4]) -> Self {
        let mut c = Self::new(width, height);
        for p in c.data.chunks_exact_mut(4) {
            p.copy_from_slice(&px);
        }
        c
    }

    #[inline]
    pub fn pixel_count(&self) -> usize {
        self.width as usize * self.height as usize
    }

    #[inline]
    pub fn idx(&self, x: u32, y: u32) -> usize {
        (y as usize * self.width as usize + x as usize) * 4
    }

    #[inline]
    pub fn get(&self, x: u32, y: u32) -> [f32; 4] {
        let i = self.idx(x, y);
        [self.data[i], self.data[i + 1], self.data[i + 2], self.data[i + 3]]
    }

    #[inline]
    pub fn set(&mut self, x: u32, y: u32, px: [f32; 4]) {
        let i = self.idx(x, y);
        self.data[i..i + 4].copy_from_slice(&px);
    }

    /// Pixel at a clamped coordinate — the sampling convention used by every filter here.
    #[inline]
    pub fn clamped(&self, x: i64, y: i64) -> [f32; 4] {
        let x = x.clamp(0, self.width as i64 - 1) as u32;
        let y = y.clamp(0, self.height as i64 - 1) as u32;
        self.get(x, y)
    }

    /// Bilinear sample in pixel coordinates (pixel centers at +0.5), clamped at the edges.
    pub fn sample(&self, x: f32, y: f32) -> [f32; 4] {
        let fx = x - 0.5;
        let fy = y - 0.5;
        let x0 = fx.floor();
        let y0 = fy.floor();
        let tx = fx - x0;
        let ty = fy - y0;
        let (x0, y0) = (x0 as i64, y0 as i64);
        let p00 = self.clamped(x0, y0);
        let p10 = self.clamped(x0 + 1, y0);
        let p01 = self.clamped(x0, y0 + 1);
        let p11 = self.clamped(x0 + 1, y0 + 1);
        let mut out = [0.0f32; 4];
        for c in 0..4 {
            let a = p00[c] + (p10[c] - p00[c]) * tx;
            let b = p01[c] + (p11[c] - p01[c]) * tx;
            out[c] = a + (b - a) * ty;
        }
        out
    }

    /// Bilinear sample with transparent-black outside the buffer, for transformed layers.
    pub fn sample_outside_transparent(&self, x: f32, y: f32) -> [f32; 4] {
        if x < -0.5 || y < -0.5 || x > self.width as f32 + 0.5 || y > self.height as f32 + 0.5 {
            return [0.0; 4];
        }
        self.sample(x, y)
    }

    /// sRGB u8 premultiplied (tiny-skia's representation) -> linear premultiplied f32.
    pub fn from_pixmap(p: PixmapRef<'_>) -> Self {
        let mut c = Canvas::new(p.width(), p.height());
        for (dst, src) in c.data.chunks_exact_mut(4).zip(p.pixels().iter()) {
            let a = src.alpha();
            if a == 0 {
                continue;
            }
            let af = a as f32 / 255.0;
            // Unpremultiply in sRGB, convert, premultiply again in linear light.
            let inv = 1.0 / a as f32;
            let r = (src.red() as f32 * inv).min(1.0);
            let g = (src.green() as f32 * inv).min(1.0);
            let b = (src.blue() as f32 * inv).min(1.0);
            dst[0] = srgb_to_linear(r) * af;
            dst[1] = srgb_to_linear(g) * af;
            dst[2] = srgb_to_linear(b) * af;
            dst[3] = af;
        }
        c
    }

    /// Linear premultiplied f32 -> sRGB u8 premultiplied.
    pub fn to_pixmap(&self) -> Pixmap {
        let mut p = Pixmap::new(self.width, self.height).expect("non-zero canvas size");
        for (src, dst) in self.data.chunks_exact(4).zip(p.pixels_mut().iter_mut()) {
            let a = src[3].clamp(0.0, 1.0);
            if a <= 0.0 {
                *dst = PremultipliedColorU8::TRANSPARENT;
                continue;
            }
            let r = linear_to_srgb((src[0] / a).clamp(0.0, 1.0));
            let g = linear_to_srgb((src[1] / a).clamp(0.0, 1.0));
            let b = linear_to_srgb((src[2] / a).clamp(0.0, 1.0));
            let a8 = (a * 255.0 + 0.5) as u8;
            let q = |v: f32| ((v * a * 255.0 + 0.5) as u8).min(a8);
            *dst = PremultipliedColorU8::from_rgba(q(r), q(g), q(b), a8)
                .unwrap_or(PremultipliedColorU8::TRANSPARENT);
        }
        p
    }

    /// Decode any PNG the asset store holds (8/16-bit, gray, palette, RGBA).
    pub fn from_png(bytes: &[u8]) -> Result<Self> {
        let img = image::load_from_memory_with_format(bytes, image::ImageFormat::Png)
            .map_err(|e| Error::AssetDecode(e.to_string()))?
            .to_rgba8();
        let (w, h) = img.dimensions();
        let t = &*SRGB8_TO_LINEAR;
        let mut c = Canvas::new(w, h);
        for (dst, src) in c.data.chunks_exact_mut(4).zip(img.pixels()) {
            let a = src.0[3] as f32 / 255.0;
            dst[0] = t[src.0[0] as usize] * a;
            dst[1] = t[src.0[1] as usize] * a;
            dst[2] = t[src.0[2] as usize] * a;
            dst[3] = a;
        }
        Ok(c)
    }

    pub fn to_png(&self) -> Result<Vec<u8>> {
        let pm = self.to_pixmap();
        let mut rgba = Vec::with_capacity(self.pixel_count() * 4);
        for px in pm.pixels() {
            let c = px.demultiply();
            rgba.extend_from_slice(&[c.red(), c.green(), c.blue(), c.alpha()]);
        }
        encode_png(self.width, self.height, image::ColorType::Rgba8, &rgba)
    }

    /// Bilinear resample to an exact pixel size.
    pub fn resized(&self, w: u32, h: u32) -> Self {
        let (w, h) = (w.max(1), h.max(1));
        if w == self.width && h == self.height {
            return self.clone();
        }
        let mut out = Canvas::new(w, h);
        let sx = self.width as f32 / w as f32;
        let sy = self.height as f32 / h as f32;
        for y in 0..h {
            for x in 0..w {
                let px = self.sample((x as f32 + 0.5) * sx, (y as f32 + 0.5) * sy);
                out.set(x, y, px);
            }
        }
        out
    }

    /// Draw `self` into a fresh `out_w x out_h` buffer through an affine map (device space),
    /// by inverse-mapping each destination pixel. Keeps resampling in linear light.
    pub fn transformed(&self, map: kurbo::Affine, out_w: u32, out_h: u32) -> Self {
        let mut out = Canvas::new(out_w, out_h);
        let Some(inv) = invert(map) else { return out };
        for y in 0..out_h {
            for x in 0..out_w {
                let p = inv * kurbo::Point::new(x as f64 + 0.5, y as f64 + 0.5);
                let px = self.sample_outside_transparent(p.x as f32, p.y as f32);
                out.set(x, y, px);
            }
        }
        out
    }

    /// Copy `self` into a buffer of `w x h` at an integer offset, no resampling.
    pub fn placed(&self, w: u32, h: u32, ox: i64, oy: i64) -> Self {
        let mut out = Canvas::new(w, h);
        for y in 0..self.height as i64 {
            let dy = y + oy;
            if dy < 0 || dy >= h as i64 {
                continue;
            }
            for x in 0..self.width as i64 {
                let dx = x + ox;
                if dx < 0 || dx >= w as i64 {
                    continue;
                }
                out.set(dx as u32, dy as u32, self.get(x as u32, y as u32));
            }
        }
        out
    }

    /// Tight bounding box of non-transparent pixels, `None` when fully transparent.
    pub fn opaque_bounds(&self, threshold: f32) -> Option<(u32, u32, u32, u32)> {
        let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0u32, 0u32);
        for y in 0..self.height {
            for x in 0..self.width {
                if self.data[self.idx(x, y) + 3] > threshold {
                    x0 = x0.min(x);
                    y0 = y0.min(y);
                    x1 = x1.max(x);
                    y1 = y1.max(y);
                }
            }
        }
        if x0 == u32::MAX {
            None
        } else {
            Some((x0, y0, x1 - x0 + 1, y1 - y0 + 1))
        }
    }

    /// Straight (non-premultiplied) linear color of a pixel.
    #[inline]
    pub fn straight(&self, i: usize) -> [f32; 4] {
        let a = self.data[i + 3];
        if a <= 0.0 {
            return [0.0, 0.0, 0.0, 0.0];
        }
        [self.data[i] / a, self.data[i + 1] / a, self.data[i + 2] / a, a]
    }

    #[inline]
    pub fn set_straight(&mut self, i: usize, c: [f32; 4]) {
        let a = c[3].clamp(0.0, 1.0);
        self.data[i] = c[0] * a;
        self.data[i + 1] = c[1] * a;
        self.data[i + 2] = c[2] * a;
        self.data[i + 3] = a;
    }
}

fn invert(a: kurbo::Affine) -> Option<kurbo::Affine> {
    let c = a.as_coeffs();
    let det = c[0] * c[3] - c[1] * c[2];
    if det.abs() < 1e-12 {
        return None;
    }
    Some(a.inverse())
}

/// PNG encoder shared by pixel layers and 8-bit masks.
pub fn encode_png(w: u32, h: u32, color: image::ColorType, bytes: &[u8]) -> Result<Vec<u8>> {
    use image::ImageEncoder;
    let mut out = Vec::new();
    image::codecs::png::PngEncoder::new(&mut out)
        .write_image(bytes, w, h, color.into())
        .map_err(|e| Error::Invalid(format!("png encode failed: {e}")))?;
    Ok(out)
}

/// Encode an 8-bit coverage buffer (selection, layer mask) as a grayscale PNG.
pub fn encode_gray_png(w: u32, h: u32, cov: &[f32]) -> Result<Vec<u8>> {
    let bytes: Vec<u8> = cov.iter().map(|v| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8).collect();
    encode_png(w, h, image::ColorType::L8, &bytes)
}

/// Decode a grayscale coverage PNG back to 0..1 floats.
pub fn decode_gray_png(bytes: &[u8]) -> Result<(u32, u32, Vec<f32>)> {
    let img = image::load_from_memory_with_format(bytes, image::ImageFormat::Png)
        .map_err(|e| Error::AssetDecode(e.to_string()))?;
    let has_alpha = img.color().has_alpha();
    let (w, h) = (img.width(), img.height());
    let cov = if has_alpha {
        // A mask stored as RGBA takes its coverage from alpha, which is what a
        // "make a mask out of this layer" round-trip produces.
        img.to_rgba8().pixels().map(|p| p.0[3] as f32 / 255.0).collect()
    } else {
        img.to_luma8().pixels().map(|p| p.0[0] as f32 / 255.0).collect()
    };
    Ok((w, h, cov))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pixmap_round_trip_is_lossless_at_byte_precision() {
        let mut pm = Pixmap::new(4, 1).unwrap();
        pm.pixels_mut()[0] = PremultipliedColorU8::from_rgba(255, 0, 0, 255).unwrap();
        pm.pixels_mut()[1] = PremultipliedColorU8::from_rgba(0, 128, 0, 255).unwrap();
        pm.pixels_mut()[2] = PremultipliedColorU8::from_rgba(32, 32, 32, 64).unwrap();
        pm.pixels_mut()[3] = PremultipliedColorU8::TRANSPARENT;
        let back = Canvas::from_pixmap(pm.as_ref()).to_pixmap();
        for (a, b) in pm.pixels().iter().zip(back.pixels()) {
            assert!(
                (a.red() as i32 - b.red() as i32).abs() <= 1
                    && (a.green() as i32 - b.green() as i32).abs() <= 1
                    && (a.blue() as i32 - b.blue() as i32).abs() <= 1
                    && a.alpha() == b.alpha(),
                "{a:?} != {b:?}"
            );
        }
    }

    #[test]
    fn mid_gray_is_stored_in_linear_light_not_gamma() {
        let mut pm = Pixmap::new(1, 1).unwrap();
        pm.pixels_mut()[0] = PremultipliedColorU8::from_rgba(128, 128, 128, 255).unwrap();
        let c = Canvas::from_pixmap(pm.as_ref());
        // sRGB 128 is ~21.6% of the light of white, not 50%.
        assert!((c.get(0, 0)[0] - 0.2158).abs() < 0.002, "{:?}", c.get(0, 0));
    }
}
