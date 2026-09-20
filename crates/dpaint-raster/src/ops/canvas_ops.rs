//! `raster.canvas.*` — document geometry and page setup.

use super::support::{self, raster_op};
use crate::canvas::Canvas;
use dpaint_core::color::Color;
use dpaint_core::doc::common::Transform;
use dpaint_core::doc::raster::{Guides, LayerKind};
use dpaint_core::kurbo::Affine;
use dpaint_core::{DocId, Error, LayerId, OpCx, OpEffect, Project, Result};

/// Apply a document-space affine to every root layer and set a new document size.
///
/// Pixel layers are resampled through the map (integer translations, flips and quarter turns
/// land exactly on pixel centers, so those stay lossless); everything else composes the map
/// into its own transform, which keeps text and shapes resolution-independent.
fn remap(
    project: &mut Project,
    doc: &DocId,
    cx: &mut OpCx,
    map: Affine,
    nw: u32,
    nh: u32,
) -> Result<()> {
    if nw == 0 || nh == 0 {
        return Err(Error::Invalid("canvas size must be at least 1x1".into()));
    }
    let rd = project.raster(doc)?;
    let ids: Vec<LayerId> = rd.layers.iter().map(|l| l.id.clone()).collect();
    let mut updates: Vec<(LayerId, dpaint_core::AssetRef)> = Vec::new();
    for id in &ids {
        let layer = support::layer_of(rd, id)?;
        if let LayerKind::Pixel { asset, offset } = &layer.kind {
            let src = Canvas::from_png(&cx.assets.get(asset)?)?;
            let m = map * Affine::translate((offset[0] as f64, offset[1] as f64));
            let out = src.transformed(m, nw, nh);
            updates.push((id.clone(), support::store_canvas(cx.assets, &out)?));
        }
    }
    if cx.dry_run {
        return Ok(());
    }
    let t = Transform::from_kurbo(map);
    let rd = project.raster_mut(doc)?;
    for (id, asset) in updates {
        support::set_pixels(rd, &id, asset, [0, 0])?;
    }
    for id in &ids {
        if let Some(layer) = rd.layer_mut(id) {
            if !matches!(layer.kind, LayerKind::Pixel { .. }) {
                layer.transform = layer.transform.then(t);
            }
        }
    }
    rd.size = [nw, nh];
    Ok(())
}

/// How `canvas.resize` treats existing content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum ResizeMode {
    /// Scale the artwork to the new size.
    #[default]
    Scale,
    /// Keep the artwork at its current scale and change the page around it.
    Extend,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Anchor {
    TopLeft,
    Top,
    TopRight,
    Left,
    #[default]
    Center,
    Right,
    BottomLeft,
    Bottom,
    BottomRight,
}

impl Anchor {
    fn factors(self) -> (f64, f64) {
        let fx = match self {
            Anchor::TopLeft | Anchor::Left | Anchor::BottomLeft => 0.0,
            Anchor::Top | Anchor::Center | Anchor::Bottom => 0.5,
            _ => 1.0,
        };
        let fy = match self {
            Anchor::TopLeft | Anchor::Top | Anchor::TopRight => 0.0,
            Anchor::Left | Anchor::Center | Anchor::Right => 0.5,
            _ => 1.0,
        };
        (fx, fy)
    }
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ResizeArgs {
    /// New document width in pixels.
    pub width: u32,
    /// New document height in pixels.
    pub height: u32,
    /// `scale` resamples the artwork; `extend` changes the page around it.
    #[serde(default)]
    pub mode: ResizeMode,
    /// Where existing artwork sits in the new page, for `extend`.
    #[serde(default)]
    pub anchor: Anchor,
}

fn resize(project: &mut Project, args: ResizeArgs, cx: &mut OpCx) -> Result<OpEffect> {
    let doc = support::doc_id(project, cx)?;
    let rd = project.raster(&doc)?;
    let (ow, oh) = (rd.width() as f64, rd.height() as f64);
    let had_selection = rd.selection.is_some();
    let map = match args.mode {
        ResizeMode::Scale => {
            Affine::scale_non_uniform(args.width as f64 / ow, args.height as f64 / oh)
        }
        ResizeMode::Extend => {
            let (fx, fy) = args.anchor.factors();
            Affine::translate((
                (args.width as f64 - ow) * fx,
                (args.height as f64 - oh) * fy,
            ))
        }
    };
    remap(project, &doc, cx, map, args.width, args.height)?;
    let mut effect = OpEffect::changed(&doc);
    if had_selection && !cx.dry_run {
        project.raster_mut(&doc)?.selection = None;
        effect = effect.warn(
            "selection-dropped",
            doc.to_string(),
            "the selection did not survive the resize and was cleared",
        );
    }
    Ok(effect)
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct CropArgs {
    /// Crop rectangle in document pixels, `[x, y, width, height]`.
    #[serde(default)]
    pub rect: Option<[f64; 4]>,
    /// Crop to the bounds of the current selection instead of an explicit rect.
    #[serde(default)]
    pub to_selection: bool,
}

fn crop(project: &mut Project, args: CropArgs, cx: &mut OpCx) -> Result<OpEffect> {
    let doc = support::doc_id(project, cx)?;
    let rd = project.raster(&doc)?;
    let rect = match (args.rect, args.to_selection) {
        (Some(r), _) => r,
        (None, true) => {
            let sel = rd
                .selection
                .as_ref()
                .ok_or_else(|| Error::Invalid("crop to-selection needs a selection".into()))?;
            let b = sel.bounds;
            if b.is_empty() {
                return Err(Error::Invalid("the current selection is empty".into()));
            }
            [b.x(), b.y(), b.w(), b.h()]
        }
        (None, false) => {
            return Err(Error::Invalid(
                "crop needs either rect or to-selection".into(),
            ))
        }
    };
    if rect[2] < 1.0 || rect[3] < 1.0 {
        return Err(Error::Invalid(format!("crop rect {rect:?} has no area")));
    }
    let (w, h) = (rect[2].round() as u32, rect[3].round() as u32);
    remap(
        project,
        &doc,
        cx,
        Affine::translate((-rect[0], -rect[1])),
        w,
        h,
    )?;
    if !cx.dry_run {
        project.raster_mut(&doc)?.selection = None;
    }
    Ok(OpEffect::changed(&doc))
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct TrimArgs {
    /// Alpha below this is treated as empty when measuring the content bounds.
    #[serde(default)]
    pub threshold: f32,
    /// Keep this many pixels of margin around the content.
    #[serde(default)]
    pub margin: u32,
}

fn trim(project: &mut Project, args: TrimArgs, cx: &mut OpCx) -> Result<OpEffect> {
    let doc = support::doc_id(project, cx)?;
    let flat = support::flatten_canvas(project, &doc, cx.assets)?;
    let Some((x, y, w, h)) = flat.opaque_bounds(args.threshold) else {
        return Err(Error::Invalid(
            "the document is empty; there is nothing to trim to".into(),
        ));
    };
    let m = args.margin as i64;
    let x0 = (x as i64 - m).max(0);
    let y0 = (y as i64 - m).max(0);
    let x1 = (x as i64 + w as i64 + m).min(flat.width as i64);
    let y1 = (y as i64 + h as i64 + m).min(flat.height as i64);
    remap(
        project,
        &doc,
        cx,
        Affine::translate((-x0 as f64, -y0 as f64)),
        (x1 - x0) as u32,
        (y1 - y0) as u32,
    )?;
    Ok(OpEffect::changed(&doc))
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct RotateArgs {
    /// Clockwise rotation in degrees. Multiples of 90 are lossless.
    pub degrees: f64,
}

fn rotate(project: &mut Project, args: RotateArgs, cx: &mut OpCx) -> Result<OpEffect> {
    let doc = support::doc_id(project, cx)?;
    let rd = project.raster(&doc)?;
    let (w, h) = (rd.width() as f64, rd.height() as f64);
    let deg = args.degrees.rem_euclid(360.0);
    let rot = Affine::rotate(deg.to_radians());
    // Rotate about the origin, then translate the rotated bounding box back to (0, 0).
    let corners = [(0.0, 0.0), (w, 0.0), (w, h), (0.0, h)];
    let mut minx = f64::MAX;
    let mut miny = f64::MAX;
    let mut maxx = f64::MIN;
    let mut maxy = f64::MIN;
    for (cx0, cy0) in corners {
        let p = rot * dpaint_core::kurbo::Point::new(cx0, cy0);
        minx = minx.min(p.x);
        miny = miny.min(p.y);
        maxx = maxx.max(p.x);
        maxy = maxy.max(p.y);
    }
    let nw = (maxx - minx).round().max(1.0);
    let nh = (maxy - miny).round().max(1.0);
    let map = Affine::translate((-minx, -miny)) * rot;
    remap(project, &doc, cx, map, nw as u32, nh as u32)?;
    let mut effect = OpEffect::changed(&doc);
    if deg % 90.0 != 0.0 {
        effect = effect.warn(
            "resampled",
            doc.to_string(),
            format!("{deg} degrees is not a quarter turn, so pixel layers were resampled"),
        );
    }
    Ok(effect)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum FlipAxis {
    Horizontal,
    Vertical,
    Both,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct FlipArgs {
    /// Which way to mirror the document.
    pub axis: FlipAxis,
}

fn flip(project: &mut Project, args: FlipArgs, cx: &mut OpCx) -> Result<OpEffect> {
    let doc = support::doc_id(project, cx)?;
    let rd = project.raster(&doc)?;
    let (w, h) = (rd.width() as f64, rd.height() as f64);
    let (sx, sy) = match args.axis {
        FlipAxis::Horizontal => (-1.0, 1.0),
        FlipAxis::Vertical => (1.0, -1.0),
        FlipAxis::Both => (-1.0, -1.0),
    };
    let map = Affine::translate((
        if sx < 0.0 { w } else { 0.0 },
        if sy < 0.0 { h } else { 0.0 },
    )) * Affine::scale_non_uniform(sx, sy);
    remap(project, &doc, cx, map, w as u32, h as u32)?;
    Ok(OpEffect::changed(&doc))
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct DpiArgs {
    /// Pixels per inch, used by print export and physical measurements.
    pub dpi: f32,
}

fn set_dpi(project: &mut Project, args: DpiArgs, cx: &mut OpCx) -> Result<OpEffect> {
    if !(args.dpi.is_finite() && args.dpi > 0.0) {
        return Err(Error::Invalid(format!(
            "dpi must be positive, got {}",
            args.dpi
        )));
    }
    let doc = support::doc_id(project, cx)?;
    if !cx.dry_run {
        project.raster_mut(&doc)?.dpi = args.dpi;
    }
    Ok(OpEffect::changed(&doc))
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct BackgroundArgs {
    /// Background color, e.g. `#ffffff`. Omit to make the document transparent.
    #[serde(default)]
    pub color: Option<Color>,
}

fn set_background(project: &mut Project, args: BackgroundArgs, cx: &mut OpCx) -> Result<OpEffect> {
    let doc = support::doc_id(project, cx)?;
    if !cx.dry_run {
        project.raster_mut(&doc)?.background = args.color;
    }
    Ok(OpEffect::changed(&doc))
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct GuidesArgs {
    /// Bleed margin in pixels.
    #[serde(default)]
    pub bleed: Option<f64>,
    /// Safe-area margin in pixels.
    #[serde(default)]
    pub safe: Option<f64>,
    /// Vertical guide positions (x coordinates).
    #[serde(default)]
    pub vertical: Option<Vec<f64>>,
    /// Horizontal guide positions (y coordinates).
    #[serde(default)]
    pub horizontal: Option<Vec<f64>>,
}

fn set_guides(project: &mut Project, args: GuidesArgs, cx: &mut OpCx) -> Result<OpEffect> {
    let doc = support::doc_id(project, cx)?;
    if cx.dry_run {
        return Ok(OpEffect::changed(&doc));
    }
    let rd = project.raster_mut(&doc)?;
    let g = Guides {
        bleed: args.bleed.unwrap_or(rd.guides.bleed),
        safe: args.safe.unwrap_or(rd.guides.safe),
        vertical: args.vertical.unwrap_or_else(|| rd.guides.vertical.clone()),
        horizontal: args
            .horizontal
            .unwrap_or_else(|| rd.guides.horizontal.clone()),
    };
    rd.guides = g;
    Ok(OpEffect::changed(&doc))
}

raster_op!(
    Resize,
    "raster.canvas.resize",
    "Resize the document, scaling or extending its content",
    ResizeArgs,
    resize
);
raster_op!(
    Crop,
    "raster.canvas.crop",
    "Crop the document to a rectangle or to the selection",
    CropArgs,
    crop
);
raster_op!(
    Trim,
    "raster.canvas.trim",
    "Trim transparent margins away from the document",
    TrimArgs,
    trim
);
raster_op!(
    Rotate,
    "raster.canvas.rotate",
    "Rotate the whole document clockwise",
    RotateArgs,
    rotate
);
raster_op!(
    Flip,
    "raster.canvas.flip",
    "Mirror the document horizontally, vertically or both",
    FlipArgs,
    flip
);
raster_op!(
    SetDpi,
    "raster.canvas.set-dpi",
    "Set the document resolution in pixels per inch",
    DpiArgs,
    set_dpi
);
raster_op!(
    SetBackground,
    "raster.canvas.set-background",
    "Set or clear the document background color",
    BackgroundArgs,
    set_background
);
raster_op!(
    SetGuides,
    "raster.canvas.set-guides",
    "Set bleed, safe area and guide positions",
    GuidesArgs,
    set_guides
);

pub fn ops() -> Vec<Box<dyn dpaint_core::Op>> {
    vec![
        Box::new(Resize),
        Box::new(Crop),
        Box::new(Trim),
        Box::new(Rotate),
        Box::new(Flip),
        Box::new(SetDpi),
        Box::new(SetBackground),
        Box::new(SetGuides),
    ]
}
