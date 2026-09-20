//! `raster.adjust.*` — every `Adjustment` variant, destructively on a pixel layer or as a
//! non-destructive adjustment layer (`as-layer: true`).

use super::support::{self, raster_op, Scope};
use crate::adjust;
use dpaint_core::doc::raster::{Adjustment, Channel, DesaturateMode, Layer, LayerKind};
use dpaint_core::{AssetRef, Error, OpCx, OpEffect, Project, Result};

/// Shared tail of every adjust op: either bake it into the target's pixels through the
/// selection, or insert a live adjustment layer directly above the target.
fn run(
    project: &mut Project,
    target: &str,
    adjustment: Adjustment,
    scope: Scope,
    as_layer: bool,
    cx: &mut OpCx,
) -> Result<OpEffect> {
    let prepared = adjust::prepare(&adjustment, cx.assets)?;
    let (doc, ids) = support::many_layers(project, cx, target)?;
    if as_layer {
        let rd = project.raster(&doc)?;
        let base = ids
            .first()
            .ok_or_else(|| Error::Invalid("no layer matched".into()))?;
        let (path, index) = support::locate(rd, base)
            .ok_or_else(|| Error::Invalid(format!("layer {base} is not in the stack")))?;
        let name = adjustment_name(&adjustment);
        let id = support::fresh_id(rd, name);
        if cx.dry_run {
            return Ok(OpEffect::changed(&doc).with_created(id.to_string()));
        }
        let layer = Layer::new(id.clone(), name, LayerKind::Adjustment { adjustment });
        let rd = project.raster_mut(&doc)?;
        let list = support::list_at(rd, &path)
            .ok_or_else(|| Error::Invalid("layer parent vanished".into()))?;
        list.insert(index + 1, layer);
        return Ok(OpEffect::changed(&doc).with_created(id.to_string()));
    }
    for id in &ids {
        support::edit_pixels(project, &doc, id, cx, scope, |c| {
            let mut out = c.clone();
            adjust::apply_prepared(&mut out, &prepared);
            Ok(out)
        })?;
    }
    Ok(OpEffect::changed(&doc))
}

fn adjustment_name(a: &Adjustment) -> &'static str {
    match a {
        Adjustment::Curves { .. } => "Curves",
        Adjustment::Levels { .. } => "Levels",
        Adjustment::BrightnessContrast { .. } => "Brightness/Contrast",
        Adjustment::Hsl { .. } => "HSL",
        Adjustment::ColorBalance { .. } => "Color Balance",
        Adjustment::Exposure { .. } => "Exposure",
        Adjustment::ChannelMixer { .. } => "Channel Mixer",
        Adjustment::Threshold { .. } => "Threshold",
        Adjustment::Posterize { .. } => "Posterize",
        Adjustment::Invert => "Invert",
        Adjustment::Desaturate { .. } => "Desaturate",
        Adjustment::Lut { .. } => "LUT",
    }
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct CurvesArgs {
    /// Layer(s) to adjust.
    pub target: String,
    /// Which channel the curve applies to.
    #[serde(default)]
    pub channel: Channel,
    /// Control points `[input, output]` in 0..=1, ascending in input.
    pub points: Vec<[f64; 2]>,
    /// Whether to honor the current selection.
    #[serde(default)]
    pub scope: Scope,
    /// Insert a live adjustment layer above the target instead of baking pixels.
    #[serde(default)]
    pub as_layer: bool,
}

fn curves(project: &mut Project, a: CurvesArgs, cx: &mut OpCx) -> Result<OpEffect> {
    run(
        project,
        &a.target,
        Adjustment::Curves { channel: a.channel, points: a.points },
        a.scope,
        a.as_layer,
        cx,
    )
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct LevelsArgs {
    /// Layer(s) to adjust.
    pub target: String,
    #[serde(default)]
    pub channel: Channel,
    /// Input black point, 0..=1.
    #[serde(default)]
    pub in_black: f64,
    /// Input white point, 0..=1.
    #[serde(default = "one")]
    pub in_white: f64,
    /// Midtone gamma; >1 brightens.
    #[serde(default = "one")]
    pub gamma: f64,
    /// Output black point.
    #[serde(default)]
    pub out_black: f64,
    /// Output white point.
    #[serde(default = "one")]
    pub out_white: f64,
    #[serde(default)]
    pub scope: Scope,
    #[serde(default)]
    pub as_layer: bool,
}

fn one() -> f64 {
    1.0
}

fn levels(project: &mut Project, a: LevelsArgs, cx: &mut OpCx) -> Result<OpEffect> {
    run(
        project,
        &a.target,
        Adjustment::Levels {
            channel: a.channel,
            in_black: a.in_black,
            in_white: a.in_white,
            gamma: a.gamma,
            out_black: a.out_black,
            out_white: a.out_white,
        },
        a.scope,
        a.as_layer,
        cx,
    )
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct BrightnessContrastArgs {
    pub target: String,
    /// Added to every channel, -1..=1.
    #[serde(default)]
    pub brightness: f64,
    /// Contrast around mid-gray, -1..=8.
    #[serde(default)]
    pub contrast: f64,
    #[serde(default)]
    pub scope: Scope,
    #[serde(default)]
    pub as_layer: bool,
}

fn brightness_contrast(
    project: &mut Project,
    a: BrightnessContrastArgs,
    cx: &mut OpCx,
) -> Result<OpEffect> {
    run(
        project,
        &a.target,
        Adjustment::BrightnessContrast { brightness: a.brightness, contrast: a.contrast },
        a.scope,
        a.as_layer,
        cx,
    )
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct HslArgs {
    pub target: String,
    /// Hue rotation in degrees.
    #[serde(default)]
    pub hue: f64,
    /// Saturation change, -1..=1.
    #[serde(default)]
    pub saturation: f64,
    /// Lightness change, -1..=1.
    #[serde(default)]
    pub lightness: f64,
    #[serde(default)]
    pub scope: Scope,
    #[serde(default)]
    pub as_layer: bool,
}

fn hsl(project: &mut Project, a: HslArgs, cx: &mut OpCx) -> Result<OpEffect> {
    run(
        project,
        &a.target,
        Adjustment::Hsl { hue: a.hue, saturation: a.saturation, lightness: a.lightness },
        a.scope,
        a.as_layer,
        cx,
    )
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ColorBalanceArgs {
    pub target: String,
    /// RGB shift applied to shadows, each -1..=1.
    #[serde(default)]
    pub shadows: [f64; 3],
    /// RGB shift applied to midtones.
    #[serde(default)]
    pub midtones: [f64; 3],
    /// RGB shift applied to highlights.
    #[serde(default)]
    pub highlights: [f64; 3],
    #[serde(default)]
    pub scope: Scope,
    #[serde(default)]
    pub as_layer: bool,
}

fn color_balance(project: &mut Project, a: ColorBalanceArgs, cx: &mut OpCx) -> Result<OpEffect> {
    run(
        project,
        &a.target,
        Adjustment::ColorBalance {
            shadows: a.shadows,
            midtones: a.midtones,
            highlights: a.highlights,
        },
        a.scope,
        a.as_layer,
        cx,
    )
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ExposureArgs {
    pub target: String,
    /// Exposure change in stops; +1 doubles the light.
    #[serde(default)]
    pub stops: f64,
    /// Linear offset added after the stop change.
    #[serde(default)]
    pub offset: f64,
    #[serde(default)]
    pub scope: Scope,
    #[serde(default)]
    pub as_layer: bool,
}

fn exposure(project: &mut Project, a: ExposureArgs, cx: &mut OpCx) -> Result<OpEffect> {
    run(
        project,
        &a.target,
        Adjustment::Exposure { stops: a.stops, offset: a.offset },
        a.scope,
        a.as_layer,
        cx,
    )
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ChannelMixerArgs {
    pub target: String,
    /// Rows are output R, G, B; columns are input R, G, B.
    pub matrix: [[f64; 3]; 3],
    #[serde(default)]
    pub scope: Scope,
    #[serde(default)]
    pub as_layer: bool,
}

fn channel_mixer(project: &mut Project, a: ChannelMixerArgs, cx: &mut OpCx) -> Result<OpEffect> {
    run(project, &a.target, Adjustment::ChannelMixer { matrix: a.matrix }, a.scope, a.as_layer, cx)
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ThresholdArgs {
    pub target: String,
    /// Luminance cutoff, 0..=1.
    #[serde(default = "half")]
    pub level: f64,
    #[serde(default)]
    pub scope: Scope,
    #[serde(default)]
    pub as_layer: bool,
}

fn half() -> f64 {
    0.5
}

fn threshold(project: &mut Project, a: ThresholdArgs, cx: &mut OpCx) -> Result<OpEffect> {
    run(project, &a.target, Adjustment::Threshold { level: a.level }, a.scope, a.as_layer, cx)
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct PosterizeArgs {
    pub target: String,
    /// Number of tone steps per channel, at least 2.
    #[serde(default = "four")]
    pub levels: u32,
    #[serde(default)]
    pub scope: Scope,
    #[serde(default)]
    pub as_layer: bool,
}

fn four() -> u32 {
    4
}

fn posterize(project: &mut Project, a: PosterizeArgs, cx: &mut OpCx) -> Result<OpEffect> {
    if a.levels < 2 {
        return Err(Error::Invalid("posterize needs at least 2 levels".into()));
    }
    run(project, &a.target, Adjustment::Posterize { levels: a.levels }, a.scope, a.as_layer, cx)
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct PlainArgs {
    pub target: String,
    #[serde(default)]
    pub scope: Scope,
    #[serde(default)]
    pub as_layer: bool,
}

fn invert(project: &mut Project, a: PlainArgs, cx: &mut OpCx) -> Result<OpEffect> {
    run(project, &a.target, Adjustment::Invert, a.scope, a.as_layer, cx)
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct DesaturateArgs {
    pub target: String,
    /// How gray is computed: perceptual luminosity, channel average, or lightness.
    #[serde(default)]
    pub mode: DesaturateMode,
    #[serde(default)]
    pub scope: Scope,
    #[serde(default)]
    pub as_layer: bool,
}

fn desaturate(project: &mut Project, a: DesaturateArgs, cx: &mut OpCx) -> Result<OpEffect> {
    run(project, &a.target, Adjustment::Desaturate { mode: a.mode }, a.scope, a.as_layer, cx)
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct LutArgs {
    pub target: String,
    /// HALD-style color cube PNG in the asset store: a square image whose side squared is a
    /// perfect cube (64x64 is a 16-step cube, 512x512 a 64-step cube).
    pub asset: AssetRef,
    /// Blend between the original and the LUT result, 0..=1.
    #[serde(default = "one")]
    pub amount: f64,
    #[serde(default)]
    pub scope: Scope,
    #[serde(default)]
    pub as_layer: bool,
}

fn lut(project: &mut Project, a: LutArgs, cx: &mut OpCx) -> Result<OpEffect> {
    run(
        project,
        &a.target,
        Adjustment::Lut { asset: a.asset, amount: a.amount },
        a.scope,
        a.as_layer,
        cx,
    )
}

raster_op!(Curves, "raster.adjust.curves", "Apply a monotone tone curve", CurvesArgs, curves);
raster_op!(Levels, "raster.adjust.levels", "Remap black point, white point and gamma", LevelsArgs, levels);
raster_op!(BrightnessContrast, "raster.adjust.brightness-contrast", "Shift brightness and contrast", BrightnessContrastArgs, brightness_contrast);
raster_op!(Hsl, "raster.adjust.hsl", "Rotate hue and change saturation and lightness", HslArgs, hsl);
raster_op!(ColorBalance, "raster.adjust.color-balance", "Shift color per tonal range", ColorBalanceArgs, color_balance);
raster_op!(Exposure, "raster.adjust.exposure", "Change exposure in stops, in linear light", ExposureArgs, exposure);
raster_op!(ChannelMixer, "raster.adjust.channel-mixer", "Mix output channels from input channels", ChannelMixerArgs, channel_mixer);
raster_op!(Threshold, "raster.adjust.threshold", "Reduce to black and white at a luminance cutoff", ThresholdArgs, threshold);
raster_op!(Posterize, "raster.adjust.posterize", "Quantize tones to a number of steps", PosterizeArgs, posterize);
raster_op!(Invert, "raster.adjust.invert", "Invert colors", PlainArgs, invert);
raster_op!(Desaturate, "raster.adjust.desaturate", "Convert to gray", DesaturateArgs, desaturate);
raster_op!(Lut, "raster.adjust.lut", "Apply a HALD color lookup cube", LutArgs, lut);

pub fn ops() -> Vec<Box<dyn dpaint_core::Op>> {
    vec![
        Box::new(Curves),
        Box::new(Levels),
        Box::new(BrightnessContrast),
        Box::new(Hsl),
        Box::new(ColorBalance),
        Box::new(Exposure),
        Box::new(ChannelMixer),
        Box::new(Threshold),
        Box::new(Posterize),
        Box::new(Invert),
        Box::new(Desaturate),
        Box::new(Lut),
    ]
}
