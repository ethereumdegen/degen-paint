//! `raster.paint.*` — brush, bucket, gradient, pattern and erase. All of them write a new
//! blob and repoint the layer, so every stroke is undoable as a JSON patch.

use super::support::{self, raster_op, Scope};
use crate::canvas::Canvas;
use crate::geom;
use crate::paint::{self, Brush, GradientKind};
use crate::select;
use dpaint_core::color::Color;
use dpaint_core::doc::common::{FillRule, GradientStop};
use dpaint_core::doc::raster::BlendMode;
use dpaint_core::{AssetRef, Error, OpCx, OpEffect, Project, Result};

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct StrokeArgs {
    /// Pixel layer to paint on.
    pub target: String,
    /// Brush path as SVG path data, in document coordinates.
    pub d: String,
    /// Brush color.
    pub color: Color,
    /// Brush diameter in pixels.
    #[serde(default = "twelve")]
    pub size: f64,
    /// Fraction of the radius that stays fully opaque, 0..=1. 1 is a hard edge.
    #[serde(default = "point_eight")]
    pub hardness: f64,
    /// Stamp spacing as a fraction of the size; smaller is smoother and slower.
    #[serde(default = "quarter")]
    pub spacing: f64,
    /// Paint deposited per stamp, 0..=1.
    #[serde(default = "one")]
    pub flow: f64,
    /// Random per-stamp offset as a fraction of the size.
    #[serde(default)]
    pub jitter: f64,
    /// Seed for the jitter, so a stroke is reproducible.
    #[serde(default)]
    pub seed: u64,
    /// Pressure curve as control points `[progress, multiplier]` with progress 0..=1, scaling
    /// tip size and flow along the stroke. `[[0,0],[0.5,1],[1,0]]` tapers both ends.
    #[serde(default)]
    pub pressure: Option<Vec<[f64; 2]>>,
    /// Overall stroke opacity, 0..=1.
    #[serde(default = "one")]
    pub opacity: f64,
    /// Blend mode used to lay the paint down.
    #[serde(default)]
    pub blend: BlendMode,
    #[serde(default)]
    pub scope: Scope,
}

fn twelve() -> f64 {
    12.0
}
fn point_eight() -> f64 {
    0.8
}
fn quarter() -> f64 {
    0.25
}
fn one() -> f64 {
    1.0
}

fn stroke(project: &mut Project, a: StrokeArgs, cx: &mut OpCx) -> Result<OpEffect> {
    if a.size <= 0.0 {
        return Err(Error::Invalid("brush size must be positive".into()));
    }
    let (doc, id) = support::one_layer(project, cx, &a.target)?;
    let path = geom::parse_d(&a.d)?;
    if path.elements().is_empty() {
        return Err(Error::DegenerateGeometry("brush path is empty".into()));
    }
    let brush = Brush {
        size: a.size,
        hardness: a.hardness,
        spacing: a.spacing,
        flow: a.flow,
        jitter: a.jitter,
        seed: a.seed,
    };
    let rd = project.raster(&doc)?;
    let (w, h) = (rd.width(), rd.height());
    let (_, offset) = support::load_pixel(rd, &id, cx.assets)?;
    let pressure = match &a.pressure {
        Some(points) => Some(crate::adjust::Curve::new(points)?),
        None => None,
    };
    let cov = paint::stroke_coverage(w, h, &path, &brush, 1.0, pressure.as_ref())?;
    support::edit_pixels(project, &doc, &id, cx, a.scope, |c| {
        let mut out = c.clone();
        let local = shift_cov(&cov, w, h, c.width, c.height, offset);
        paint::fill_cov(&mut out, &local, a.color, a.blend, a.opacity as f32);
        Ok(out)
    })?;
    Ok(OpEffect::changed(&doc))
}

/// Move a document-space coverage buffer into a layer's local pixel space.
fn shift_cov(cov: &[f32], dw: u32, dh: u32, lw: u32, lh: u32, offset: [i32; 2]) -> Vec<f32> {
    if offset == [0, 0] && dw == lw && dh == lh {
        return cov.to_vec();
    }
    let mut out = vec![0.0f32; lw as usize * lh as usize];
    for y in 0..lh as i64 {
        let sy = y + offset[1] as i64;
        if sy < 0 || sy >= dh as i64 {
            continue;
        }
        for x in 0..lw as i64 {
            let sx = x + offset[0] as i64;
            if sx < 0 || sx >= dw as i64 {
                continue;
            }
            out[y as usize * lw as usize + x as usize] =
                cov[sy as usize * dw as usize + sx as usize];
        }
    }
    out
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct BucketArgs {
    /// Pixel layer to fill.
    pub target: String,
    /// Fill color.
    pub color: Color,
    /// Seed pixel `[x, y]` in document coordinates. Omit to fill the whole selection.
    #[serde(default)]
    pub at: Option<[u32; 2]>,
    /// Color distance that counts as the same region, 0..=1.
    #[serde(default = "tenth")]
    pub tolerance: f32,
    /// Only fill the region connected to the seed.
    #[serde(default = "yes")]
    pub contiguous: bool,
    /// Grow the filled region by this many pixels, to cover anti-aliased edges.
    #[serde(default)]
    pub grow: u32,
    #[serde(default = "one")]
    pub opacity: f64,
    #[serde(default)]
    pub blend: BlendMode,
    #[serde(default)]
    pub scope: Scope,
}

fn tenth() -> f32 {
    0.1
}
fn yes() -> bool {
    true
}

fn fill_bucket(project: &mut Project, a: BucketArgs, cx: &mut OpCx) -> Result<OpEffect> {
    let (doc, id) = support::one_layer(project, cx, &a.target)?;
    let rd = project.raster(&doc)?;
    let (_, offset) = support::load_pixel(rd, &id, cx.assets)?;
    support::edit_pixels(project, &doc, &id, cx, a.scope, |c| {
        let cov = match a.at {
            Some(at) => {
                let lx = at[0] as i64 - offset[0] as i64;
                let ly = at[1] as i64 - offset[1] as i64;
                if lx < 0 || ly < 0 || lx >= c.width as i64 || ly >= c.height as i64 {
                    return Err(Error::Invalid(format!(
                        "seed {at:?} is outside this layer's pixels"
                    )));
                }
                let mut m = select::wand(c, lx as u32, ly as u32, a.tolerance, a.contiguous);
                if a.grow > 0 {
                    m.morph(a.grow, true);
                }
                m.cov
            }
            None => vec![1.0; c.pixel_count()],
        };
        let mut out = c.clone();
        paint::fill_cov(&mut out, &cov, a.color, a.blend, a.opacity as f32);
        Ok(out)
    })?;
    Ok(OpEffect::changed(&doc))
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct GradientArgs {
    /// Pixel layer to fill.
    pub target: String,
    /// Gradient geometry.
    #[serde(default)]
    pub kind: GradientKind,
    /// Start point `[x, y]` in document pixels; the center for radial and angular.
    pub from: [f64; 2],
    /// End point `[x, y]`; sets the radius for radial and the 0 angle for angular.
    pub to: [f64; 2],
    /// Color stops; `offset` runs 0..=1.
    pub stops: Vec<GradientStop>,
    #[serde(default = "one")]
    pub opacity: f64,
    #[serde(default)]
    pub blend: BlendMode,
    #[serde(default)]
    pub scope: Scope,
}

fn gradient(project: &mut Project, a: GradientArgs, cx: &mut OpCx) -> Result<OpEffect> {
    if a.stops.len() < 2 {
        return Err(Error::Invalid("a gradient needs at least two stops".into()));
    }
    let (doc, id) = support::one_layer(project, cx, &a.target)?;
    let rd = project.raster(&doc)?;
    let (w, h) = (rd.width(), rd.height());
    let (_, offset) = support::load_pixel(rd, &id, cx.assets)?;
    let grad = paint::gradient_canvas(w, h, a.kind, a.from, a.to, &a.stops, 1.0);
    support::edit_pixels(project, &doc, &id, cx, a.scope, |c| {
        let src = grad.placed(c.width, c.height, -(offset[0] as i64), -(offset[1] as i64));
        let mut out = c.clone();
        crate::blend::composite(
            &mut out,
            &src,
            a.blend,
            a.opacity as f32,
            &crate::blend::Coverage::Full,
            0,
        );
        Ok(out)
    })?;
    Ok(OpEffect::changed(&doc))
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct EraseArgs {
    /// Pixel layer to erase from.
    pub target: String,
    /// Brush path as SVG path data. Omit to erase the whole selection.
    #[serde(default)]
    pub d: Option<String>,
    #[serde(default = "twelve")]
    pub size: f64,
    #[serde(default = "point_eight")]
    pub hardness: f64,
    #[serde(default = "quarter")]
    pub spacing: f64,
    /// How much is removed per pass, 0..=1.
    #[serde(default = "one")]
    pub flow: f64,
    #[serde(default)]
    pub scope: Scope,
}

fn erase(project: &mut Project, a: EraseArgs, cx: &mut OpCx) -> Result<OpEffect> {
    let (doc, id) = support::one_layer(project, cx, &a.target)?;
    let rd = project.raster(&doc)?;
    let (w, h) = (rd.width(), rd.height());
    let (_, offset) = support::load_pixel(rd, &id, cx.assets)?;
    let cov = match &a.d {
        Some(d) => {
            let path = geom::parse_d(d)?;
            let brush = Brush {
                size: a.size,
                hardness: a.hardness,
                spacing: a.spacing,
                flow: 1.0,
                jitter: 0.0,
                seed: 0,
            };
            Some(paint::stroke_coverage(w, h, &path, &brush, 1.0, None)?)
        }
        None => {
            if rd.selection.is_none() {
                return Err(Error::Invalid(
                    "erase needs either a path or a selection to scope it".into(),
                ));
            }
            None
        }
    };
    support::edit_pixels(project, &doc, &id, cx, a.scope, |c| {
        let local = match &cov {
            Some(cov) => shift_cov(cov, w, h, c.width, c.height, offset),
            None => vec![1.0; c.pixel_count()],
        };
        let mut out = c.clone();
        paint::erase_cov(&mut out, &local, a.flow as f32);
        Ok(out)
    })?;
    Ok(OpEffect::changed(&doc))
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct PatternArgs {
    /// Pixel layer to fill.
    pub target: String,
    /// PNG in the asset store used as the tile.
    pub asset: AssetRef,
    /// Tile origin offset in pixels.
    #[serde(default)]
    pub offset: [i32; 2],
    /// Scale applied to the tile before tiling.
    #[serde(default = "one")]
    pub scale: f64,
    /// Restrict the fill to the interior of this SVG path, in document coordinates.
    #[serde(default)]
    pub d: Option<String>,
    #[serde(default = "one")]
    pub opacity: f64,
    #[serde(default)]
    pub scope: Scope,
}

fn pattern(project: &mut Project, a: PatternArgs, cx: &mut OpCx) -> Result<OpEffect> {
    if a.scale <= 0.0 {
        return Err(Error::Invalid("pattern scale must be positive".into()));
    }
    let (doc, id) = support::one_layer(project, cx, &a.target)?;
    let mut tile = Canvas::from_png(&cx.assets.get(&a.asset)?)?;
    if (a.scale - 1.0).abs() > 1e-9 {
        tile = tile.resized(
            (tile.width as f64 * a.scale).round().max(1.0) as u32,
            (tile.height as f64 * a.scale).round().max(1.0) as u32,
        );
    }
    let rd = project.raster(&doc)?;
    let (w, h) = (rd.width(), rd.height());
    let (_, offset) = support::load_pixel(rd, &id, cx.assets)?;
    let region = match &a.d {
        Some(d) => Some(paint::shape_cov(
            &geom::parse_d(d)?,
            w,
            h,
            1.0,
            FillRule::Nonzero,
        )?),
        None => None,
    };
    support::edit_pixels(project, &doc, &id, cx, a.scope, |c| {
        let cov = match &region {
            Some(r) => shift_cov(r, w, h, c.width, c.height, offset),
            None => vec![1.0; c.pixel_count()],
        };
        let mut out = c.clone();
        paint::pattern_cov(
            &mut out,
            &cov,
            &tile,
            (
                a.offset[0] as i64 - offset[0] as i64,
                a.offset[1] as i64 - offset[1] as i64,
            ),
            a.opacity as f32,
        );
        Ok(out)
    })?;
    Ok(OpEffect::changed(&doc))
}

raster_op!(
    PaintStroke,
    "raster.paint.stroke",
    "Paint a brush stroke along a path",
    StrokeArgs,
    stroke
);
raster_op!(
    FillBucket,
    "raster.paint.fill-bucket",
    "Flood fill from a seed pixel or fill the selection",
    BucketArgs,
    fill_bucket
);
raster_op!(
    PaintGradient,
    "raster.paint.gradient",
    "Paint a linear, radial, angular or diamond gradient",
    GradientArgs,
    gradient
);
raster_op!(
    PaintErase,
    "raster.paint.erase",
    "Erase along a path or within the selection",
    EraseArgs,
    erase
);
raster_op!(
    PaintPattern,
    "raster.paint.pattern",
    "Tile an image across a region",
    PatternArgs,
    pattern
);

pub fn ops() -> Vec<Box<dyn dpaint_core::Op>> {
    vec![
        Box::new(PaintStroke),
        Box::new(FillBucket),
        Box::new(PaintGradient),
        Box::new(PaintErase),
        Box::new(PaintPattern),
    ]
}
