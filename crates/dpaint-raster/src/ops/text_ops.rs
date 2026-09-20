//! `raster.text.*` — text layers stay live text until someone asks for outlines.

use super::support::{self, raster_op};
use dpaint_core::color::Color;
use dpaint_core::doc::common::{Paint, Rect, Stroke, TextAlign, TextSpec};
use dpaint_core::doc::raster::{Layer, LayerKind};
use dpaint_core::{Error, OpCx, OpEffect, Project, Result};

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct AddArgs {
    /// The string to set.
    pub text: String,
    /// Font family. Unavailable families fall back and report `font-fallback`.
    #[serde(default)]
    pub family: Option<String>,
    /// Font weight, 100..=900.
    #[serde(default)]
    pub weight: Option<u16>,
    /// Use the italic face.
    #[serde(default)]
    pub italic: bool,
    /// Font size in pixels.
    #[serde(default)]
    pub size: Option<f64>,
    /// Horizontal alignment within the box.
    #[serde(default)]
    pub align: Option<TextAlign>,
    /// Line height as a multiple of the size.
    #[serde(default)]
    pub leading: Option<f64>,
    /// Extra letter spacing in pixels.
    #[serde(default)]
    pub tracking: Option<f64>,
    /// Layout box `[x, y, width, height]`. Text wraps to its width.
    #[serde(default)]
    pub r#box: Option<[f64; 4]>,
    /// Fill paint; defaults to black.
    #[serde(default)]
    pub fill: Option<Paint>,
    /// Optional outline stroke.
    #[serde(default)]
    pub stroke: Option<Stroke>,
    /// Layer name.
    #[serde(default)]
    pub name: Option<String>,
}

fn spec_from(args: &AddArgs) -> TextSpec {
    let mut spec = TextSpec::new(args.text.clone());
    if let Some(f) = &args.family {
        spec.family = f.clone();
    }
    if let Some(w) = args.weight {
        spec.weight = w;
    }
    spec.italic = args.italic;
    if let Some(s) = args.size {
        spec.size = s;
    }
    if let Some(a) = args.align {
        spec.align = a;
    }
    if let Some(l) = args.leading {
        spec.leading = l;
    }
    if let Some(t) = args.tracking {
        spec.tracking = t;
    }
    if let Some(b) = args.r#box {
        spec.r#box = Some(Rect(b));
    }
    spec
}

fn add(project: &mut Project, args: AddArgs, cx: &mut OpCx) -> Result<OpEffect> {
    let doc = support::doc_id(project, cx)?;
    let spec = spec_from(&args);
    if spec.text.is_empty() {
        return Err(Error::Invalid("text.add needs a non-empty string".into()));
    }
    let fonts = crate::text::FontSet::new(project, cx.assets);
    let layout = crate::text::layout(&spec, &fonts)?;
    let name = args.name.clone().unwrap_or_else(|| {
        spec.text.lines().next().unwrap_or("Text").chars().take(24).collect()
    });
    let rd = project.raster(&doc)?;
    let id = support::fresh_id(rd, &name);
    let layer = Layer::new(
        id.clone(),
        name,
        LayerKind::Text {
            spec: spec.clone(),
            fill: args.fill.clone().unwrap_or_else(|| Paint::solid(Color::BLACK)),
            stroke: args.stroke.clone(),
        },
    );
    if !cx.dry_run {
        project.raster_mut(&doc)?.layers.push(layer);
    }
    let mut effect = OpEffect::changed(&doc).with_created(id.to_string());
    if layout.fallback {
        effect = effect.warn(
            "font-fallback",
            id.to_string(),
            format!("'{}' is not available; used '{}'", spec.family, layout.used_family),
        );
    }
    if layout.overflow {
        effect = effect.warn(
            "text-overflow",
            id.to_string(),
            "the text is taller than its box; raster.text.fit can shrink it",
        );
    }
    Ok(effect)
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SetArgs {
    /// Text layer to change.
    pub target: String,
    /// Replacement string.
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub family: Option<String>,
    #[serde(default)]
    pub weight: Option<u16>,
    #[serde(default)]
    pub italic: Option<bool>,
    #[serde(default)]
    pub size: Option<f64>,
    #[serde(default)]
    pub align: Option<TextAlign>,
    #[serde(default)]
    pub leading: Option<f64>,
    #[serde(default)]
    pub tracking: Option<f64>,
    /// Layout box `[x, y, width, height]`.
    #[serde(default)]
    pub r#box: Option<[f64; 4]>,
    #[serde(default)]
    pub fill: Option<Paint>,
    #[serde(default)]
    pub stroke: Option<Stroke>,
}

fn set(project: &mut Project, args: SetArgs, cx: &mut OpCx) -> Result<OpEffect> {
    let (doc, id) = support::one_layer(project, cx, &args.target)?;
    let rd = project.raster(&doc)?;
    let layer = support::layer_of(rd, &id)?;
    let LayerKind::Text { spec, fill, stroke } = &layer.kind else {
        return Err(Error::Invalid(format!("'{}' is not a text layer", layer.name)));
    };
    let mut spec = spec.clone();
    if let Some(t) = args.text {
        spec.text = t;
    }
    if let Some(f) = args.family {
        spec.family = f;
    }
    if let Some(w) = args.weight {
        spec.weight = w;
    }
    if let Some(i) = args.italic {
        spec.italic = i;
    }
    if let Some(s) = args.size {
        if s <= 0.0 {
            return Err(Error::Invalid("text size must be positive".into()));
        }
        spec.size = s;
    }
    if let Some(a) = args.align {
        spec.align = a;
    }
    if let Some(l) = args.leading {
        spec.leading = l;
    }
    if let Some(t) = args.tracking {
        spec.tracking = t;
    }
    if let Some(b) = args.r#box {
        spec.r#box = Some(Rect(b));
    }
    let fill = args.fill.unwrap_or_else(|| fill.clone());
    let stroke = args.stroke.or_else(|| stroke.clone());
    let fonts = crate::text::FontSet::new(project, cx.assets);
    let layout = crate::text::layout(&spec, &fonts)?;
    let requested = spec.family.clone();
    if !cx.dry_run {
        if let Some(l) = project.raster_mut(&doc)?.layer_mut(&id) {
            l.kind = LayerKind::Text { spec, fill, stroke };
        }
    }
    let mut effect = OpEffect::changed(&doc);
    if layout.fallback {
        effect = effect.warn(
            "font-fallback",
            id.to_string(),
            format!("'{requested}' is not available; used '{}'", layout.used_family),
        );
    }
    if layout.overflow {
        effect = effect.warn(
            "text-overflow",
            id.to_string(),
            "the text is taller than its box; raster.text.fit can shrink it",
        );
    }
    Ok(effect)
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct FitArgs {
    /// Text layer to shrink.
    pub target: String,
    /// Smallest size the text may shrink to.
    #[serde(default = "six")]
    pub min_size: f64,
}

fn six() -> f64 {
    6.0
}

fn fit(project: &mut Project, args: FitArgs, cx: &mut OpCx) -> Result<OpEffect> {
    let (doc, id) = support::one_layer(project, cx, &args.target)?;
    let rd = project.raster(&doc)?;
    let layer = support::layer_of(rd, &id)?;
    let LayerKind::Text { spec, .. } = &layer.kind else {
        return Err(Error::Invalid(format!("'{}' is not a text layer", layer.name)));
    };
    if spec.r#box.is_none() {
        return Err(Error::Invalid(
            "text.fit needs a layout box; set one with raster.text.set --box".into(),
        ));
    }
    let fonts = crate::text::FontSet::new(project, cx.assets);
    let size = crate::text::fit_size(spec, &fonts, args.min_size)?;
    let hit_floor = size <= args.min_size;
    let original = spec.size;
    if !cx.dry_run {
        if let Some(LayerKind::Text { spec, .. }) =
            project.raster_mut(&doc)?.layer_mut(&id).map(|l| &mut l.kind)
        {
            spec.size = size;
        }
    }
    let mut effect = OpEffect::changed(&doc)
        .with_data(serde_json::json!({ "from": original, "to": size }));
    if hit_floor {
        effect = effect.warn(
            "text-overflow",
            id.to_string(),
            format!("the text still overflows at the minimum size of {}", args.min_size),
        );
    }
    Ok(effect)
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ToShapeArgs {
    /// Text layer to convert.
    pub target: String,
    /// Keep the text layer and add the shape beside it instead of replacing it.
    #[serde(default)]
    pub keep: bool,
}

fn to_shape(project: &mut Project, args: ToShapeArgs, cx: &mut OpCx) -> Result<OpEffect> {
    let (doc, id) = support::one_layer(project, cx, &args.target)?;
    let rd = project.raster(&doc)?;
    let layer = support::layer_of(rd, &id)?;
    let LayerKind::Text { spec, fill, stroke } = &layer.kind else {
        return Err(Error::Invalid(format!("'{}' is not a text layer", layer.name)));
    };
    let fonts = crate::text::FontSet::new(project, cx.assets);
    let (d, fallback, used) = crate::text::outline_d(spec, &fonts)?;
    let shape = LayerKind::Shape {
        d,
        fill: fill.clone(),
        stroke: stroke.clone(),
        fill_rule: Default::default(),
    };
    let requested = spec.family.clone();
    let name = format!("{} outlines", layer.name);
    let (path, index) = support::locate(rd, &id)
        .ok_or_else(|| Error::Invalid(format!("layer {id} is not in the stack")))?;
    let new_id = if args.keep { support::fresh_id(rd, &name) } else { id.clone() };
    let transform = layer.transform;
    if !cx.dry_run {
        if args.keep {
            let mut l = Layer::new(new_id.clone(), name, shape);
            l.transform = transform;
            let rd = project.raster_mut(&doc)?;
            let list = support::list_at(rd, &path)
                .ok_or_else(|| Error::Invalid("layer parent vanished".into()))?;
            list.insert(index + 1, l);
        } else if let Some(l) = project.raster_mut(&doc)?.layer_mut(&id) {
            l.kind = shape;
        }
    }
    let mut effect = OpEffect::changed(&doc);
    if args.keep {
        effect = effect.with_created(new_id.to_string());
    }
    if fallback {
        effect = effect.warn(
            "font-fallback",
            id.to_string(),
            format!("'{requested}' is not available; outlined with '{used}'"),
        );
    }
    Ok(effect)
}

raster_op!(TextAdd, "raster.text.add", "Add a live text layer", AddArgs, add);
raster_op!(TextSet, "raster.text.set", "Change a text layer's content or typography", SetArgs, set);
raster_op!(TextFit, "raster.text.fit", "Shrink a text layer's size until it fits its box", FitArgs, fit);
raster_op!(TextToShape, "raster.text.to-shape", "Convert a text layer to vector outlines", ToShapeArgs, to_shape);

pub fn ops() -> Vec<Box<dyn dpaint_core::Op>> {
    vec![Box::new(TextAdd), Box::new(TextSet), Box::new(TextFit), Box::new(TextToShape)]
}
