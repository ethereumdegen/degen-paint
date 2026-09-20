//! `raster.filter.*` — pixel filters, always scoped by the current selection unless the
//! caller asks for `scope: whole`.

use super::support::{self, raster_op, Scope};
use crate::canvas::Canvas;
use crate::filters::{self, ChannelSlot, ChannelVerb, EdgeKernel, MorphOp, MorphShape, RadialMode};
use dpaint_core::{AssetRef, Error, OpCx, OpEffect, Project, Result};

/// Apply a pixel kernel to every matching layer.
fn each(
    project: &mut Project,
    target: &str,
    scope: Scope,
    cx: &mut OpCx,
    f: impl Fn(&Canvas) -> Result<Canvas>,
) -> Result<OpEffect> {
    let (doc, ids) = support::many_layers(project, cx, target)?;
    for id in &ids {
        support::edit_pixels(project, &doc, id, cx, scope, |c| f(c))?;
    }
    Ok(OpEffect::changed(&doc))
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct BlurArgs {
    /// Layer(s) to blur.
    pub target: String,
    /// Gaussian sigma in pixels; the kernel spans three sigma either side.
    pub sigma: f32,
    #[serde(default)]
    pub scope: Scope,
}

fn gaussian_blur(project: &mut Project, a: BlurArgs, cx: &mut OpCx) -> Result<OpEffect> {
    if !(a.sigma.is_finite() && a.sigma > 0.0) {
        return Err(Error::Invalid(format!("sigma must be positive, got {}", a.sigma)));
    }
    each(project, &a.target, a.scope, cx, |c| Ok(filters::gaussian_blur(c, a.sigma)))
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct BoxBlurArgs {
    pub target: String,
    /// Box radius in pixels.
    pub radius: u32,
    /// Repeat count; three iterations approximate a gaussian.
    #[serde(default = "one_u32")]
    pub iterations: u32,
    #[serde(default)]
    pub scope: Scope,
}

fn one_u32() -> u32 {
    1
}

fn box_blur(project: &mut Project, a: BoxBlurArgs, cx: &mut OpCx) -> Result<OpEffect> {
    if a.radius == 0 {
        return Err(Error::Invalid("box blur radius must be at least 1".into()));
    }
    each(project, &a.target, a.scope, cx, |c| Ok(filters::box_blur(c, a.radius, a.iterations)))
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct MotionBlurArgs {
    pub target: String,
    /// Streak length in pixels.
    pub distance: f32,
    /// Streak direction in degrees, 0 pointing right.
    #[serde(default)]
    pub angle: f32,
    #[serde(default)]
    pub scope: Scope,
}

fn motion_blur(project: &mut Project, a: MotionBlurArgs, cx: &mut OpCx) -> Result<OpEffect> {
    if a.distance <= 0.0 {
        return Err(Error::Invalid("motion blur distance must be positive".into()));
    }
    each(project, &a.target, a.scope, cx, |c| Ok(filters::motion_blur(c, a.distance, a.angle)))
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct RadialBlurArgs {
    pub target: String,
    /// `zoom` streaks outward, `spin` streaks around the center.
    pub mode: RadialMode,
    /// Strength: percent of the radius for zoom, degrees of arc for spin.
    pub amount: f32,
    /// Center in document pixels. Defaults to the document center.
    #[serde(default)]
    pub center: Option<[f32; 2]>,
    #[serde(default)]
    pub scope: Scope,
}

fn radial_blur(project: &mut Project, a: RadialBlurArgs, cx: &mut OpCx) -> Result<OpEffect> {
    if a.amount <= 0.0 {
        return Err(Error::Invalid("radial blur amount must be positive".into()));
    }
    let doc = support::doc_id(project, cx)?;
    let rd = project.raster(&doc)?;
    let center = a
        .center
        .unwrap_or([rd.width() as f32 / 2.0, rd.height() as f32 / 2.0]);
    each(project, &a.target, a.scope, cx, |c| Ok(filters::radial_blur(c, a.mode, a.amount, center)))
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct UnsharpArgs {
    pub target: String,
    /// Radius of the comparison blur, as a gaussian sigma.
    pub sigma: f32,
    /// How much of the difference to add back; 1.0 doubles local contrast.
    #[serde(default = "one_f32")]
    pub amount: f32,
    /// Ignore differences smaller than this (0..=1), so flat areas stay flat.
    #[serde(default)]
    pub threshold: f32,
    #[serde(default)]
    pub scope: Scope,
}

fn one_f32() -> f32 {
    1.0
}

fn unsharp(project: &mut Project, a: UnsharpArgs, cx: &mut OpCx) -> Result<OpEffect> {
    if a.sigma <= 0.0 {
        return Err(Error::Invalid("unsharp sigma must be positive".into()));
    }
    each(project, &a.target, a.scope, cx, |c| {
        Ok(filters::unsharp(c, a.sigma, a.amount, a.threshold))
    })
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SharpenArgs {
    pub target: String,
    /// Sharpening strength; 0 is a no-op.
    #[serde(default = "one_f32")]
    pub amount: f32,
    #[serde(default)]
    pub scope: Scope,
}

fn sharpen(project: &mut Project, a: SharpenArgs, cx: &mut OpCx) -> Result<OpEffect> {
    each(project, &a.target, a.scope, cx, |c| Ok(filters::sharpen(c, a.amount)))
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct NoiseArgs {
    pub target: String,
    /// Noise amplitude in display units, 0..=1.
    pub amount: f32,
    /// Same noise in every channel, so grain does not tint the image.
    #[serde(default)]
    pub monochrome: bool,
    /// Seed, so the same call always produces the same grain.
    #[serde(default)]
    pub seed: u64,
    #[serde(default)]
    pub scope: Scope,
}

fn noise_add(project: &mut Project, a: NoiseArgs, cx: &mut OpCx) -> Result<OpEffect> {
    if !(0.0..=1.0).contains(&a.amount) {
        return Err(Error::Invalid("noise amount must be in 0..=1".into()));
    }
    each(project, &a.target, a.scope, cx, |c| {
        Ok(filters::add_noise(c, a.amount, a.monochrome, a.seed))
    })
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct NoiseReduceArgs {
    pub target: String,
    /// Window radius in pixels.
    #[serde(default = "one_u32")]
    pub radius: u32,
    /// Only replace a pixel when it is within this much of the window median, which keeps
    /// real edges from being smeared.
    #[serde(default = "quarter")]
    pub threshold: f32,
    #[serde(default)]
    pub scope: Scope,
}

fn quarter() -> f32 {
    0.25
}

fn noise_reduce(project: &mut Project, a: NoiseReduceArgs, cx: &mut OpCx) -> Result<OpEffect> {
    if a.radius == 0 {
        return Err(Error::Invalid("noise-reduce radius must be at least 1".into()));
    }
    each(project, &a.target, a.scope, cx, |c| Ok(filters::noise_reduce(c, a.radius, a.threshold)))
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct PixelateArgs {
    pub target: String,
    /// Block size in pixels.
    pub size: u32,
    #[serde(default)]
    pub scope: Scope,
}

fn pixelate(project: &mut Project, a: PixelateArgs, cx: &mut OpCx) -> Result<OpEffect> {
    if a.size < 2 {
        return Err(Error::Invalid("pixelate size must be at least 2".into()));
    }
    each(project, &a.target, a.scope, cx, |c| Ok(filters::pixelate(c, a.size)))
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ConvolveArgs {
    pub target: String,
    /// Kernel width; must be odd.
    pub width: usize,
    /// Kernel height; must be odd.
    pub height: usize,
    /// Kernel values, row-major, `width * height` of them.
    pub kernel: Vec<f32>,
    /// Divisor applied after the sum. 0 means "use the kernel sum", which preserves brightness.
    #[serde(default)]
    pub divisor: f32,
    /// Constant added after dividing.
    #[serde(default)]
    pub bias: f32,
    #[serde(default)]
    pub scope: Scope,
}

fn convolve(project: &mut Project, a: ConvolveArgs, cx: &mut OpCx) -> Result<OpEffect> {
    // Validate the kernel once, before touching any pixels.
    filters::convolve(&Canvas::new(1, 1), a.width, a.height, &a.kernel, a.divisor, a.bias)?;
    each(project, &a.target, a.scope, cx, |c| {
        filters::convolve(c, a.width, a.height, &a.kernel, a.divisor, a.bias)
    })
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct MorphologyArgs {
    pub target: String,
    /// `dilate` grows coverage, `erode` shrinks it.
    pub op: MorphOp,
    /// Structuring element radius in pixels.
    pub radius: u32,
    /// Structuring element shape.
    #[serde(default)]
    pub shape: MorphShape,
    #[serde(default)]
    pub scope: Scope,
}

fn morphology(project: &mut Project, a: MorphologyArgs, cx: &mut OpCx) -> Result<OpEffect> {
    if a.radius == 0 {
        return Err(Error::Invalid("morphology radius must be at least 1".into()));
    }
    each(project, &a.target, a.scope, cx, |c| Ok(filters::morphology(c, a.op, a.radius, a.shape)))
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct DisplaceArgs {
    pub target: String,
    /// PNG in the asset store whose red channel drives x and green channel drives y,
    /// with 0.5 meaning "no shift".
    pub map: AssetRef,
    /// Maximum shift in pixels along x.
    pub scale_x: f32,
    /// Maximum shift in pixels along y. Defaults to `scale_x`.
    #[serde(default)]
    pub scale_y: Option<f32>,
    #[serde(default)]
    pub scope: Scope,
}

fn displace(project: &mut Project, a: DisplaceArgs, cx: &mut OpCx) -> Result<OpEffect> {
    let map = Canvas::from_png(&cx.assets.get(&a.map)?)?;
    let sy = a.scale_y.unwrap_or(a.scale_x);
    each(project, &a.target, a.scope, cx, |c| Ok(filters::displace(c, &map, a.scale_x, sy)))
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ChannelOpArgs {
    pub target: String,
    /// What to do to the destination channel.
    pub op: ChannelVerb,
    /// Channel read from.
    #[serde(default = "red")]
    pub source: ChannelSlot,
    /// Channel written to.
    #[serde(default = "alpha")]
    pub dest: ChannelSlot,
    /// Constant used by the `set` verb.
    #[serde(default)]
    pub value: f32,
    #[serde(default)]
    pub scope: Scope,
}

fn red() -> ChannelSlot {
    ChannelSlot::Red
}

fn alpha() -> ChannelSlot {
    ChannelSlot::Alpha
}

fn channel_op(project: &mut Project, a: ChannelOpArgs, cx: &mut OpCx) -> Result<OpEffect> {
    each(project, &a.target, a.scope, cx, |c| {
        Ok(filters::channel_op(c, a.op, a.source, a.dest, a.value))
    })
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct DitherArgs {
    pub target: String,
    /// Tone steps per channel after dithering.
    #[serde(default = "two")]
    pub levels: u32,
    /// Bayer matrix size: 2, 4 or 8.
    #[serde(default = "eight")]
    pub matrix: u32,
    #[serde(default)]
    pub scope: Scope,
}

fn two() -> u32 {
    2
}

fn eight() -> u32 {
    8
}

fn dither(project: &mut Project, a: DitherArgs, cx: &mut OpCx) -> Result<OpEffect> {
    if !matches!(a.matrix, 2 | 4 | 8) {
        return Err(Error::Invalid("dither matrix must be 2, 4 or 8".into()));
    }
    if a.levels < 2 {
        return Err(Error::Invalid("dither needs at least 2 levels".into()));
    }
    each(project, &a.target, a.scope, cx, |c| Ok(filters::dither(c, a.levels, a.matrix)))
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct EdgeArgs {
    pub target: String,
    /// Which gradient operator to use.
    #[serde(default = "sobel")]
    pub kernel: EdgeKernel,
    /// Gain applied to the edge magnitude.
    #[serde(default = "one_f32")]
    pub amount: f32,
    #[serde(default)]
    pub scope: Scope,
}

fn sobel() -> EdgeKernel {
    EdgeKernel::Sobel
}

fn edge_detect(project: &mut Project, a: EdgeArgs, cx: &mut OpCx) -> Result<OpEffect> {
    each(project, &a.target, a.scope, cx, |c| Ok(filters::edge_detect(c, a.kernel, a.amount)))
}

raster_op!(GaussianBlur, "raster.filter.gaussian-blur", "Separable gaussian blur in linear light", BlurArgs, gaussian_blur);
raster_op!(BoxBlur, "raster.filter.box-blur", "Box blur, optionally iterated", BoxBlurArgs, box_blur);
raster_op!(MotionBlur, "raster.filter.motion-blur", "Directional blur along an angle", MotionBlurArgs, motion_blur);
raster_op!(RadialBlur, "raster.filter.radial-blur", "Zoom or spin blur about a center", RadialBlurArgs, radial_blur);
raster_op!(Unsharp, "raster.filter.unsharp", "Unsharp mask with a threshold", UnsharpArgs, unsharp);
raster_op!(Sharpen, "raster.filter.sharpen", "Simple sharpen convolution", SharpenArgs, sharpen);
raster_op!(NoiseAdd, "raster.filter.noise-add", "Add seeded noise", NoiseArgs, noise_add);
raster_op!(NoiseReduce, "raster.filter.noise-reduce", "Edge-preserving median denoise", NoiseReduceArgs, noise_reduce);
raster_op!(Pixelate, "raster.filter.pixelate", "Average pixels into square blocks", PixelateArgs, pixelate);
raster_op!(Convolve, "raster.filter.convolve", "Apply an arbitrary convolution kernel", ConvolveArgs, convolve);
raster_op!(Morphology, "raster.filter.morphology", "Dilate or erode coverage", MorphologyArgs, morphology);
raster_op!(Displace, "raster.filter.displace", "Displace pixels by a map image", DisplaceArgs, displace);
raster_op!(ChannelOp, "raster.filter.channel-op", "Copy, swap, invert or combine channels", ChannelOpArgs, channel_op);
raster_op!(Dither, "raster.filter.dither", "Ordered Bayer dithering", DitherArgs, dither);
raster_op!(EdgeDetect, "raster.filter.edge-detect", "Sobel, Prewitt or Laplace edge detection", EdgeArgs, edge_detect);

pub fn ops() -> Vec<Box<dyn dpaint_core::Op>> {
    vec![
        Box::new(GaussianBlur),
        Box::new(BoxBlur),
        Box::new(MotionBlur),
        Box::new(RadialBlur),
        Box::new(Unsharp),
        Box::new(Sharpen),
        Box::new(NoiseAdd),
        Box::new(NoiseReduce),
        Box::new(Pixelate),
        Box::new(Convolve),
        Box::new(Morphology),
        Box::new(Displace),
        Box::new(ChannelOp),
        Box::new(Dither),
        Box::new(EdgeDetect),
    ]
}
