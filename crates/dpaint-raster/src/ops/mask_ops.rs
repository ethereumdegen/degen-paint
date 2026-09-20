//! `raster.mask.*` and `raster.clip.set` — layer masks and clipping.

use super::support::{self, raster_op};
use crate::canvas::{encode_gray_png, Canvas};
use crate::composite;
use crate::select;
use dpaint_core::color::linear_to_srgb;
use dpaint_core::doc::raster::Mask;
use dpaint_core::{AssetRef, Error, OpCx, OpEffect, Project, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum MaskSource {
    /// Fully revealing mask (white).
    #[default]
    White,
    /// Fully hiding mask (black).
    Black,
    /// The current selection's coverage.
    Selection,
    /// The layer's own luminance.
    Luminance,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct AddArgs {
    /// Layer to mask.
    pub target: String,
    /// What the new mask starts as.
    #[serde(default)]
    pub from: MaskSource,
    /// Use an existing grayscale blob as the mask instead of `from`.
    #[serde(default)]
    pub asset: Option<AssetRef>,
    /// Invert the mask's meaning.
    #[serde(default)]
    pub inverted: bool,
}

fn build_mask_asset(
    project: &Project,
    doc: &dpaint_core::DocId,
    layer: &dpaint_core::LayerId,
    from: MaskSource,
    cx: &OpCx,
) -> Result<AssetRef> {
    let rd = project.raster(doc)?;
    let (w, h) = (rd.width(), rd.height());
    let cov: Vec<f32> = match from {
        MaskSource::White => vec![1.0; w as usize * h as usize],
        MaskSource::Black => vec![0.0; w as usize * h as usize],
        MaskSource::Selection => {
            let sel = select::resolve(rd, cx.assets, w, h, 1.0)?.ok_or_else(|| {
                Error::Invalid("there is no selection to build a mask from".into())
            })?;
            sel.cov
        }
        MaskSource::Luminance => {
            let c = support::layer_canvas(project, doc, layer, cx.assets)?;
            luminance_cov(&c)
        }
    };
    cx.assets.put(&encode_gray_png(w, h, &cov)?, "png")
}

fn luminance_cov(c: &Canvas) -> Vec<f32> {
    (0..c.pixel_count())
        .map(|i| {
            let s = c.straight(i * 4);
            // Unlit (transparent) pixels contribute nothing, which is what "make a mask from
            // this layer's brightness" has to mean for a layer with holes in it.
            linear_to_srgb((0.2126 * s[0] + 0.7152 * s[1] + 0.0722 * s[2]).clamp(0.0, 1.0)) * s[3]
        })
        .collect()
}

fn add(project: &mut Project, args: AddArgs, cx: &mut OpCx) -> Result<OpEffect> {
    let (doc, id) = support::one_layer(project, cx, &args.target)?;
    let asset = match args.asset {
        Some(a) => {
            if !cx.assets.contains(&a) {
                return Err(Error::AssetMissing(a.0.clone()));
            }
            a
        }
        None => build_mask_asset(project, &doc, &id, args.from, cx)?,
    };
    if !cx.dry_run {
        if let Some(l) = project.raster_mut(&doc)?.layer_mut(&id) {
            l.mask = Some(Mask {
                asset,
                enabled: true,
                inverted: args.inverted,
                offset: [0, 0],
            });
        }
    }
    Ok(OpEffect::changed(&doc))
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct TargetArgs {
    /// Layer to act on.
    pub target: String,
}

fn remove(project: &mut Project, args: TargetArgs, cx: &mut OpCx) -> Result<OpEffect> {
    let (doc, id) = support::one_layer(project, cx, &args.target)?;
    let rd = project.raster(&doc)?;
    if support::layer_of(rd, &id)?.mask.is_none() {
        return Err(Error::Invalid("that layer has no mask".into()));
    }
    if !cx.dry_run {
        if let Some(l) = project.raster_mut(&doc)?.layer_mut(&id) {
            l.mask = None;
        }
    }
    Ok(OpEffect::changed(&doc))
}

/// Bake the mask into the layer's pixels and drop it.
fn apply(project: &mut Project, args: TargetArgs, cx: &mut OpCx) -> Result<OpEffect> {
    let (doc, id) = support::one_layer(project, cx, &args.target)?;
    let rd = project.raster(&doc)?;
    let layer = support::layer_of(rd, &id)?;
    let mask = layer
        .mask
        .clone()
        .ok_or_else(|| Error::Invalid("that layer has no mask to apply".into()))?;
    let (old, offset) = support::load_pixel(rd, &id, cx.assets)?;
    let cov = composite::mask_coverage(&mask, cx.assets, rd.width(), rd.height(), 1.0)?;
    let mut out = old.clone();
    for y in 0..out.height {
        for x in 0..out.width {
            let (dx, dy) = (x as i64 + offset[0] as i64, y as i64 + offset[1] as i64);
            let m = if dx < 0 || dy < 0 || dx >= rd.width() as i64 || dy >= rd.height() as i64 {
                0.0
            } else {
                cov[dy as usize * rd.width() as usize + dx as usize]
            };
            let i = out.idx(x, y);
            for c in 0..4 {
                out.data[i + c] *= m;
            }
        }
    }
    let asset = support::store_canvas(cx.assets, &out)?;
    if !cx.dry_run {
        let rd = project.raster_mut(&doc)?;
        support::set_pixels(rd, &id, asset, offset)?;
        if let Some(l) = rd.layer_mut(&id) {
            l.mask = None;
        }
    }
    Ok(OpEffect::changed(&doc))
}

fn invert(project: &mut Project, args: TargetArgs, cx: &mut OpCx) -> Result<OpEffect> {
    let (doc, id) = support::one_layer(project, cx, &args.target)?;
    let rd = project.raster(&doc)?;
    if support::layer_of(rd, &id)?.mask.is_none() {
        return Err(Error::Invalid("that layer has no mask to invert".into()));
    }
    if !cx.dry_run {
        if let Some(m) = project
            .raster_mut(&doc)?
            .layer_mut(&id)
            .and_then(|l| l.mask.as_mut())
        {
            m.inverted = !m.inverted;
        }
    }
    Ok(OpEffect::changed(&doc))
}

fn from_selection(project: &mut Project, args: TargetArgs, cx: &mut OpCx) -> Result<OpEffect> {
    let (doc, id) = support::one_layer(project, cx, &args.target)?;
    let asset = build_mask_asset(project, &doc, &id, MaskSource::Selection, cx)?;
    if !cx.dry_run {
        if let Some(l) = project.raster_mut(&doc)?.layer_mut(&id) {
            l.mask = Some(Mask {
                asset,
                enabled: true,
                inverted: false,
                offset: [0, 0],
            });
        }
    }
    Ok(OpEffect::changed(&doc))
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct FromLuminanceArgs {
    /// Layer that receives the mask.
    pub target: String,
    /// Layer whose luminance becomes the mask. Defaults to `target` itself.
    #[serde(default)]
    pub source: Option<String>,
    /// Invert the resulting mask.
    #[serde(default)]
    pub inverted: bool,
}

fn from_luminance(
    project: &mut Project,
    args: FromLuminanceArgs,
    cx: &mut OpCx,
) -> Result<OpEffect> {
    let (doc, id) = support::one_layer(project, cx, &args.target)?;
    let source = match &args.source {
        Some(sel) => support::one_layer(project, cx, sel)?.1,
        None => id.clone(),
    };
    let asset = build_mask_asset(project, &doc, &source, MaskSource::Luminance, cx)?;
    if !cx.dry_run {
        if let Some(l) = project.raster_mut(&doc)?.layer_mut(&id) {
            l.mask = Some(Mask {
                asset,
                enabled: true,
                inverted: args.inverted,
                offset: [0, 0],
            });
        }
    }
    Ok(OpEffect::changed(&doc))
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ClipArgs {
    /// Layer(s) to clip.
    pub target: String,
    /// `true` clips to the layer beneath; `false` releases the clip.
    #[serde(default = "yes")]
    pub clip: bool,
}

fn yes() -> bool {
    true
}

fn clip_set(project: &mut Project, args: ClipArgs, cx: &mut OpCx) -> Result<OpEffect> {
    let (doc, ids) = support::many_layers(project, cx, &args.target)?;
    let rd = project.raster(&doc)?;
    // Clipping to nothing is a mistake worth reporting rather than silently ignoring.
    for id in &ids {
        let (path, index) = support::locate(rd, id)
            .ok_or_else(|| Error::Invalid(format!("layer {id} is not in the stack")))?;
        if args.clip && index == 0 {
            let where_ = if path.is_empty() {
                "the stack"
            } else {
                "its group"
            };
            return Err(Error::Invalid(format!(
                "'{}' is at the bottom of {where_}; there is no layer beneath it to clip to",
                support::layer_of(rd, id)?.name
            )));
        }
    }
    if cx.dry_run {
        return Ok(OpEffect::changed(&doc));
    }
    let rd = project.raster_mut(&doc)?;
    for id in &ids {
        if let Some(l) = rd.layer_mut(id) {
            l.clip = args.clip;
        }
    }
    Ok(OpEffect::changed(&doc))
}

raster_op!(
    Add,
    "raster.mask.add",
    "Add a layer mask from white, black, the selection or luminance",
    AddArgs,
    add
);
raster_op!(
    Remove,
    "raster.mask.remove",
    "Remove a layer mask",
    TargetArgs,
    remove
);
raster_op!(
    Apply,
    "raster.mask.apply",
    "Bake a layer mask into the layer's pixels",
    TargetArgs,
    apply
);
raster_op!(
    Invert,
    "raster.mask.invert",
    "Invert a layer mask",
    TargetArgs,
    invert
);
raster_op!(
    FromSelection,
    "raster.mask.from-selection",
    "Replace a layer mask with the current selection",
    TargetArgs,
    from_selection
);
raster_op!(
    FromLuminance,
    "raster.mask.from-luminance",
    "Build a layer mask from a layer's luminance",
    FromLuminanceArgs,
    from_luminance
);
raster_op!(
    ClipSet,
    "raster.clip.set",
    "Clip a layer to the one beneath it, or release it",
    ClipArgs,
    clip_set
);

pub fn ops() -> Vec<Box<dyn dpaint_core::Op>> {
    vec![
        Box::new(Add),
        Box::new(Remove),
        Box::new(Apply),
        Box::new(Invert),
        Box::new(FromSelection),
        Box::new(FromLuminance),
        Box::new(ClipSet),
    ]
}
