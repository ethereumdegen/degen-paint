//! `raster.select.*` — the selection is the scope of every other raster edit.
//!
//! Rectangles, ellipses and paths are stored as **outlines** so they stay
//! resolution-independent; wand, color-range and alpha selections are stored as coverage
//! blobs, because there is no honest vector form of "these pixels are close to that color".

use super::support::{self, raster_op};
use crate::canvas::Canvas;
use crate::geom;
use crate::select::{self, SelMask};
use dpaint_core::color::Color;
use dpaint_core::doc::common::Rect;
use dpaint_core::doc::raster::{LayerKind, Selection};
use dpaint_core::kurbo::Shape;
use dpaint_core::{DocId, Error, OpCx, OpEffect, Project, RasterDoc, Result};

/// How a new selection combines with the current one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Combine {
    /// Discard the old selection.
    #[default]
    Replace,
    /// Union.
    Add,
    /// Remove the new region from the old one.
    Subtract,
    /// Keep only the overlap.
    Intersect,
}

fn current(rd: &RasterDoc, assets: &dpaint_core::AssetStore) -> Result<Option<SelMask>> {
    select::resolve(rd, assets, rd.width(), rd.height(), 1.0)
}

fn combine(old: Option<SelMask>, new: SelMask, mode: Combine) -> SelMask {
    let Some(old) = old else {
        return match mode {
            Combine::Subtract => SelMask::new(new.width, new.height, 0.0),
            Combine::Intersect => SelMask::new(new.width, new.height, 0.0),
            _ => new,
        };
    };
    let mut out = new;
    for i in 0..out.cov.len() {
        let (a, b) = (old.cov[i], out.cov[i]);
        out.cov[i] = match mode {
            Combine::Replace => b,
            Combine::Add => (a + b - a * b).clamp(0.0, 1.0),
            Combine::Subtract => (a * (1.0 - b)).clamp(0.0, 1.0),
            Combine::Intersect => a * b,
        };
    }
    out
}

/// Store a coverage-based selection, combining it with the current one.
fn commit_mask(
    project: &mut Project,
    doc: &DocId,
    cx: &mut OpCx,
    new: SelMask,
    mode: Combine,
) -> Result<OpEffect> {
    let rd = project.raster(doc)?;
    let merged = combine(current(rd, cx.assets)?, new, mode);
    let empty = merged.is_empty();
    let area = merged.area();
    if !cx.dry_run {
        let asset = cx.assets.put(&merged.to_png()?, "png")?;
        let bounds = merged.bounds();
        let rd = project.raster_mut(doc)?;
        rd.selection = Some(Selection {
            d: None,
            mask: Some(asset),
            feather: 0.0,
            inverted: false,
            bounds,
        });
    }
    let mut effect = OpEffect::changed(doc)
        .with_data(serde_json::json!({ "area_px": area, "bounds": merged.bounds().0 }));
    if empty {
        effect = effect.warn("empty-selection", doc.to_string(), "the result selects no pixels");
    }
    Ok(effect)
}

/// Store an outline selection when it can stay vector, otherwise fall back to coverage.
fn commit_outline(
    project: &mut Project,
    doc: &DocId,
    cx: &mut OpCx,
    d: String,
    feather: f64,
    mode: Combine,
) -> Result<OpEffect> {
    let rd = project.raster(doc)?;
    let (w, h) = (rd.width(), rd.height());
    let path = geom::parse_d(&d)?;
    if path.elements().is_empty() {
        return Err(Error::DegenerateGeometry("selection path is empty".into()));
    }
    if mode == Combine::Replace {
        let bb = path.bounding_box();
        if !cx.dry_run {
            let rd = project.raster_mut(doc)?;
            select::store_outline(
                rd,
                d,
                Rect::new(bb.x0, bb.y0, bb.width(), bb.height()),
                feather,
            );
        }
        return Ok(OpEffect::changed(doc)
            .with_data(serde_json::json!({ "bounds": [bb.x0, bb.y0, bb.width(), bb.height()] })));
    }
    let mut mask = select::mask_from_d(&d, w, h, 1.0)?;
    if feather > 0.0 {
        mask.feather(feather as f32);
    }
    commit_mask(project, doc, cx, mask, mode)
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct RectArgs {
    /// `[x, y, width, height]` in document pixels.
    pub rect: [f64; 4],
    /// Corner radius in pixels.
    #[serde(default)]
    pub radius: f64,
    /// Soften the edge by this many pixels.
    #[serde(default)]
    pub feather: f64,
    #[serde(default)]
    pub mode: Combine,
}

fn rect(project: &mut Project, a: RectArgs, cx: &mut OpCx) -> Result<OpEffect> {
    let doc = support::doc_id(project, cx)?;
    if a.rect[2] <= 0.0 || a.rect[3] <= 0.0 {
        return Err(Error::Invalid(format!("selection rect {:?} has no area", a.rect)));
    }
    let path = if a.radius > 0.0 {
        let r = a.radius.min(a.rect[2] / 2.0).min(a.rect[3] / 2.0);
        dpaint_core::kurbo::RoundedRect::new(
            a.rect[0],
            a.rect[1],
            a.rect[0] + a.rect[2],
            a.rect[1] + a.rect[3],
            r,
        )
        .to_path(0.05)
    } else {
        geom::rect_path(a.rect[0], a.rect[1], a.rect[2], a.rect[3])
    };
    commit_outline(project, &doc, cx, path.to_svg(), a.feather, a.mode)
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct EllipseArgs {
    /// Center `[x, y]` in document pixels.
    pub center: [f64; 2],
    /// Radii `[rx, ry]` in pixels.
    pub radius: [f64; 2],
    #[serde(default)]
    pub feather: f64,
    #[serde(default)]
    pub mode: Combine,
}

fn ellipse(project: &mut Project, a: EllipseArgs, cx: &mut OpCx) -> Result<OpEffect> {
    let doc = support::doc_id(project, cx)?;
    if a.radius[0] <= 0.0 || a.radius[1] <= 0.0 {
        return Err(Error::Invalid("ellipse radii must be positive".into()));
    }
    let path = geom::ellipse_path(a.center[0], a.center[1], a.radius[0], a.radius[1]);
    commit_outline(project, &doc, cx, path.to_svg(), a.feather, a.mode)
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct PathArgs {
    /// SVG path data in document coordinates.
    pub d: String,
    #[serde(default)]
    pub feather: f64,
    #[serde(default)]
    pub mode: Combine,
}

fn path(project: &mut Project, a: PathArgs, cx: &mut OpCx) -> Result<OpEffect> {
    let doc = support::doc_id(project, cx)?;
    commit_outline(project, &doc, cx, a.d, a.feather, a.mode)
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ColorRangeArgs {
    /// Color to match.
    pub color: Color,
    /// Distance that still counts as a match, 0..=1 of the full RGB+alpha range.
    #[serde(default = "tenth")]
    pub tolerance: f32,
    /// Extra distance over which coverage falls off to zero.
    #[serde(default)]
    pub fuzz: f32,
    /// Layer to sample. Defaults to the flattened composite.
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub mode: Combine,
}

fn tenth() -> f32 {
    0.1
}

fn source_canvas(
    project: &Project,
    doc: &DocId,
    cx: &OpCx,
    source: &Option<String>,
) -> Result<Canvas> {
    match source {
        Some(sel) => {
            let m = dpaint_core::resolve_one(project, sel, Some(doc))?;
            support::layer_canvas(project, doc, &dpaint_core::LayerId::from(m.id), cx.assets)
        }
        None => support::flatten_canvas(project, doc, cx.assets),
    }
}

fn color_range(project: &mut Project, a: ColorRangeArgs, cx: &mut OpCx) -> Result<OpEffect> {
    let doc = support::doc_id(project, cx)?;
    let c = source_canvas(project, &doc, cx, &a.source)?;
    let target = a.color.to_linear();
    let mask = select::color_range(&c, target, a.tolerance, a.fuzz);
    commit_mask(project, &doc, cx, mask, a.mode)
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct WandArgs {
    /// Seed pixel `[x, y]`.
    pub at: [u32; 2],
    /// Color distance that still counts as the same region, 0..=1.
    #[serde(default = "tenth")]
    pub tolerance: f32,
    /// Only select the region connected to the seed.
    #[serde(default = "yes")]
    pub contiguous: bool,
    /// Layer to sample. Defaults to the flattened composite.
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub mode: Combine,
}

fn yes() -> bool {
    true
}

fn wand(project: &mut Project, a: WandArgs, cx: &mut OpCx) -> Result<OpEffect> {
    let doc = support::doc_id(project, cx)?;
    let c = source_canvas(project, &doc, cx, &a.source)?;
    if a.at[0] >= c.width || a.at[1] >= c.height {
        return Err(Error::Invalid(format!(
            "wand seed {:?} is outside the {}x{} document",
            a.at, c.width, c.height
        )));
    }
    let mask = select::wand(&c, a.at[0], a.at[1], a.tolerance, a.contiguous);
    commit_mask(project, &doc, cx, mask, a.mode)
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct AlphaArgs {
    /// Layer whose alpha becomes the selection.
    pub target: String,
    #[serde(default)]
    pub mode: Combine,
}

fn alpha(project: &mut Project, a: AlphaArgs, cx: &mut OpCx) -> Result<OpEffect> {
    let (doc, id) = support::one_layer(project, cx, &a.target)?;
    let c = support::layer_canvas(project, &doc, &id, cx.assets)?;
    commit_mask(project, &doc, cx, select::from_alpha(&c), a.mode)
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct TextArgs {
    /// Text layer whose glyph outlines become the selection.
    pub target: String,
    #[serde(default)]
    pub feather: f64,
    #[serde(default)]
    pub mode: Combine,
}

fn text(project: &mut Project, a: TextArgs, cx: &mut OpCx) -> Result<OpEffect> {
    let (doc, id) = support::one_layer(project, cx, &a.target)?;
    let rd = project.raster(&doc)?;
    let layer = support::layer_of(rd, &id)?;
    let LayerKind::Text { spec, .. } = &layer.kind else {
        return Err(Error::Invalid(format!("'{}' is not a text layer", layer.name)));
    };
    let fonts = crate::text::FontSet::new(project, cx.assets);
    let l = crate::text::layout(spec, &fonts)?;
    if l.outline.elements().is_empty() {
        return Err(Error::DegenerateGeometry("that text has no glyph outlines".into()));
    }
    // Text sits in the layer's own space, so bring it into document space first.
    let placed = dpaint_core::kurbo::Affine::new(layer.transform.0) * l.outline.clone();
    let requested = spec.family.clone();
    let mut effect = commit_outline(project, &doc, cx, placed.to_svg(), a.feather, a.mode)?;
    if l.fallback {
        effect = effect.warn(
            "font-fallback",
            id.to_string(),
            format!("'{requested}' is not available; used '{}'", l.used_family),
        );
    }
    Ok(effect)
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct NoArgs {}

fn all(project: &mut Project, _a: NoArgs, cx: &mut OpCx) -> Result<OpEffect> {
    let doc = support::doc_id(project, cx)?;
    let rd = project.raster(&doc)?;
    let (w, h) = (rd.width() as f64, rd.height() as f64);
    let d = geom::rect_path(0.0, 0.0, w, h).to_svg();
    commit_outline(project, &doc, cx, d, 0.0, Combine::Replace)
}

fn none(project: &mut Project, _a: NoArgs, cx: &mut OpCx) -> Result<OpEffect> {
    let doc = support::doc_id(project, cx)?;
    if !cx.dry_run {
        project.raster_mut(&doc)?.selection = None;
    }
    Ok(OpEffect::changed(&doc))
}

fn invert(project: &mut Project, _a: NoArgs, cx: &mut OpCx) -> Result<OpEffect> {
    let doc = support::doc_id(project, cx)?;
    let rd = project.raster(&doc)?;
    if rd.selection.is_none() {
        // Inverting "everything" is "nothing", and that is worth saying out loud.
        return Err(Error::Invalid(
            "there is no selection to invert; select something first, or use select.none".into(),
        ));
    }
    if !cx.dry_run {
        let rd = project.raster_mut(&doc)?;
        if let Some(s) = rd.selection.as_mut() {
            s.inverted = !s.inverted;
        }
    }
    Ok(OpEffect::changed(&doc))
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct AmountArgs {
    /// Distance in pixels.
    pub pixels: u32,
}

fn existing(project: &Project, doc: &DocId, cx: &OpCx) -> Result<SelMask> {
    let rd = project.raster(doc)?;
    current(rd, cx.assets)?
        .ok_or_else(|| Error::Invalid("there is no selection to modify".into()))
}

fn grow(project: &mut Project, a: AmountArgs, cx: &mut OpCx) -> Result<OpEffect> {
    let doc = support::doc_id(project, cx)?;
    let mut mask = existing(project, &doc, cx)?;
    mask.morph(a.pixels, true);
    commit_mask(project, &doc, cx, mask, Combine::Replace)
}

fn shrink(project: &mut Project, a: AmountArgs, cx: &mut OpCx) -> Result<OpEffect> {
    let doc = support::doc_id(project, cx)?;
    let mut mask = existing(project, &doc, cx)?;
    mask.morph(a.pixels, false);
    commit_mask(project, &doc, cx, mask, Combine::Replace)
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct FeatherArgs {
    /// Feather radius in pixels.
    pub radius: f64,
}

fn feather(project: &mut Project, a: FeatherArgs, cx: &mut OpCx) -> Result<OpEffect> {
    let doc = support::doc_id(project, cx)?;
    let rd = project.raster(&doc)?;
    let sel = rd
        .selection
        .clone()
        .ok_or_else(|| Error::Invalid("there is no selection to feather".into()))?;
    if a.radius < 0.0 {
        return Err(Error::Invalid("feather radius cannot be negative".into()));
    }
    // An outline selection keeps its vector form and just records the feather; a coverage
    // selection has to be blurred for real.
    if sel.mask.is_none() && sel.d.is_some() {
        if !cx.dry_run {
            if let Some(s) = project.raster_mut(&doc)?.selection.as_mut() {
                s.feather = a.radius;
            }
        }
        return Ok(OpEffect::changed(&doc));
    }
    let mut mask = existing(project, &doc, cx)?;
    mask.feather(a.radius as f32);
    commit_mask(project, &doc, cx, mask, Combine::Replace)
}

fn to_path(project: &mut Project, _a: NoArgs, cx: &mut OpCx) -> Result<OpEffect> {
    let doc = support::doc_id(project, cx)?;
    let rd = project.raster(&doc)?;
    let sel = rd
        .selection
        .clone()
        .ok_or_else(|| Error::Invalid("there is no selection to convert".into()))?;
    if let Some(d) = sel.d.filter(|_| sel.mask.is_none() && !sel.inverted) {
        // Already vector: hand it back rather than round-tripping through pixels.
        return Ok(OpEffect::changed(&doc).with_data(serde_json::json!({ "d": d })));
    }
    let mask = existing(project, &doc, cx)?;
    let d = select::to_path_d(&mask);
    if d.is_empty() {
        return Err(Error::DegenerateGeometry("the selection traced to an empty path".into()));
    }
    let bb = geom::parse_d(&d)?.bounding_box();
    if !cx.dry_run {
        let rd = project.raster_mut(&doc)?;
        select::store_outline(
            rd,
            d.clone(),
            Rect::new(bb.x0, bb.y0, bb.width(), bb.height()),
            0.0,
        );
    }
    Ok(OpEffect::changed(&doc).with_data(serde_json::json!({ "d": d })))
}

raster_op!(SelectRect, "raster.select.rect", "Select a rectangle, optionally rounded", RectArgs, rect);
raster_op!(SelectEllipse, "raster.select.ellipse", "Select an ellipse", EllipseArgs, ellipse);
raster_op!(SelectPath, "raster.select.path", "Select the interior of SVG path data", PathArgs, path);
raster_op!(SelectColorRange, "raster.select.color-range", "Select pixels near a color", ColorRangeArgs, color_range);
raster_op!(SelectWand, "raster.select.wand", "Flood-select a region from a seed pixel", WandArgs, wand);
raster_op!(SelectAlpha, "raster.select.alpha", "Select a layer's alpha", AlphaArgs, alpha);
raster_op!(SelectText, "raster.select.text", "Select a text layer's glyph outlines", TextArgs, text);
raster_op!(SelectAll, "raster.select.all", "Select the whole document", NoArgs, all);
raster_op!(SelectNone, "raster.select.none", "Clear the selection", NoArgs, none);
raster_op!(SelectInvert, "raster.select.invert", "Invert the selection", NoArgs, invert);
raster_op!(SelectGrow, "raster.select.grow", "Expand the selection by a number of pixels", AmountArgs, grow);
raster_op!(SelectShrink, "raster.select.shrink", "Contract the selection by a number of pixels", AmountArgs, shrink);
raster_op!(SelectFeather, "raster.select.feather", "Soften the selection edge", FeatherArgs, feather);
raster_op!(SelectToPath, "raster.select.to-path", "Trace the selection into SVG path data", NoArgs, to_path);

pub fn ops() -> Vec<Box<dyn dpaint_core::Op>> {
    vec![
        Box::new(SelectRect),
        Box::new(SelectEllipse),
        Box::new(SelectPath),
        Box::new(SelectColorRange),
        Box::new(SelectWand),
        Box::new(SelectAlpha),
        Box::new(SelectText),
        Box::new(SelectAll),
        Box::new(SelectNone),
        Box::new(SelectInvert),
        Box::new(SelectGrow),
        Box::new(SelectShrink),
        Box::new(SelectFeather),
        Box::new(SelectToPath),
    ]
}
