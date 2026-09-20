//! `raster.effect.*` — live layer effects. They are stored on the layer and evaluated at
//! composite time, so changing one is a JSON edit, not a re-render of baked pixels.

use super::support::{self, raster_op};
use dpaint_core::color::Color;
use dpaint_core::doc::raster::{Effect, StrokeAlign};
use dpaint_core::{Error, LayerId, OpCx, OpEffect, Project, RasterDoc, Result};

/// Add or replace an effect of the same type on every matching layer.
fn push(
    project: &mut Project,
    target: &str,
    effect: Effect,
    replace: bool,
    cx: &mut OpCx,
) -> Result<OpEffect> {
    let (doc, ids) = support::many_layers(project, cx, target)?;
    if cx.dry_run {
        return Ok(OpEffect::changed(&doc));
    }
    let rd = project.raster_mut(&doc)?;
    for id in &ids {
        if let Some(l) = rd.layer_mut(id) {
            if replace {
                l.effects.retain(|e| discriminant(e) != discriminant(&effect));
            }
            l.effects.push(effect.clone());
        }
    }
    Ok(OpEffect::changed(&doc))
}

fn discriminant(e: &Effect) -> &'static str {
    match e {
        Effect::DropShadow { .. } => "drop-shadow",
        Effect::InnerShadow { .. } => "inner-shadow",
        Effect::Stroke { .. } => "stroke",
        Effect::OuterGlow { .. } => "outer-glow",
        Effect::Blur { .. } => "blur",
    }
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ShadowArgs {
    /// Layer(s) to decorate.
    pub target: String,
    /// Horizontal offset in pixels.
    #[serde(default = "four")]
    pub dx: f64,
    /// Vertical offset in pixels.
    #[serde(default = "four")]
    pub dy: f64,
    /// Shadow softness in pixels (diameter, like a GUI "blur" slider).
    #[serde(default = "eight")]
    pub blur: f64,
    /// Shadow color, alpha included.
    #[serde(default = "shadow_black")]
    pub color: Color,
    /// Replace an existing effect of the same type instead of stacking another one.
    #[serde(default = "yes")]
    pub replace: bool,
}

fn four() -> f64 {
    4.0
}
fn eight() -> f64 {
    8.0
}
fn yes() -> bool {
    true
}
fn shadow_black() -> Color {
    Color::rgba(0.0, 0.0, 0.0, 0.5)
}

fn drop_shadow(project: &mut Project, a: ShadowArgs, cx: &mut OpCx) -> Result<OpEffect> {
    validate_blur(a.blur)?;
    push(
        project,
        &a.target,
        Effect::DropShadow { dx: a.dx, dy: a.dy, blur: a.blur, color: a.color },
        a.replace,
        cx,
    )
}

fn inner_shadow(project: &mut Project, a: ShadowArgs, cx: &mut OpCx) -> Result<OpEffect> {
    validate_blur(a.blur)?;
    push(
        project,
        &a.target,
        Effect::InnerShadow { dx: a.dx, dy: a.dy, blur: a.blur, color: a.color },
        a.replace,
        cx,
    )
}

fn validate_blur(blur: f64) -> Result<()> {
    if blur < 0.0 || !blur.is_finite() {
        return Err(Error::Invalid(format!("blur must be zero or more, got {blur}")));
    }
    Ok(())
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct StrokeArgs {
    /// Layer(s) to outline.
    pub target: String,
    /// Stroke width in pixels.
    pub width: f64,
    /// Stroke color.
    pub color: Color,
    /// Which side of the layer's edge the stroke sits on.
    #[serde(default)]
    pub align: StrokeAlign,
    #[serde(default = "yes")]
    pub replace: bool,
}

fn stroke(project: &mut Project, a: StrokeArgs, cx: &mut OpCx) -> Result<OpEffect> {
    if a.width <= 0.0 {
        return Err(Error::Invalid("stroke width must be positive".into()));
    }
    push(
        project,
        &a.target,
        Effect::Stroke { width: a.width, color: a.color, align: a.align },
        a.replace,
        cx,
    )
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct GlowArgs {
    /// Layer(s) to decorate.
    pub target: String,
    /// Glow spread in pixels.
    #[serde(default = "eight")]
    pub blur: f64,
    /// Glow color.
    pub color: Color,
    #[serde(default = "yes")]
    pub replace: bool,
}

fn outer_glow(project: &mut Project, a: GlowArgs, cx: &mut OpCx) -> Result<OpEffect> {
    validate_blur(a.blur)?;
    push(project, &a.target, Effect::OuterGlow { blur: a.blur, color: a.color }, a.replace, cx)
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct BlurArgs {
    /// Layer(s) to blur at composite time.
    pub target: String,
    /// Blur radius in pixels.
    pub radius: f64,
    #[serde(default = "yes")]
    pub replace: bool,
}

fn blur(project: &mut Project, a: BlurArgs, cx: &mut OpCx) -> Result<OpEffect> {
    if a.radius <= 0.0 {
        return Err(Error::Invalid("blur radius must be positive".into()));
    }
    push(project, &a.target, Effect::Blur { radius: a.radius }, a.replace, cx)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum EffectKind {
    DropShadow,
    InnerShadow,
    Stroke,
    OuterGlow,
    Blur,
    /// Every effect on the layer.
    All,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct RemoveArgs {
    /// Layer(s) to clean up.
    pub target: String,
    /// Which effect to drop.
    #[serde(default = "all_kinds")]
    pub effect: EffectKind,
}

fn all_kinds() -> EffectKind {
    EffectKind::All
}

fn remove(project: &mut Project, a: RemoveArgs, cx: &mut OpCx) -> Result<OpEffect> {
    let (doc, ids) = support::many_layers(project, cx, &a.target)?;
    let rd = project.raster(&doc)?;
    if !ids.iter().any(|id| has_effects(rd, id)) {
        return Err(Error::Invalid("none of the matching layers have effects".into()));
    }
    if cx.dry_run {
        return Ok(OpEffect::changed(&doc));
    }
    let wanted = match a.effect {
        EffectKind::DropShadow => Some("drop-shadow"),
        EffectKind::InnerShadow => Some("inner-shadow"),
        EffectKind::Stroke => Some("stroke"),
        EffectKind::OuterGlow => Some("outer-glow"),
        EffectKind::Blur => Some("blur"),
        EffectKind::All => None,
    };
    let rd = project.raster_mut(&doc)?;
    for id in &ids {
        if let Some(l) = rd.layer_mut(id) {
            match wanted {
                Some(k) => l.effects.retain(|e| discriminant(e) != k),
                None => l.effects.clear(),
            }
        }
    }
    Ok(OpEffect::changed(&doc))
}

fn has_effects(rd: &RasterDoc, id: &LayerId) -> bool {
    rd.layer(id).map(|l| !l.effects.is_empty()).unwrap_or(false)
}

raster_op!(DropShadow, "raster.effect.drop-shadow", "Add a drop shadow behind a layer", ShadowArgs, drop_shadow);
raster_op!(InnerShadow, "raster.effect.inner-shadow", "Add a shadow inside a layer's edges", ShadowArgs, inner_shadow);
raster_op!(Stroke, "raster.effect.stroke", "Outline a layer's edges", StrokeArgs, stroke);
raster_op!(OuterGlow, "raster.effect.outer-glow", "Add a glow around a layer", GlowArgs, outer_glow);
raster_op!(Blur, "raster.effect.blur", "Blur a layer at composite time, non-destructively", BlurArgs, blur);
raster_op!(Remove, "raster.effect.remove", "Remove one kind of effect, or all of them", RemoveArgs, remove);

pub fn ops() -> Vec<Box<dyn dpaint_core::Op>> {
    vec![
        Box::new(DropShadow),
        Box::new(InnerShadow),
        Box::new(Stroke),
        Box::new(OuterGlow),
        Box::new(Blur),
        Box::new(Remove),
    ]
}
