//! The compositor: a depth-first walk of the layer tree in z-order.
//!
//! Three things force the structure of this file:
//!
//! - **Groups need their own buffer.** Group opacity 50% must fade the *composited* group,
//!   not each child independently, so a group renders into a fresh canvas and is then
//!   composited once.
//! - **Adjustment layers read the backdrop.** They are not sources; they transform whatever
//!   has accumulated beneath them inside their own group, which is why the accumulator is
//!   passed down rather than returned up.
//! - **Clipping masks intersect coverage.** A run of `clip: true` layers composites into the
//!   base layer's buffer, limited to the base's alpha, before the base hits the backdrop.

use crate::adjust;
use crate::blend::{composite, mask_alpha, Coverage};
use crate::canvas::{decode_gray_png, Canvas};
use crate::effects;
use crate::geom;
use crate::paint;
use crate::select::SelMask;
use crate::text::FontSet;
use dpaint_core::doc::common::FillRule;
use dpaint_core::doc::raster::{Adjustment, Fit, Layer, LayerKind, Mask};
use dpaint_core::{AssetStore, DocId, Error, Project, RasterDoc, Result};
use tiny_skia::Pixmap;

/// Renders another document in the project (vector or model) at a requested pixel size.
/// `dpaint-render` supplies this; raster must not depend on the other engines.
pub type LinkResolver<'a> = dyn Fn(&DocId, u32, u32) -> Result<Pixmap> + 'a;

pub struct Ctx<'a> {
    pub project: &'a Project,
    pub assets: &'a AssetStore,
    pub scale: f64,
    pub width: u32,
    pub height: u32,
    pub link: &'a LinkResolver<'a>,
    pub fonts: FontSet,
    /// Blend seed for `Dissolve`, taken from the document id so replay is stable.
    pub seed: u64,
}

fn doc_seed(doc: &DocId) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for b in doc.as_str().as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    h
}

/// Device size of a document at a render scale. Scale 2 doubles both dimensions exactly.
pub fn device_size(doc: &RasterDoc, scale: f64) -> Result<(u32, u32)> {
    if !(scale.is_finite() && scale > 0.0) {
        return Err(Error::Invalid(format!("render scale must be positive, got {scale}")));
    }
    let w = (doc.width() as f64 * scale).round().max(1.0) as u32;
    let h = (doc.height() as f64 * scale).round().max(1.0) as u32;
    Ok((w, h))
}

/// Composite a raster document to a pixmap.
pub fn render_doc(
    project: &Project,
    doc: &DocId,
    assets: &AssetStore,
    scale: f64,
    link: &LinkResolver<'_>,
) -> Result<Pixmap> {
    Ok(render_canvas(project, doc, assets, scale, link)?.to_pixmap())
}

/// Same walk as [`render_doc`], but keeping the linear-light buffer — used by
/// `layer.rasterize`, `merge-down`, `doc.flatten` and the selection ops.
pub fn render_canvas(
    project: &Project,
    doc: &DocId,
    assets: &AssetStore,
    scale: f64,
    link: &LinkResolver<'_>,
) -> Result<Canvas> {
    let rd = project.raster(doc)?;
    let (w, h) = device_size(rd, scale)?;
    let ctx = Ctx {
        project,
        assets,
        scale,
        width: w,
        height: h,
        link,
        fonts: FontSet::new(project, assets),
        seed: doc_seed(doc),
    };
    let mut acc = match rd.background {
        Some(bg) => {
            let mut c = Canvas::new(w, h);
            let s = bg.to_linear();
            for i in (0..c.data.len()).step_by(4) {
                c.set_straight(i, s);
            }
            c
        }
        None => Canvas::new(w, h),
    };
    composite_stack(&rd.layers, &mut acc, &ctx)?;
    Ok(acc)
}

/// Composite one sibling list onto `acc`, handling clip runs and adjustment layers.
pub fn composite_stack(layers: &[Layer], acc: &mut Canvas, ctx: &Ctx<'_>) -> Result<()> {
    let mut i = 0;
    while i < layers.len() {
        let layer = &layers[i];
        // The clipped run above this layer travels with it.
        let mut j = i + 1;
        while j < layers.len() && layers[j].clip {
            j += 1;
        }
        if !layer.visible {
            i = j;
            continue;
        }
        if let LayerKind::Adjustment { adjustment } = &layer.kind {
            apply_adjustment_layer(acc, layer, adjustment, ctx, None)?;
            i = j;
            continue;
        }

        let mut content = render_layer(layer, ctx)?;
        if j > i + 1 {
            // Clipping mask: the run is limited to the base layer's coverage.
            let base_alpha: Vec<f32> =
                (0..content.pixel_count()).map(|k| content.data[k * 4 + 3]).collect();
            for child in &layers[i + 1..j] {
                if !child.visible {
                    continue;
                }
                if let LayerKind::Adjustment { adjustment } = &child.kind {
                    apply_adjustment_layer(&mut content, child, adjustment, ctx, Some(&base_alpha))?;
                    continue;
                }
                let cc = render_layer(child, ctx)?;
                composite(
                    &mut content,
                    &cc,
                    child.blend,
                    child.opacity,
                    &Coverage::Buffer(&base_alpha),
                    ctx.seed,
                );
            }
        }
        composite(acc, &content, layer.blend, layer.opacity, &Coverage::Full, ctx.seed);
        i = j;
    }
    Ok(())
}

fn apply_adjustment_layer(
    acc: &mut Canvas,
    layer: &Layer,
    adjustment: &Adjustment,
    ctx: &Ctx<'_>,
    extra: Option<&[f32]>,
) -> Result<()> {
    let prepared = adjust::prepare(adjustment, ctx.assets)?;
    let mask = layer_mask_coverage(layer, ctx)?;
    let opacity = layer.opacity.clamp(0.0, 1.0);
    for i in 0..acc.pixel_count() {
        let mut w = opacity;
        if let Some(m) = &mask {
            w *= m[i];
        }
        if let Some(e) = extra {
            w *= e[i];
        }
        if w <= 0.0 {
            continue;
        }
        let o = i * 4;
        if acc.data[o + 3] <= 0.0 {
            continue;
        }
        let s = acc.straight(o);
        let adjusted = adjust::apply_pixel(&prepared, s);
        let mixed = [
            s[0] + (adjusted[0] - s[0]) * w,
            s[1] + (adjusted[1] - s[1]) * w,
            s[2] + (adjusted[2] - s[2]) * w,
            s[3] + (adjusted[3] - s[3]) * w,
        ];
        acc.set_straight(o, mixed);
    }
    Ok(())
}

/// Coverage of a layer's mask in device space, `None` when the layer has no active mask.
pub fn layer_mask_coverage(layer: &Layer, ctx: &Ctx<'_>) -> Result<Option<Vec<f32>>> {
    let Some(mask) = &layer.mask else { return Ok(None) };
    if !mask.enabled {
        return Ok(None);
    }
    Ok(Some(mask_coverage(mask, ctx.assets, ctx.width, ctx.height, ctx.scale)?))
}

/// Decode a layer mask blob into device-space coverage, honoring its offset and `inverted`.
pub fn mask_coverage(
    mask: &Mask,
    assets: &AssetStore,
    w: u32,
    h: u32,
    scale: f64,
) -> Result<Vec<f32>> {
    let (mw, mh, cov) = decode_gray_png(&assets.get(&mask.asset)?)?;
    let native = SelMask::from_cov(mw, mh, cov);
    // Place at its document-space offset, then scale to device.
    let doc_w = (w as f64 / scale).round().max(1.0) as u32;
    let doc_h = (h as f64 / scale).round().max(1.0) as u32;
    let mut placed = SelMask::new(doc_w, doc_h, 0.0);
    for y in 0..doc_h as i64 {
        let sy = y - mask.offset[1] as i64;
        if sy < 0 || sy >= mh as i64 {
            continue;
        }
        for x in 0..doc_w as i64 {
            let sx = x - mask.offset[0] as i64;
            if sx < 0 || sx >= mw as i64 {
                continue;
            }
            placed.cov[y as usize * doc_w as usize + x as usize] = native.at(sx as u32, sy as u32);
        }
    }
    let mut out = placed.resized(w, h);
    if mask.inverted {
        out.invert();
    }
    Ok(out.cov)
}

/// Render one layer's own content (no opacity, no blend — those belong to the composite
/// step), with its mask and effects applied.
pub fn render_layer(layer: &Layer, ctx: &Ctx<'_>) -> Result<Canvas> {
    let (w, h) = (ctx.width, ctx.height);
    let at = geom::device_matrix(layer.transform, ctx.scale);
    let identity = layer.transform.is_identity();
    let mut content = match &layer.kind {
        LayerKind::Pixel { asset, offset } => {
            let src = Canvas::from_png(&ctx.assets.get(asset)?)?;
            let map = at * dpaint_core::kurbo::Affine::translate((offset[0] as f64, offset[1] as f64));
            if identity && offset == &[0, 0] && src.width == w && src.height == h {
                src
            } else if identity && ctx.scale == 1.0 {
                src.placed(w, h, offset[0] as i64, offset[1] as i64)
            } else {
                src.transformed(map, w, h)
            }
        }
        LayerKind::Fill { color } => {
            let mut c = Canvas::new(w, h);
            let s = color.to_linear();
            for i in (0..c.data.len()).step_by(4) {
                c.set_straight(i, s);
            }
            if identity {
                c
            } else {
                c.transformed(at, w, h)
            }
        }
        LayerKind::Gradient { paint } => {
            let c = paint::paint_canvas(paint, w, h, ctx.scale, &|d, tw, th| (ctx.link)(d, tw, th))?;
            if identity {
                c
            } else {
                c.transformed(at, w, h)
            }
        }
        LayerKind::Shape { d, fill, stroke, fill_rule } => {
            let path = geom::parse_d(d)?;
            let sk = geom::to_sk(&path, at)
                .ok_or_else(|| Error::DegenerateGeometry("shape path is empty".into()))?;
            let mut c = Canvas::new(w, h);
            let cov = geom::fill_coverage(&sk, w, h, *fill_rule);
            let fill_c = paint::paint_canvas(fill, w, h, ctx.scale, &|d, tw, th| (ctx.link)(d, tw, th))?;
            composite(
                &mut c,
                &fill_c,
                dpaint_core::doc::raster::BlendMode::Normal,
                1.0,
                &Coverage::Buffer(&cov),
                ctx.seed,
            );
            if let Some(s) = stroke {
                let scov = geom::stroke_coverage(&sk, s, ctx.scale, w, h);
                let sc = paint::paint_canvas(&s.paint, w, h, ctx.scale, &|d, tw, th| (ctx.link)(d, tw, th))?;
                composite(
                    &mut c,
                    &sc,
                    dpaint_core::doc::raster::BlendMode::Normal,
                    1.0,
                    &Coverage::Buffer(&scov),
                    ctx.seed,
                );
            }
            c
        }
        LayerKind::Text { spec, fill, stroke } => {
            let l = crate::text::layout(spec, &ctx.fonts)?;
            let mut c = Canvas::new(w, h);
            if let Some(sk) = geom::to_sk(&l.outline, at) {
                let cov = geom::fill_coverage(&sk, w, h, FillRule::Nonzero);
                let fill_c = paint::paint_canvas(fill, w, h, ctx.scale, &|d, tw, th| (ctx.link)(d, tw, th))?;
                composite(
                    &mut c,
                    &fill_c,
                    dpaint_core::doc::raster::BlendMode::Normal,
                    1.0,
                    &Coverage::Buffer(&cov),
                    ctx.seed,
                );
                if let Some(s) = stroke {
                    let scov = geom::stroke_coverage(&sk, s, ctx.scale, w, h);
                    let sc = paint::paint_canvas(&s.paint, w, h, ctx.scale, &|d, tw, th| (ctx.link)(d, tw, th))?;
                    composite(
                        &mut c,
                        &sc,
                        dpaint_core::doc::raster::BlendMode::Normal,
                        1.0,
                        &Coverage::Buffer(&scov),
                        ctx.seed,
                    );
                }
            }
            c
        }
        LayerKind::Group { layers } => {
            let mut c = Canvas::new(w, h);
            composite_stack(layers, &mut c, ctx)?;
            if identity {
                c
            } else {
                c.transformed(at, w, h)
            }
        }
        LayerKind::Adjustment { .. } => Canvas::new(w, h),
        LayerKind::Linked { document, fit, r#box } => {
            render_linked(document, *fit, *r#box, at, ctx)?
        }
    };

    if let Some(cov) = layer_mask_coverage(layer, ctx)? {
        mask_alpha(&mut content, &cov);
    }
    if !layer.effects.is_empty() {
        content = effects::apply(&content, &layer.effects, ctx.scale);
    }
    Ok(content)
}

fn render_linked(
    document: &DocId,
    fit: Fit,
    r#box: dpaint_core::doc::common::Rect,
    at: dpaint_core::kurbo::Affine,
    ctx: &Ctx<'_>,
) -> Result<Canvas> {
    use dpaint_core::kurbo::Affine;
    if r#box.is_empty() {
        return Err(Error::Invalid(format!(
            "linked layer for {document} has an empty box {:?}",
            r#box.0
        )));
    }
    let target_w = (r#box.w() * ctx.scale).round().max(1.0) as u32;
    let target_h = (r#box.h() * ctx.scale).round().max(1.0) as u32;
    let pm = (ctx.link)(document, target_w, target_h)?;
    let src = Canvas::from_pixmap(pm.as_ref());
    let (sw, sh) = (src.width as f64, src.height as f64);
    // Map the rendered image onto the layer box per the fit mode, in document units.
    let (sx, sy) = (r#box.w() / sw, r#box.h() / sh);
    let (kx, ky, ox, oy) = match fit {
        Fit::Stretch => (sx, sy, 0.0, 0.0),
        // "None" means native pixels: one pixel of the render is one device pixel.
        Fit::None => (1.0 / ctx.scale, 1.0 / ctx.scale, 0.0, 0.0),
        Fit::Contain => {
            let k = sx.min(sy);
            (k, k, (r#box.w() - sw * k) / 2.0, (r#box.h() - sh * k) / 2.0)
        }
        Fit::Cover => {
            let k = sx.max(sy);
            (k, k, (r#box.w() - sw * k) / 2.0, (r#box.h() - sh * k) / 2.0)
        }
    };
    let map = at
        * Affine::translate((r#box.x() + ox, r#box.y() + oy))
        * Affine::scale_non_uniform(kx, ky);
    let mut out = src.transformed(map, ctx.width, ctx.height);
    if fit == Fit::Cover {
        // Cover overflows the box by construction; clip it back.
        let box_path = geom::rect_path(r#box.x(), r#box.y(), r#box.w(), r#box.h());
        if let Some(sk) = geom::to_sk(&box_path, at) {
            let cov = geom::fill_coverage(&sk, ctx.width, ctx.height, FillRule::Nonzero);
            mask_alpha(&mut out, &cov);
        }
    }
    Ok(out)
}

/// A link resolver that renders *raster* documents in this project, and says plainly that
/// vector and model documents need `dpaint-render`'s resolver. Ops use this;
/// `dpaint-render` passes its own resolver to [`render_doc`].
pub fn raster_only_link<'a>(
    project: &'a Project,
    assets: &'a AssetStore,
) -> impl Fn(&DocId, u32, u32) -> Result<Pixmap> + 'a {
    move |id: &DocId, w: u32, h: u32| render_link(project, assets, id, w, h, 0)
}

/// Depth-limited so a raster document that links itself fails as a cycle instead of
/// recursing until the stack dies.
const MAX_LINK_DEPTH: usize = 8;

fn render_link(
    project: &Project,
    assets: &AssetStore,
    id: &DocId,
    w: u32,
    h: u32,
    depth: usize,
) -> Result<Pixmap> {
    if depth >= MAX_LINK_DEPTH {
        return Err(Error::CyclicLink { from: id.to_string(), to: id.to_string() });
    }
    let doc = project.doc(id)?;
    let rd = doc.as_raster().ok_or_else(|| {
        Error::Invalid(format!(
            "{id} is a {} document; rendering it needs dpaint-render's link resolver",
            doc.kind()
        ))
    })?;
    // The requested size is a resolution hint, not a crop: render at the largest scale that
    // fits inside it so the document's aspect ratio survives and `Fit` can do its job.
    let sx = w as f64 / rd.width().max(1) as f64;
    let sy = h as f64 / rd.height().max(1) as f64;
    let scale = sx.min(sy).max(1e-6);
    let inner = |d: &DocId, tw: u32, th: u32| render_link(project, assets, d, tw, th, depth + 1);
    Ok(render_canvas(project, id, assets, scale, &inner)?.to_pixmap())
}
