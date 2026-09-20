//! Pixel plumbing between the document and a provider: decoding results, building the mask a
//! model needs, and turning a cutout into a layer mask.

use dpaint_core::doc::raster::{RasterDoc, Selection};
use dpaint_core::kurbo::{BezPath, PathEl};
use dpaint_core::{AssetStore, Error, Result};
use tiny_skia as ts;

/// `data:<mime>;base64,<…>` — how inputs reach a provider that takes image URLs.
pub fn data_uri(bytes: &[u8], mime: &str) -> String {
    use base64::Engine;
    format!(
        "data:{mime};base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    )
}

pub fn mime_for_ext(ext: &str) -> &'static str {
    match ext {
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "tif" | "tiff" => "image/tiff",
        _ => "image/png",
    }
}

/// File extension for a downloaded result, from its content type, else its URL.
pub fn ext_for(content_type: Option<&str>, url: &str) -> String {
    let from_ct = content_type.and_then(|ct| match ct.split(';').next().unwrap_or("").trim() {
        "image/png" => Some("png"),
        "image/jpeg" => Some("jpg"),
        "image/webp" => Some("webp"),
        "image/tiff" => Some("tiff"),
        _ => None,
    });
    if let Some(e) = from_ct {
        return e.to_string();
    }
    let path = url.split(['?', '#']).next().unwrap_or(url);
    match path.rsplit('.').next().map(|e| e.to_ascii_lowercase()) {
        Some(e) if matches!(e.as_str(), "png" | "jpg" | "jpeg" | "webp" | "tiff") => {
            if e == "jpeg" {
                "jpg".into()
            } else {
                e
            }
        }
        _ => "png".into(),
    }
}

/// Decode any format the editor accepts into a premultiplied RGBA pixmap.
pub fn decode(bytes: &[u8]) -> Result<ts::Pixmap> {
    let img = image::load_from_memory(bytes)
        .map_err(|e| Error::AssetDecode(e.to_string()))?
        .to_rgba8();
    let (w, h) = img.dimensions();
    let mut pm = ts::Pixmap::new(w.max(1), h.max(1))
        .ok_or_else(|| Error::AssetDecode(format!("image is {w}x{h}")))?;
    for (dst, src) in pm.pixels_mut().iter_mut().zip(img.pixels()) {
        let [r, g, b, a] = src.0;
        *dst = ts::PremultipliedColorU8::from_rgba(
            mul(r, a),
            mul(g, a),
            mul(b, a),
            a,
        )
        .expect("premultiplied by construction");
    }
    Ok(pm)
}

fn mul(c: u8, a: u8) -> u8 {
    ((c as u32 * a as u32 + 127) / 255) as u8
}

pub fn encode_png(pm: &ts::Pixmap) -> Result<Vec<u8>> {
    pm.encode_png().map_err(|e| Error::AssetDecode(e.to_string()))
}

/// An opaque greyscale PNG of the alpha channel: white where the subject is. This is what a
/// background-removal result becomes, so the original pixels stay untouched and the cutout is
/// an ordinary, editable layer mask.
pub fn alpha_mask_png(pm: &ts::Pixmap) -> Result<Vec<u8>> {
    let mut out = ts::Pixmap::new(pm.width(), pm.height())
        .ok_or_else(|| Error::AssetDecode("empty cutout".into()))?;
    for (dst, src) in out.pixels_mut().iter_mut().zip(pm.pixels()) {
        let a = src.alpha();
        *dst = ts::PremultipliedColorU8::from_rgba(a, a, a, 255).expect("opaque grey");
    }
    encode_png(&out)
}

/// The document's live selection as a mask the size of the canvas: white = the region a model
/// should regenerate. The editor's own selection tools are the inpaint mask, which is the
/// entire point of having them.
pub fn selection_mask_png(doc: &RasterDoc, assets: &AssetStore) -> Result<Vec<u8>> {
    let sel = doc
        .selection
        .as_ref()
        .ok_or_else(|| Error::Invalid(format!(
            "document '{}' has no selection; select a region first (raster.select.*)",
            doc.id
        )))?;
    let (w, h) = (doc.width().max(1), doc.height().max(1));
    let mut pm = ts::Pixmap::new(w, h)
        .ok_or_else(|| Error::Invalid(format!("canvas is {w}x{h}")))?;
    pm.fill(ts::Color::BLACK);

    let mut covered = false;
    if let Some(mask) = &sel.mask {
        let bytes = assets.get(mask)?;
        let src = decode(&bytes)?;
        let sx = w as f32 / src.width() as f32;
        let sy = h as f32 / src.height() as f32;
        let mut paint = ts::PixmapPaint::default();
        paint.quality = ts::FilterQuality::Bilinear;
        pm.draw_pixmap(0, 0, src.as_ref(), &paint, ts::Transform::from_scale(sx, sy), None);
        // Compositing over black already premultiplies coverage into the channels, so the
        // final `normalize_opaque` collapses both alpha- and luminance-encoded masks.
        covered = true;
    }

    if let Some(d) = &sel.d {
        let bez = BezPath::from_svg(d)
            .map_err(|e| Error::DegenerateGeometry(format!("selection path: {e}")))?;
        if let Some(path) = to_skia_path(&bez) {
            let mut paint = ts::Paint::default();
            paint.set_color(ts::Color::WHITE);
            paint.anti_alias = true;
            pm.fill_path(
                &path,
                &paint,
                ts::FillRule::Winding,
                ts::Transform::identity(),
                None,
            );
            covered = true;
        }
    }

    if !covered {
        return Err(Error::Invalid(format!(
            "selection in '{}' has neither an outline nor a coverage mask",
            doc.id
        )));
    }

    if sel.feather > 0.5 {
        box_blur(&mut pm, sel.feather.round() as u32);
    }
    if sel.inverted {
        invert(&mut pm);
    }
    normalize_opaque(&mut pm);
    encode_png(&pm)
}

/// Selection coverage in [0, 1], used to reject an empty selection before billing.
pub fn selection_coverage(mask_png: &[u8]) -> Result<f64> {
    let pm = decode(mask_png)?;
    let total = (pm.width() as u64 * pm.height() as u64).max(1);
    let lit: u64 = pm.pixels().iter().map(|p| p.red() as u64).sum();
    Ok(lit as f64 / (total as f64 * 255.0))
}

pub fn bounds_of(sel: &Selection, doc: &RasterDoc) -> (f64, f64, f64, f64) {
    let b = sel.bounds;
    if b.is_empty() {
        (0.0, 0.0, doc.width() as f64, doc.height() as f64)
    } else {
        (b.x(), b.y(), b.w(), b.h())
    }
}

/// Grow a canvas by the requested margins, returning the padded image and the mask of the
/// new area — outpainting is inpainting of the margin.
pub fn pad(src: &ts::Pixmap, left: u32, top: u32, right: u32, bottom: u32) -> Result<(ts::Pixmap, ts::Pixmap)> {
    let w = src.width() + left + right;
    let h = src.height() + top + bottom;
    let mut out = ts::Pixmap::new(w, h).ok_or_else(|| Error::Invalid(format!("padded canvas is {w}x{h}")))?;
    let mut mask = ts::Pixmap::new(w, h).ok_or_else(|| Error::Invalid(format!("padded canvas is {w}x{h}")))?;
    mask.fill(ts::Color::WHITE);
    out.draw_pixmap(
        left as i32,
        top as i32,
        src.as_ref(),
        &ts::PixmapPaint::default(),
        ts::Transform::identity(),
        None,
    );
    // The original area is kept: black in the mask.
    let mut paint = ts::Paint::default();
    paint.set_color(ts::Color::BLACK);
    if let Some(rect) = ts::Rect::from_xywh(
        left as f32,
        top as f32,
        src.width() as f32,
        src.height() as f32,
    ) {
        mask.fill_rect(rect, &paint, ts::Transform::identity(), None);
    }
    normalize_opaque(&mut mask);
    Ok((out, mask))
}

fn invert(pm: &mut ts::Pixmap) {
    for px in pm.pixels_mut().iter_mut() {
        let v = 255 - px.red();
        *px = ts::PremultipliedColorU8::from_rgba(v, v, v, 255).expect("opaque grey");
    }
}

fn normalize_opaque(pm: &mut ts::Pixmap) {
    for px in pm.pixels_mut().iter_mut() {
        let v = px.red().max(px.green()).max(px.blue());
        *px = ts::PremultipliedColorU8::from_rgba(v, v, v, 255).expect("opaque grey");
    }
}

/// Separable box blur, three passes — a cheap, deterministic approximation of a Gaussian,
/// which is what a feathered selection edge needs.
fn box_blur(pm: &mut ts::Pixmap, radius: u32) {
    if radius == 0 {
        return;
    }
    let (w, h) = (pm.width() as usize, pm.height() as usize);
    let mut buf: Vec<u8> = pm.pixels().iter().map(|p| p.red()).collect();
    let mut tmp = vec![0u8; buf.len()];
    let r = radius as usize;
    for _ in 0..3 {
        blur_rows(&buf, &mut tmp, w, h, r);
        transpose(&tmp, &mut buf, w, h);
        blur_rows(&buf, &mut tmp, h, w, r);
        transpose(&tmp, &mut buf, h, w);
    }
    for (px, v) in pm.pixels_mut().iter_mut().zip(buf) {
        *px = ts::PremultipliedColorU8::from_rgba(v, v, v, 255).expect("opaque grey");
    }
}

fn blur_rows(src: &[u8], dst: &mut [u8], w: usize, h: usize, r: usize) {
    for y in 0..h {
        let row = y * w;
        let mut sum: u32 = 0;
        let mut count: u32 = 0;
        for x in 0..(r + 1).min(w) {
            sum += src[row + x] as u32;
            count += 1;
        }
        for x in 0..w {
            dst[row + x] = (sum / count.max(1)) as u8;
            if x >= r {
                sum -= src[row + x - r] as u32;
                count -= 1;
            }
            if x + r + 1 < w {
                sum += src[row + x + r + 1] as u32;
                count += 1;
            }
        }
    }
}

fn transpose(src: &[u8], dst: &mut [u8], w: usize, h: usize) {
    for y in 0..h {
        for x in 0..w {
            dst[x * h + y] = src[y * w + x];
        }
    }
}

fn to_skia_path(bez: &BezPath) -> Option<ts::Path> {
    let mut pb = ts::PathBuilder::new();
    for el in bez.elements() {
        match *el {
            PathEl::MoveTo(p) => pb.move_to(p.x as f32, p.y as f32),
            PathEl::LineTo(p) => pb.line_to(p.x as f32, p.y as f32),
            PathEl::QuadTo(a, b) => pb.quad_to(a.x as f32, a.y as f32, b.x as f32, b.y as f32),
            PathEl::CurveTo(a, b, c) => pb.cubic_to(
                a.x as f32, a.y as f32, b.x as f32, b.y as f32, c.x as f32, c.y as f32,
            ),
            PathEl::ClosePath => pb.close(),
        }
    }
    pb.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use dpaint_core::doc::Rect;
    use dpaint_core::DocId;

    fn doc_with_selection(sel: Selection) -> RasterDoc {
        let mut d = RasterDoc::new(DocId::from("doc_main"), "main", 64, 64);
        d.selection = Some(sel);
        d
    }

    #[test]
    fn a_rectangular_selection_becomes_a_white_on_black_mask() {
        let dir = tempfile::tempdir().unwrap();
        let assets = AssetStore::new(dir.path());
        let doc = doc_with_selection(Selection {
            d: Some("M16 16 L48 16 L48 48 L16 48 Z".into()),
            mask: None,
            feather: 0.0,
            inverted: false,
            bounds: Rect::new(16.0, 16.0, 32.0, 32.0),
        });

        let png = selection_mask_png(&doc, &assets).unwrap();
        let pm = decode(&png).unwrap();
        assert_eq!((pm.width(), pm.height()), (64, 64));
        let at = |x: u32, y: u32| pm.pixel(x, y).unwrap().red();
        assert_eq!(at(32, 32), 255, "inside the selection is opaque white");
        assert_eq!(at(2, 2), 0, "outside the selection is black");
        // A quarter of the canvas is selected.
        let coverage = selection_coverage(&png).unwrap();
        assert!((coverage - 0.25).abs() < 0.02, "coverage {coverage}");
    }

    #[test]
    fn inverting_the_selection_inverts_the_mask() {
        let dir = tempfile::tempdir().unwrap();
        let assets = AssetStore::new(dir.path());
        let doc = doc_with_selection(Selection {
            d: Some("M16 16 L48 16 L48 48 L16 48 Z".into()),
            mask: None,
            feather: 0.0,
            inverted: true,
            bounds: Rect::new(16.0, 16.0, 32.0, 32.0),
        });
        let pm = decode(&selection_mask_png(&doc, &assets).unwrap()).unwrap();
        assert_eq!(pm.pixel(32, 32).unwrap().red(), 0);
        assert_eq!(pm.pixel(2, 2).unwrap().red(), 255);
    }

    #[test]
    fn feathering_softens_the_selection_edge() {
        let dir = tempfile::tempdir().unwrap();
        let assets = AssetStore::new(dir.path());
        let hard = decode(
            &selection_mask_png(
                &doc_with_selection(Selection {
                    d: Some("M16 16 L48 16 L48 48 L16 48 Z".into()),
                    mask: None,
                    feather: 0.0,
                    inverted: false,
                    bounds: Rect::default(),
                }),
                &assets,
            )
            .unwrap(),
        )
        .unwrap();
        let soft = decode(
            &selection_mask_png(
                &doc_with_selection(Selection {
                    d: Some("M16 16 L48 16 L48 48 L16 48 Z".into()),
                    mask: None,
                    feather: 6.0,
                    inverted: false,
                    bounds: Rect::default(),
                }),
                &assets,
            )
            .unwrap(),
        )
        .unwrap();

        let edge_hard = hard.pixel(16, 32).unwrap().red();
        let edge_soft = soft.pixel(16, 32).unwrap().red();
        assert!(edge_hard == 0 || edge_hard == 255, "hard edge is binary: {edge_hard}");
        assert!(
            (1..=254).contains(&edge_soft),
            "feathered edge is a gradient, got {edge_soft}"
        );
    }

    #[test]
    fn a_document_without_a_selection_is_an_error_not_a_full_canvas_mask() {
        let dir = tempfile::tempdir().unwrap();
        let assets = AssetStore::new(dir.path());
        let doc = RasterDoc::new(DocId::from("doc_main"), "main", 8, 8);
        let err = selection_mask_png(&doc, &assets).unwrap_err();
        assert!(err.to_string().contains("no selection"), "{err}");
    }

    #[test]
    fn padding_marks_only_the_new_margin_as_paintable() {
        let mut src = ts::Pixmap::new(8, 8).unwrap();
        src.fill(ts::Color::from_rgba8(10, 20, 30, 255));
        let (padded, mask) = pad(&src, 4, 0, 4, 0).unwrap();
        assert_eq!((padded.width(), padded.height()), (16, 8));
        assert_eq!(mask.pixel(0, 0).unwrap().red(), 255, "new margin is paintable");
        assert_eq!(mask.pixel(8, 4).unwrap().red(), 0, "original pixels are protected");
        assert_eq!(padded.pixel(8, 4).unwrap().red(), 10, "original pixels are kept");
    }

    #[test]
    fn a_cutout_becomes_a_mask_of_its_alpha() {
        let mut pm = ts::Pixmap::new(4, 1).unwrap();
        {
            let px = pm.pixels_mut();
            px[0] = ts::PremultipliedColorU8::from_rgba(0, 0, 0, 0).unwrap();
            px[1] = ts::PremultipliedColorU8::from_rgba(64, 64, 64, 128).unwrap();
            px[2] = ts::PremultipliedColorU8::from_rgba(255, 0, 0, 255).unwrap();
            px[3] = ts::PremultipliedColorU8::from_rgba(0, 0, 0, 0).unwrap();
        }
        let mask = decode(&alpha_mask_png(&pm).unwrap()).unwrap();
        assert_eq!(mask.pixel(0, 0).unwrap().red(), 0);
        assert_eq!(mask.pixel(1, 0).unwrap().red(), 128);
        assert_eq!(mask.pixel(2, 0).unwrap().red(), 255);
    }
}
