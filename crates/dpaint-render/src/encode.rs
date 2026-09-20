//! Output encoders. One place that knows about file formats, so every surface — CLI, MCP,
//! GUI — writes identical bytes.

use dpaint_core::{Error, Result};
use image::{ImageEncoder, RgbaImage};
use tiny_skia::Pixmap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ImageFormat {
    Png,
    Jpeg,
    Webp,
    Tiff,
}

impl ImageFormat {
    pub fn from_path(path: &str) -> Result<Self> {
        let ext = path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
        Self::from_ext(&ext)
    }

    pub fn from_ext(ext: &str) -> Result<Self> {
        match ext {
            "png" => Ok(ImageFormat::Png),
            "jpg" | "jpeg" => Ok(ImageFormat::Jpeg),
            "webp" => Ok(ImageFormat::Webp),
            "tif" | "tiff" => Ok(ImageFormat::Tiff),
            other => Err(Error::UnsupportedFormat(format!(
                "'{other}' (expected png, jpg, webp or tiff)"
            ))),
        }
    }

    pub fn ext(self) -> &'static str {
        match self {
            ImageFormat::Png => "png",
            ImageFormat::Jpeg => "jpg",
            ImageFormat::Webp => "webp",
            ImageFormat::Tiff => "tiff",
        }
    }

    pub fn has_alpha(self) -> bool {
        !matches!(self, ImageFormat::Jpeg)
    }
}

/// Un-premultiply a tiny-skia pixmap into straight-alpha RGBA8.
pub fn to_rgba(pixmap: &Pixmap) -> RgbaImage {
    let (w, h) = (pixmap.width(), pixmap.height());
    let mut out = RgbaImage::new(w, h);
    for (i, px) in pixmap.pixels().iter().enumerate() {
        let a = px.alpha();
        let unpre = |c: u8| -> u8 {
            if a == 0 {
                0
            } else {
                ((c as u32 * 255 + a as u32 / 2) / a as u32).min(255) as u8
            }
        };
        out.as_mut()[i * 4] = unpre(px.red());
        out.as_mut()[i * 4 + 1] = unpre(px.green());
        out.as_mut()[i * 4 + 2] = unpre(px.blue());
        out.as_mut()[i * 4 + 3] = a;
    }
    out
}

/// Flatten onto an opaque background, for formats without alpha.
pub fn flatten(img: &RgbaImage, bg: dpaint_core::Color) -> RgbaImage {
    let b = bg.to_rgba8();
    let mut out = img.clone();
    for px in out.pixels_mut() {
        let a = px.0[3] as u32;
        for c in 0..3 {
            px.0[c] = ((px.0[c] as u32 * a + b[c] as u32 * (255 - a)) / 255) as u8;
        }
        px.0[3] = 255;
    }
    out
}

/// Encode to bytes. `quality` applies to lossy formats (1..=100).
pub fn encode(img: &RgbaImage, format: ImageFormat, quality: u8) -> Result<Vec<u8>> {
    let mut buf = Vec::new();
    let (w, h) = (img.width(), img.height());
    match format {
        ImageFormat::Png => {
            image::codecs::png::PngEncoder::new_with_quality(
                &mut buf,
                image::codecs::png::CompressionType::Best,
                image::codecs::png::FilterType::Adaptive,
            )
            .write_image(img.as_raw(), w, h, image::ExtendedColorType::Rgba8)
            .map_err(|e| Error::Invalid(format!("png encode failed: {e}")))?;
        }
        ImageFormat::Jpeg => {
            let rgb = image::DynamicImage::ImageRgba8(img.clone()).to_rgb8();
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, quality.clamp(1, 100))
                .write_image(rgb.as_raw(), w, h, image::ExtendedColorType::Rgb8)
                .map_err(|e| Error::Invalid(format!("jpeg encode failed: {e}")))?;
        }
        ImageFormat::Webp => {
            // The `image` crate encodes lossless WebP only, which is the right default for
            // design output; lossy WebP would need a separate encoder.
            image::codecs::webp::WebPEncoder::new_lossless(&mut buf)
                .write_image(img.as_raw(), w, h, image::ExtendedColorType::Rgba8)
                .map_err(|e| Error::Invalid(format!("webp encode failed: {e}")))?;
        }
        ImageFormat::Tiff => {
            let mut cursor = std::io::Cursor::new(&mut buf);
            image::codecs::tiff::TiffEncoder::new(&mut cursor)
                .write_image(img.as_raw(), w, h, image::ExtendedColorType::Rgba8)
                .map_err(|e| Error::Invalid(format!("tiff encode failed: {e}")))?;
        }
    }
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tiny_skia::{Paint, PixmapPaint, Transform};

    fn checker() -> Pixmap {
        let mut p = Pixmap::new(8, 8).unwrap();
        let mut paint = Paint::default();
        paint.set_color_rgba8(255, 0, 0, 128);
        p.fill_rect(
            tiny_skia::Rect::from_xywh(0.0, 0.0, 4.0, 8.0).unwrap(),
            &paint,
            Transform::identity(),
            None,
        );
        let _ = PixmapPaint::default();
        p
    }

    #[test]
    fn unpremultiplying_recovers_the_source_color() {
        let rgba = to_rgba(&checker());
        let px = rgba.get_pixel(1, 1);
        assert_eq!(px.0[3], 128);
        assert!(px.0[0] > 250, "red must come back near full after unpremultiply, got {px:?}");
    }

    #[test]
    fn flattening_composites_alpha_against_the_background() {
        let rgba = to_rgba(&checker());
        let flat = flatten(&rgba, dpaint_core::Color::WHITE);
        let px = flat.get_pixel(1, 1);
        assert_eq!(px.0[3], 255);
        assert!(px.0[1] > 100 && px.0[1] < 160, "half-alpha red on white is pink, got {px:?}");
    }

    #[test]
    fn every_format_encodes_to_a_decodable_file_of_the_right_size() {
        let rgba = to_rgba(&checker());
        for f in [ImageFormat::Png, ImageFormat::Jpeg, ImageFormat::Webp, ImageFormat::Tiff] {
            let bytes = encode(&rgba, f, 90).unwrap();
            assert!(!bytes.is_empty(), "{f:?} produced no bytes");
            let decoded = image::load_from_memory(&bytes)
                .unwrap_or_else(|e| panic!("{f:?} output did not decode: {e}"));
            assert_eq!((decoded.width(), decoded.height()), (8, 8), "{f:?} changed dimensions");
        }
    }

    #[test]
    fn format_is_inferred_from_the_path_and_unknown_ones_are_rejected() {
        assert_eq!(ImageFormat::from_path("out/poster.PNG").unwrap(), ImageFormat::Png);
        assert_eq!(ImageFormat::from_path("a.jpeg").unwrap(), ImageFormat::Jpeg);
        assert_eq!(ImageFormat::from_path("a.xcf").unwrap_err().code(), "unsupported_format");
    }
}
