//! Rasterizing a vector document with `tiny-skia`.
//!
//! Geometry stays in document units and the device transform is handed to `tiny-skia`, so
//! stroke widths, dash phases and gradients all scale with the render instead of being
//! baked at one resolution.

use crate::geom;
use dpaint_core::asset::AssetStore;
use dpaint_core::color::Color;
use dpaint_core::doc::common::{
    BlendMode, FillRule, LineCap, LineJoin, Paint as DocPaint, Stroke as DocStroke,
};
use dpaint_core::doc::vector::{VKind, VObject, VectorDoc};
use dpaint_core::error::{Error, Result};
use dpaint_core::ids::DocId;
use dpaint_core::kurbo::{Affine, Rect as KRect, Shape};
use dpaint_core::project::Project;
use tiny_skia::{
    FillRule as SkFillRule, GradientStop, LinearGradient, Mask, MaskType, Paint, Pattern, Pixmap,
    PixmapPaint, Point as SkPoint, RadialGradient, Shader, SpreadMode, Stroke as SkStroke,
    StrokeDash, Transform as SkTransform,
};

/// Render a vector document to a premultiplied sRGB pixmap at `scale` device pixels per
/// document unit.
pub fn render_doc(
    project: &Project,
    doc: &DocId,
    assets: &AssetStore,
    scale: f64,
) -> Result<Pixmap> {
    render_at_depth(project, doc, assets, scale, 0)
}

fn render_at_depth(
    project: &Project,
    doc: &DocId,
    assets: &AssetStore,
    scale: f64,
    depth: u32,
) -> Result<Pixmap> {
    if !(scale > 0.0) {
        return Err(Error::Invalid(format!("render scale must be positive, got {scale}")));
    }
    let v = project.vector(doc)?;
    let bounds = doc_bounds(v);
    let w = ((bounds.width() * scale).ceil() as u32).max(1);
    let h = ((bounds.height() * scale).ceil() as u32).max(1);
    if w > 32_768 || h > 32_768 {
        return Err(Error::Invalid(format!(
            "render of {w}x{h} exceeds the 32768 px limit; lower the scale"
        )));
    }
    let mut pm = Pixmap::new(w, h)
        .ok_or_else(|| Error::Invalid(format!("cannot allocate a {w}x{h} pixmap")))?;
    let device = SkTransform::from_row(
        scale as f32,
        0.0,
        0.0,
        scale as f32,
        (-bounds.x0 * scale) as f32,
        (-bounds.y0 * scale) as f32,
    );
    for ab in &v.artboards {
        if let Some(bg) = ab.background {
            let r = ab.rect.to_kurbo();
            if let Some(path) = geom::to_skia(&r.to_path(1e-3), Affine::IDENTITY) {
                let mut p = Paint::default();
                p.shader = Shader::SolidColor(sk_color(bg, 1.0));
                p.anti_alias = true;
                pm.fill_path(&path, &p, SkFillRule::Winding, device, None);
            }
        }
    }
    let cx = Ctx {
        project,
        assets,
        device,
        depth,
    };
    draw_objects(&mut pm, &v.objects, v, &cx, Affine::IDENTITY, None)?;
    Ok(pm)
}

/// The union of every artboard, which is the document's canvas.
pub fn doc_bounds(v: &VectorDoc) -> KRect {
    let mut r: Option<KRect> = None;
    for ab in &v.artboards {
        let k = ab.rect.to_kurbo();
        r = Some(match r {
            Some(p) => p.union(k),
            None => k,
        });
    }
    r.filter(|r| r.width() > 0.0 && r.height() > 0.0)
        .unwrap_or(KRect::new(0.0, 0.0, 1.0, 1.0))
}

struct Ctx<'a> {
    project: &'a Project,
    assets: &'a AssetStore,
    device: SkTransform,
    depth: u32,
}

fn draw_objects(
    pm: &mut Pixmap,
    objects: &[VObject],
    v: &VectorDoc,
    cx: &Ctx,
    parent: Affine,
    mask: Option<&Mask>,
) -> Result<()> {
    for o in objects {
        draw_object(pm, o, v, cx, parent, mask)?;
    }
    Ok(())
}

fn draw_object(
    pm: &mut Pixmap,
    o: &VObject,
    v: &VectorDoc,
    cx: &Ctx,
    parent: Affine,
    inherited: Option<&Mask>,
) -> Result<()> {
    if !o.visible || o.opacity <= 0.0 {
        return Ok(());
    }
    let world = parent * o.transform.to_kurbo();

    // Clip path and luminance mask both narrow what this object may paint.
    let own_mask = build_mask(pm.width(), pm.height(), o, v, cx, inherited)?;
    let mask_ref = own_mask.as_ref().or(inherited);

    let needs_layer = !o.blend.is_normal()
        || (o.opacity < 1.0 && matches!(o.kind, VKind::Group { .. }))
        || (o.opacity < 1.0 && o.fill != DocPaint::None && o.stroke.is_some());

    if needs_layer {
        let mut layer = Pixmap::new(pm.width(), pm.height())
            .ok_or_else(|| Error::Invalid("cannot allocate a compositing layer".into()))?;
        paint_object(&mut layer, o, v, cx, world, mask_ref, 1.0)?;
        let mut pp = PixmapPaint::default();
        pp.opacity = o.opacity.clamp(0.0, 1.0);
        pp.blend_mode = sk_blend(o.blend);
        pm.draw_pixmap(0, 0, layer.as_ref(), &pp, SkTransform::identity(), None);
        return Ok(());
    }
    paint_object(pm, o, v, cx, world, mask_ref, o.opacity)
}

fn paint_object(
    pm: &mut Pixmap,
    o: &VObject,
    v: &VectorDoc,
    cx: &Ctx,
    world: Affine,
    mask: Option<&Mask>,
    alpha: f32,
) -> Result<()> {
    if let VKind::Group { objects } = &o.kind {
        return draw_objects(pm, objects, v, cx, world, mask);
    }
    if let VKind::Image { asset, rect } = &o.kind {
        draw_image(pm, cx, asset, *rect, world, mask, alpha)?;
        // An image can still carry a stroke around its frame.
    }
    let local = geom::local_path(v, o)?;
    if local.elements().is_empty() {
        return Ok(());
    }
    let doc_path = world * local;
    let Some(path) = geom::to_skia(&doc_path, Affine::IDENTITY) else {
        return Ok(());
    };
    let bbox = doc_path.bounding_box();

    let fill_pm = doc_pixmap(&o.fill, bbox, cx)?;
    if !matches!(o.kind, VKind::Image { .. }) {
        if let Some(shader) = shader_for(&o.fill, bbox, alpha, fill_pm.as_ref()) {
            let mut p = Paint::default();
            p.shader = shader;
            p.anti_alias = true;
            pm.fill_path(&path, &p, sk_rule(o.fill_rule), cx.device, mask);
        }
    }
    if let Some(stroke) = &o.stroke {
        if stroke.width > 0.0 {
            let stroke_pm = doc_pixmap(&stroke.paint, bbox, cx)?;
            if let Some(shader) = shader_for(&stroke.paint, bbox, alpha, stroke_pm.as_ref()) {
                let mut p = Paint::default();
                p.shader = shader;
                p.anti_alias = true;
                pm.stroke_path(&path, &p, &sk_stroke(stroke), cx.device, mask);
            }
        }
    }
    Ok(())
}

/// A clip path narrows to its filled region; a mask object narrows by its luminance.
fn build_mask(
    w: u32,
    h: u32,
    o: &VObject,
    v: &VectorDoc,
    cx: &Ctx,
    inherited: Option<&Mask>,
) -> Result<Option<Mask>> {
    if o.clip.is_none() && o.mask.is_none() {
        return Ok(None);
    }
    let mut m = match inherited {
        Some(prev) => prev.clone(),
        None => {
            let mut full = Mask::new(w, h).ok_or_else(|| Error::Invalid("cannot allocate mask".into()))?;
            full.fill_path(
                &tiny_skia::PathBuilder::from_rect(
                    tiny_skia::Rect::from_xywh(0.0, 0.0, w as f32, h as f32).unwrap(),
                ),
                SkFillRule::Winding,
                false,
                SkTransform::identity(),
            );
            full
        }
    };
    if let Some(clip) = &o.clip {
        let cp = geom::path_in_doc(v, clip)?;
        let Some(skp) = geom::to_skia(&cp, Affine::IDENTITY) else {
            return Ok(None);
        };
        m.intersect_path(&skp, SkFillRule::Winding, true, cx.device);
    }
    if let Some(mask_id) = &o.mask {
        let Some((mobj, parent)) = geom::locate(v, mask_id) else {
            return Err(Error::Invalid(format!("mask object '{mask_id}' not found")));
        };
        let mut layer = Pixmap::new(w, h)
            .ok_or_else(|| Error::Invalid("cannot allocate a mask layer".into()))?;
        paint_object(&mut layer, mobj, v, cx, parent * mobj.transform.to_kurbo(), None, 1.0)?;
        let lum = Mask::from_pixmap(layer.as_ref(), MaskType::Luminance);
        let dst = m.data_mut();
        for (d, s) in dst.iter_mut().zip(lum.data().iter()) {
            *d = ((*d as u16 * *s as u16) / 255) as u8;
        }
    }
    Ok(Some(m))
}

fn draw_image(
    pm: &mut Pixmap,
    cx: &Ctx,
    asset: &dpaint_core::asset::AssetRef,
    rect: dpaint_core::doc::common::Rect,
    world: Affine,
    mask: Option<&Mask>,
    alpha: f32,
) -> Result<()> {
    let bytes = cx.assets.get(asset)?;
    let img = image::load_from_memory(&bytes)
        .map_err(|e| Error::AssetDecode(format!("{asset}: {e}")))?
        .to_rgba8();
    let (iw, ih) = img.dimensions();
    let mut src = Pixmap::new(iw.max(1), ih.max(1))
        .ok_or_else(|| Error::AssetDecode(format!("{asset}: zero-sized image")))?;
    for (px, dst) in img.pixels().zip(src.pixels_mut()) {
        let [r, g, b, a] = px.0;
        *dst = tiny_skia::ColorU8::from_rgba(r, g, b, a).premultiply();
    }
    let r = rect.to_kurbo();
    if r.width() <= 0.0 || r.height() <= 0.0 {
        return Ok(());
    }
    let place = world
        * Affine::translate((r.x0, r.y0))
        * Affine::scale_non_uniform(r.width() / iw as f64, r.height() / ih as f64);
    let c = place.as_coeffs();
    let shader = Pattern::new(
        src.as_ref(),
        SpreadMode::Pad,
        tiny_skia::FilterQuality::Bilinear,
        alpha.clamp(0.0, 1.0),
        SkTransform::from_row(
            c[0] as f32,
            c[1] as f32,
            c[2] as f32,
            c[3] as f32,
            c[4] as f32,
            c[5] as f32,
        ),
    );
    let outline = world * r.to_path(1e-3);
    let Some(path) = geom::to_skia(&outline, Affine::IDENTITY) else {
        return Ok(());
    };
    let mut p = Paint::default();
    p.shader = shader;
    p.anti_alias = true;
    pm.fill_path(&path, &p, SkFillRule::Winding, cx.device, mask);
    Ok(())
}

/// A `Paint::Document` fill renders the referenced vector document once, here, so the
/// pattern shader can borrow it for the length of the draw call.
fn doc_pixmap(paint: &DocPaint, bbox: KRect, cx: &Ctx) -> Result<Option<Pixmap>> {
    let DocPaint::Document { document } = paint else {
        return Ok(None);
    };
    if cx.depth >= 4 {
        return Err(Error::CyclicLink {
            from: document.to_string(),
            to: document.to_string(),
        });
    }
    // Raster and model documents are composited by `dpaint-render`, which owns the
    // cross-mode bridge; this crate paints nothing for them.
    let Ok(inner) = cx.project.vector(document) else {
        return Ok(None);
    };
    let ib = doc_bounds(inner);
    let px = (bbox.width().max(1.0) / ib.width().max(1e-6)).clamp(0.05, 16.0);
    Ok(Some(render_at_depth(
        cx.project,
        document,
        cx.assets,
        px,
        cx.depth + 1,
    )?))
}

fn shader_for<'a>(
    paint: &DocPaint,
    bbox: KRect,
    alpha: f32,
    doc_pm: Option<&'a Pixmap>,
) -> Option<Shader<'a>> {
    match paint {
        DocPaint::None => None,
        DocPaint::Solid { color } => Some(Shader::SolidColor(sk_color(*color, alpha))),
        DocPaint::Linear { stops, from, to } => LinearGradient::new(
            SkPoint::from_xy(from[0] as f32, from[1] as f32),
            SkPoint::from_xy(to[0] as f32, to[1] as f32),
            sk_stops(stops, alpha),
            SpreadMode::Pad,
            SkTransform::identity(),
        ),
        DocPaint::Radial {
            stops,
            center,
            radius,
            focal,
        } => {
            let c = SkPoint::from_xy(center[0] as f32, center[1] as f32);
            let f = focal
                .map(|f| SkPoint::from_xy(f[0] as f32, f[1] as f32))
                .unwrap_or(c);
            RadialGradient::new(
                f,
                0.0,
                c,
                (*radius as f32).max(1e-4),
                sk_stops(stops, alpha),
                SpreadMode::Pad,
                SkTransform::identity(),
            )
        }
        DocPaint::Document { .. } => {
            let rendered = doc_pm?;
            let place = Affine::translate((bbox.x0, bbox.y0))
                * Affine::scale_non_uniform(
                    bbox.width() / rendered.width().max(1) as f64,
                    bbox.height() / rendered.height().max(1) as f64,
                );
            let c = place.as_coeffs();
            Some(Pattern::new(
                rendered.as_ref(),
                SpreadMode::Pad,
                tiny_skia::FilterQuality::Bilinear,
                alpha.clamp(0.0, 1.0),
                SkTransform::from_row(
                    c[0] as f32,
                    c[1] as f32,
                    c[2] as f32,
                    c[3] as f32,
                    c[4] as f32,
                    c[5] as f32,
                ),
            ))
        }
    }
}

fn sk_stops(stops: &[dpaint_core::doc::common::GradientStop], alpha: f32) -> Vec<GradientStop> {
    let mut out: Vec<GradientStop> = stops
        .iter()
        .map(|s| GradientStop::new(s.offset.clamp(0.0, 1.0) as f32, sk_color(s.color, alpha)))
        .collect();
    if out.is_empty() {
        out.push(GradientStop::new(0.0, sk_color(Color::TRANSPARENT, alpha)));
        out.push(GradientStop::new(1.0, sk_color(Color::TRANSPARENT, alpha)));
    }
    if out.len() == 1 {
        let only = stops[0].color;
        out.push(GradientStop::new(1.0, sk_color(only, alpha)));
    }
    out
}

pub fn sk_color(c: Color, alpha: f32) -> tiny_skia::Color {
    tiny_skia::Color::from_rgba(
        c.r.clamp(0.0, 1.0),
        c.g.clamp(0.0, 1.0),
        c.b.clamp(0.0, 1.0),
        (c.a * alpha).clamp(0.0, 1.0),
    )
    .unwrap_or(tiny_skia::Color::TRANSPARENT)
}

pub fn sk_rule(r: FillRule) -> SkFillRule {
    match r {
        FillRule::Nonzero => SkFillRule::Winding,
        FillRule::Evenodd => SkFillRule::EvenOdd,
    }
}

fn sk_stroke(s: &DocStroke) -> SkStroke {
    let mut out = SkStroke::default();
    out.width = s.width as f32;
    out.miter_limit = s.miter.max(1.0) as f32;
    out.line_cap = match s.cap {
        LineCap::Butt => tiny_skia::LineCap::Butt,
        LineCap::Round => tiny_skia::LineCap::Round,
        LineCap::Square => tiny_skia::LineCap::Square,
    };
    out.line_join = match s.join {
        LineJoin::Miter => tiny_skia::LineJoin::Miter,
        LineJoin::Round => tiny_skia::LineJoin::Round,
        LineJoin::Bevel => tiny_skia::LineJoin::Bevel,
    };
    if !s.dash.is_empty() && s.dash.iter().any(|d| *d > 0.0) {
        let mut arr: Vec<f32> = s.dash.iter().map(|d| *d as f32).collect();
        if arr.len() % 2 == 1 {
            arr = arr.iter().chain(arr.iter()).copied().collect();
        }
        out.dash = StrokeDash::new(arr, s.dash_offset as f32);
    }
    out
}

/// `tiny-skia` implements the CSS/PDF separable and non-separable set. The extras in
/// `BlendMode` that it does not implement fall back to their closest relative rather than
/// silently painting nothing.
pub fn sk_blend(b: BlendMode) -> tiny_skia::BlendMode {
    use tiny_skia::BlendMode as B;
    match b {
        BlendMode::Normal | BlendMode::Dissolve => B::SourceOver,
        BlendMode::Multiply | BlendMode::LinearBurn | BlendMode::DarkerColor => B::Multiply,
        BlendMode::Screen | BlendMode::LinearDodge | BlendMode::LighterColor => B::Screen,
        BlendMode::Overlay => B::Overlay,
        BlendMode::Darken => B::Darken,
        BlendMode::Lighten => B::Lighten,
        BlendMode::ColorDodge => B::ColorDodge,
        BlendMode::ColorBurn => B::ColorBurn,
        BlendMode::HardLight | BlendMode::VividLight | BlendMode::LinearLight | BlendMode::PinLight
        | BlendMode::HardMix => B::HardLight,
        BlendMode::SoftLight => B::SoftLight,
        BlendMode::Difference | BlendMode::Subtract => B::Difference,
        BlendMode::Exclusion | BlendMode::Divide => B::Exclusion,
        BlendMode::Hue => B::Hue,
        BlendMode::Saturation => B::Saturation,
        BlendMode::Color => B::Color,
        BlendMode::Luminosity => B::Luminosity,
    }
}

/// Count of pixels whose alpha exceeds `min_alpha`. Tests measure coverage with this.
pub fn covered(pm: &Pixmap, min_alpha: u8) -> usize {
    pm.pixels().iter().filter(|p| p.alpha() > min_alpha).count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use dpaint_core::doc::common::{GradientStop as DocStop, Paint as P, Rect, Stroke};
    use dpaint_core::doc::Document;
    use dpaint_core::ids::ObjectId;

    fn project_with(objects: Vec<VObject>) -> (Project, DocId, tempfile::TempDir) {
        let mut v = VectorDoc::new(DocId::from("doc_v"), "v", 100.0, 100.0);
        v.objects = objects;
        let id = v.id.clone();
        let p = Project::new("t", Document::Vector(v));
        let tmp = tempfile::tempdir().unwrap();
        (p, id, tmp)
    }

    fn red_square() -> VObject {
        VObject::new(
            ObjectId::from("obj_sq"),
            "sq",
            VKind::Rect {
                rect: Rect::new(10.0, 10.0, 40.0, 40.0),
                radius: 0.0,
            },
        )
        .with_fill(P::solid(Color::parse("#ff0000").unwrap()))
    }

    #[test]
    fn scale_four_quadruples_the_pixel_size_and_keeps_proportions() {
        let (p, id, tmp) = project_with(vec![red_square()]);
        let assets = AssetStore::new(tmp.path());
        let a = render_doc(&p, &id, &assets, 1.0).unwrap();
        let b = render_doc(&p, &id, &assets, 4.0).unwrap();
        assert_eq!((a.width(), a.height()), (100, 100));
        assert_eq!((b.width(), b.height()), (400, 400));
        let ca = covered(&a, 128) as f64;
        let cb = covered(&b, 128) as f64;
        assert!(
            (cb / ca - 16.0).abs() < 0.2,
            "coverage scales with the area: {ca} -> {cb}"
        );
    }

    #[test]
    fn a_filled_rectangle_covers_exactly_its_area() {
        let (p, id, tmp) = project_with(vec![red_square()]);
        let pm = render_doc(&p, &id, &AssetStore::new(tmp.path()), 1.0).unwrap();
        assert!((covered(&pm, 128) as f64 - 1600.0).abs() < 100.0, "{}", covered(&pm, 128));
        let px = pm.pixel(20, 20).unwrap();
        assert!(px.red() > 200 && px.green() < 40, "the fill is red: {px:?}");
    }

    #[test]
    fn the_even_odd_rule_punches_a_hole_the_nonzero_rule_does_not() {
        let mut ring = VObject::new(
            ObjectId::from("obj_ring"),
            "ring",
            VKind::Path {
                d: "M 0 0 L 60 0 L 60 60 L 0 60 Z M 20 20 L 40 20 L 40 40 L 20 40 Z".into(),
            },
        )
        .with_fill(P::solid(Color::BLACK));
        ring.fill_rule = FillRule::Evenodd;
        let (p, id, tmp) = project_with(vec![ring.clone()]);
        let holed = render_doc(&p, &id, &AssetStore::new(tmp.path()), 1.0).unwrap();
        ring.fill_rule = FillRule::Nonzero;
        let (p2, id2, tmp2) = project_with(vec![ring]);
        let solid = render_doc(&p2, &id2, &AssetStore::new(tmp2.path()), 1.0).unwrap();
        assert!(
            covered(&solid, 128) - covered(&holed, 128) > 350,
            "even-odd removes the 20x20 hole: {} vs {}",
            covered(&solid, 128),
            covered(&holed, 128)
        );
    }

    #[test]
    fn a_linear_gradient_paints_different_colours_across_the_shape() {
        let mut o = red_square();
        o.fill = P::Linear {
            stops: vec![
                DocStop {
                    offset: 0.0,
                    color: Color::parse("#000000").unwrap(),
                },
                DocStop {
                    offset: 1.0,
                    color: Color::parse("#ffffff").unwrap(),
                },
            ],
            from: [10.0, 0.0],
            to: [50.0, 0.0],
        };
        let (p, id, tmp) = project_with(vec![o]);
        let pm = render_doc(&p, &id, &AssetStore::new(tmp.path()), 1.0).unwrap();
        let left = pm.pixel(12, 30).unwrap().red();
        let right = pm.pixel(48, 30).unwrap().red();
        assert!(right > left + 150, "gradient ramps: {left} -> {right}");
    }

    #[test]
    fn opacity_reduces_coverage_alpha() {
        let mut o = red_square();
        o.opacity = 0.5;
        let (p, id, tmp) = project_with(vec![o]);
        let pm = render_doc(&p, &id, &AssetStore::new(tmp.path()), 1.0).unwrap();
        let a = pm.pixel(20, 20).unwrap().alpha();
        assert!((a as i32 - 128).abs() < 6, "alpha {a} near half");
    }

    #[test]
    fn a_clip_path_limits_what_is_painted() {
        let clip = VObject::new(
            ObjectId::from("obj_clip"),
            "clip",
            VKind::Rect {
                rect: Rect::new(10.0, 10.0, 20.0, 40.0),
                radius: 0.0,
            },
        );
        let mut sq = red_square();
        sq.clip = Some(ObjectId::from("obj_clip"));
        let (p, id, tmp) = project_with(vec![clip, sq]);
        let pm = render_doc(&p, &id, &AssetStore::new(tmp.path()), 1.0).unwrap();
        let c = covered(&pm, 128);
        assert!((c as i64 - 800).abs() < 60, "only the clipped half paints: {c}");
    }

    #[test]
    fn a_dashed_stroke_paints_less_than_a_solid_one() {
        let mut line = VObject::new(
            ObjectId::from("obj_l"),
            "l",
            VKind::Line {
                from: [10.0, 50.0],
                to: [90.0, 50.0],
            },
        );
        line.stroke = Some(Stroke::solid(Color::BLACK, 4.0));
        let (p, id, tmp) = project_with(vec![line.clone()]);
        let solid = covered(
            &render_doc(&p, &id, &AssetStore::new(tmp.path()), 1.0).unwrap(),
            128,
        );
        let mut dashed = line;
        if let Some(s) = dashed.stroke.as_mut() {
            s.dash = vec![8.0, 8.0];
        }
        let (p2, id2, tmp2) = project_with(vec![dashed]);
        let dash = covered(
            &render_doc(&p2, &id2, &AssetStore::new(tmp2.path()), 1.0).unwrap(),
            128,
        );
        assert!(dash * 2 < solid * 3 && dash < solid, "dashes paint gaps: {dash} < {solid}");
    }

    #[test]
    fn an_artboard_background_fills_the_canvas() {
        let (mut p, id, tmp) = project_with(vec![]);
        p.vector_mut(&id).unwrap().artboards[0].background = Some(Color::parse("#00ff00").unwrap());
        let pm = render_doc(&p, &id, &AssetStore::new(tmp.path()), 1.0).unwrap();
        assert_eq!(covered(&pm, 250), 100 * 100);
        assert!(pm.pixel(1, 1).unwrap().green() > 200);
    }
}
