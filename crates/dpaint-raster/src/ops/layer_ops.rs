//! `raster.layer.*` and `raster.doc.flatten` — the layer stack itself.

use super::support::{self, raster_op};
use crate::blend::{composite, Coverage};
use crate::canvas::Canvas;
use crate::select;
use dpaint_core::color::Color;
use dpaint_core::doc::common::{FillRule, Paint, Rect, Stroke, TextSpec, Transform};
use dpaint_core::doc::raster::{Adjustment, BlendMode, Fit, Layer, LayerKind};
use dpaint_core::kurbo::Affine;
use dpaint_core::{AssetRef, DocId, Error, LayerId, OpCx, OpEffect, Project, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum NewLayerKind {
    /// Empty pixel layer (or one adopting an existing blob via `asset`).
    Pixel,
    /// Solid color covering the document.
    Fill,
    /// Gradient covering the document.
    Gradient,
    Text,
    /// Vector shape from SVG path data, rasterized at render time.
    Shape,
    /// Non-destructive tone/color adjustment applied to everything beneath it.
    Adjustment,
    Group,
    /// Another document in this project, rendered live.
    Linked,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct AddArgs {
    /// What kind of layer to create.
    #[serde(rename = "type")]
    pub kind: NewLayerKind,
    /// Layer name; also seeds a readable layer id.
    #[serde(default)]
    pub name: Option<String>,
    /// Selector of a group layer to add into. Defaults to the root of the stack.
    #[serde(default)]
    pub parent: Option<String>,
    /// Position within its siblings, 0 being the bottom. Defaults to the top.
    #[serde(default)]
    pub index: Option<usize>,
    /// Existing blob to adopt for a `pixel` layer.
    #[serde(default)]
    pub asset: Option<AssetRef>,
    /// Pixel offset of a `pixel` layer's blob within the document.
    #[serde(default)]
    pub offset: Option<[i32; 2]>,
    /// Color for a `fill` layer.
    #[serde(default)]
    pub color: Option<Color>,
    /// Paint for a `gradient` layer.
    #[serde(default)]
    pub paint: Option<Paint>,
    /// Text content and typography for a `text` layer.
    #[serde(default)]
    pub text: Option<TextSpec>,
    /// SVG path data for a `shape` layer, in document coordinates.
    #[serde(default)]
    pub d: Option<String>,
    /// Fill paint for `text` and `shape` layers.
    #[serde(default)]
    pub fill: Option<Paint>,
    /// Stroke for `text` and `shape` layers.
    #[serde(default)]
    pub stroke: Option<Stroke>,
    /// Fill rule for a `shape` layer.
    #[serde(default)]
    pub fill_rule: Option<FillRule>,
    /// Adjustment definition for an `adjustment` layer.
    #[serde(default)]
    pub adjustment: Option<Adjustment>,
    /// Document rendered live by a `linked` layer, by id or name. The document being
    /// edited is chosen with the global `--doc`.
    #[serde(default)]
    pub source: Option<DocId>,
    /// How a `linked` render is fitted into its box.
    #[serde(default)]
    pub fit: Option<Fit>,
    /// Box `[x, y, width, height]` for a `linked` layer.
    #[serde(default)]
    pub r#box: Option<[f64; 4]>,
    /// Initial opacity, 0..=1.
    #[serde(default)]
    pub opacity: Option<f32>,
    /// Initial blend mode.
    #[serde(default)]
    pub blend: Option<BlendMode>,
    /// Clip this layer to the one beneath it.
    #[serde(default)]
    pub clip: bool,
}

fn add(project: &mut Project, args: AddArgs, cx: &mut OpCx) -> Result<OpEffect> {
    let doc = support::doc_id(project, cx)?;
    let parent = match &args.parent {
        Some(sel) => Some(support::one_layer(project, cx, sel)?.1),
        None => None,
    };
    let rd = project.raster(&doc)?;
    let (w, h) = (rd.width(), rd.height());
    let mut warnings: Vec<(String, String, String)> = Vec::new();
    let kind = match args.kind {
        NewLayerKind::Pixel => {
            let asset = match args.asset.clone() {
                Some(a) => {
                    if !cx.assets.contains(&a) {
                        return Err(Error::AssetMissing(a.0.clone()));
                    }
                    a
                }
                None => support::store_canvas(cx.assets, &Canvas::new(w, h))?,
            };
            LayerKind::Pixel {
                asset,
                offset: args.offset.unwrap_or([0, 0]),
            }
        }
        NewLayerKind::Fill => LayerKind::Fill {
            color: args
                .color
                .ok_or_else(|| Error::Invalid("a fill layer needs a color".into()))?,
        },
        NewLayerKind::Gradient => LayerKind::Gradient {
            paint: args
                .paint
                .clone()
                .ok_or_else(|| Error::Invalid("a gradient layer needs a paint".into()))?,
        },
        NewLayerKind::Text => {
            let spec = args
                .text
                .clone()
                .ok_or_else(|| Error::Invalid("a text layer needs a text spec".into()))?;
            let fonts = crate::text::FontSet::new(project, cx.assets);
            let layout = crate::text::layout(&spec, &fonts)?;
            if layout.fallback {
                warnings.push((
                    "font-fallback".into(),
                    spec.family.clone(),
                    format!(
                        "'{}' is not available; used '{}'",
                        spec.family, layout.used_family
                    ),
                ));
            }
            LayerKind::Text {
                spec,
                fill: args
                    .fill
                    .clone()
                    .unwrap_or_else(|| Paint::solid(Color::BLACK)),
                stroke: args.stroke.clone(),
            }
        }
        NewLayerKind::Shape => {
            let d = args
                .d
                .clone()
                .ok_or_else(|| Error::Invalid("a shape layer needs path data in 'd'".into()))?;
            crate::geom::parse_d(&d)?;
            LayerKind::Shape {
                d,
                fill: args
                    .fill
                    .clone()
                    .unwrap_or_else(|| Paint::solid(Color::BLACK)),
                stroke: args.stroke.clone(),
                fill_rule: args.fill_rule.unwrap_or_default(),
            }
        }
        NewLayerKind::Adjustment => {
            let adjustment = args
                .adjustment
                .clone()
                .ok_or_else(|| Error::Invalid("an adjustment layer needs an adjustment".into()))?;
            crate::adjust::prepare(&adjustment, cx.assets)?;
            LayerKind::Adjustment { adjustment }
        }
        NewLayerKind::Group => LayerKind::Group { layers: Vec::new() },
        NewLayerKind::Linked => {
            let requested = args
                .source
                .clone()
                .ok_or_else(|| Error::Invalid("a linked layer needs a document".into()))?;
            // Accept an id or a name, the same as every other document argument.
            let document = project.resolve_doc(Some(requested.as_str()))?;
            if project.would_cycle(&doc, &document) {
                return Err(Error::CyclicLink {
                    from: doc.to_string(),
                    to: document.to_string(),
                });
            }
            let b = args.r#box.unwrap_or([0.0, 0.0, w as f64, h as f64]);
            LayerKind::Linked {
                document,
                fit: args.fit.unwrap_or_default(),
                r#box: Rect(b),
            }
        }
    };
    let name = args.name.clone().unwrap_or_else(|| {
        match args.kind {
            NewLayerKind::Pixel => "Layer",
            NewLayerKind::Fill => "Fill",
            NewLayerKind::Gradient => "Gradient",
            NewLayerKind::Text => "Text",
            NewLayerKind::Shape => "Shape",
            NewLayerKind::Adjustment => "Adjustment",
            NewLayerKind::Group => "Group",
            NewLayerKind::Linked => "Linked",
        }
        .to_string()
    });
    let mut layer = Layer::new(support::fresh_id(rd, &name), name, kind);
    if let Some(o) = args.opacity {
        layer.opacity = o.clamp(0.0, 1.0);
    }
    if let Some(b) = args.blend {
        layer.blend = b;
    }
    layer.clip = args.clip;
    let id = layer.id.clone();
    if !cx.dry_run {
        let rd = project.raster_mut(&doc)?;
        support::insert_layer(rd, parent.as_ref(), args.index, layer)?;
    }
    let mut effect = OpEffect::changed(&doc).with_created(id.to_string());
    for (code, target, detail) in warnings {
        effect = effect.warn(&code, target, detail);
    }
    Ok(effect)
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct TargetArgs {
    /// Selector for the layer(s) to act on, e.g. `#lyr_sky` or `layer[type=text]`.
    pub target: String,
}

fn remove(project: &mut Project, args: TargetArgs, cx: &mut OpCx) -> Result<OpEffect> {
    let (doc, ids) = support::many_layers(project, cx, &args.target)?;
    let mut effect = OpEffect::changed(&doc);
    if !cx.dry_run {
        let rd = project.raster_mut(&doc)?;
        for id in &ids {
            rd.remove_layer(id);
        }
    }
    for id in &ids {
        effect = effect.with_removed(id.to_string());
    }
    Ok(effect)
}

fn duplicate(project: &mut Project, args: TargetArgs, cx: &mut OpCx) -> Result<OpEffect> {
    let (doc, id) = support::one_layer(project, cx, &args.target)?;
    let rd = project.raster(&doc)?;
    let source = support::layer_of(rd, &id)?.clone();
    let new_name = format!("{} copy", source.name);
    let new_id = support::fresh_id(rd, &new_name);
    let (path, index) = support::locate(rd, &id)
        .ok_or_else(|| Error::Invalid(format!("layer {id} is not in the stack")))?;
    if !cx.dry_run {
        let mut clone = retag(source, new_id.clone());
        clone.name = new_name;
        let rd = project.raster_mut(&doc)?;
        let list = support::list_at(rd, &path)
            .ok_or_else(|| Error::Invalid("layer parent vanished".into()))?;
        list.insert(index + 1, clone);
    }
    Ok(OpEffect::changed(&doc).with_created(new_id.to_string()))
}

/// Give a cloned subtree fresh ids so selectors stay unambiguous.
fn retag(mut layer: Layer, id: LayerId) -> Layer {
    layer.id = id;
    if let LayerKind::Group { layers } = &mut layer.kind {
        *layers = std::mem::take(layers)
            .into_iter()
            .map(|child| retag(child, LayerId::generate()))
            .collect();
    }
    layer
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum ReorderTo {
    /// Top of its sibling list.
    Front,
    /// Bottom of its sibling list.
    Back,
    /// One step up.
    Forward,
    /// One step down.
    Backward,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ReorderArgs {
    /// Layer to move.
    pub target: String,
    /// Relative move. Mutually exclusive with `index`.
    #[serde(default)]
    pub to: Option<ReorderTo>,
    /// Absolute position within its siblings, 0 being the bottom.
    #[serde(default)]
    pub index: Option<usize>,
}

fn reorder(project: &mut Project, args: ReorderArgs, cx: &mut OpCx) -> Result<OpEffect> {
    let (doc, id) = support::one_layer(project, cx, &args.target)?;
    let rd = project.raster(&doc)?;
    let (path, index) = support::locate(rd, &id)
        .ok_or_else(|| Error::Invalid(format!("layer {id} is not in the stack")))?;
    if args.to.is_none() && args.index.is_none() {
        return Err(Error::Invalid(
            "reorder needs either 'to' or 'index'".into(),
        ));
    }
    if cx.dry_run {
        return Ok(OpEffect::changed(&doc));
    }
    let rd = project.raster_mut(&doc)?;
    let list = support::list_at(rd, &path)
        .ok_or_else(|| Error::Invalid("layer parent vanished".into()))?;
    let last = list.len().saturating_sub(1);
    let dest = match (args.to, args.index) {
        (Some(ReorderTo::Front), _) => last,
        (Some(ReorderTo::Back), _) => 0,
        (Some(ReorderTo::Forward), _) => (index + 1).min(last),
        (Some(ReorderTo::Backward), _) => index.saturating_sub(1),
        (None, Some(i)) => i.min(last),
        (None, None) => index,
    };
    let layer = list.remove(index);
    list.insert(dest, layer);
    Ok(OpEffect::changed(&doc))
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct RenameArgs {
    /// Layer to rename.
    pub target: String,
    /// New name. Ids never change, so selectors by id keep working.
    pub name: String,
}

fn rename(project: &mut Project, args: RenameArgs, cx: &mut OpCx) -> Result<OpEffect> {
    let (doc, id) = support::one_layer(project, cx, &args.target)?;
    if !cx.dry_run {
        if let Some(l) = project.raster_mut(&doc)?.layer_mut(&id) {
            l.name = args.name;
        }
    }
    Ok(OpEffect::changed(&doc))
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SetArgs {
    /// Layer(s) to change.
    pub target: String,
    /// Opacity 0..=1.
    #[serde(default)]
    pub opacity: Option<f32>,
    /// Blend mode.
    #[serde(default)]
    pub blend: Option<BlendMode>,
    /// Visibility.
    #[serde(default)]
    pub visible: Option<bool>,
    /// Lock, which only guards interactive editing; ops still address it.
    #[serde(default)]
    pub locked: Option<bool>,
}

fn set(project: &mut Project, args: SetArgs, cx: &mut OpCx) -> Result<OpEffect> {
    let (doc, ids) = support::many_layers(project, cx, &args.target)?;
    if let Some(o) = args.opacity {
        if !(0.0..=1.0).contains(&o) {
            return Err(Error::Invalid(format!("opacity must be in 0..=1, got {o}")));
        }
    }
    if cx.dry_run {
        return Ok(OpEffect::changed(&doc));
    }
    let rd = project.raster_mut(&doc)?;
    for id in &ids {
        if let Some(l) = rd.layer_mut(id) {
            if let Some(o) = args.opacity {
                l.opacity = o;
            }
            if let Some(b) = args.blend {
                l.blend = b;
            }
            if let Some(v) = args.visible {
                l.visible = v;
            }
            if let Some(v) = args.locked {
                l.locked = v;
            }
        }
    }
    Ok(OpEffect::changed(&doc))
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct TransformArgs {
    /// Layer(s) to transform.
    pub target: String,
    /// Translate by `[dx, dy]` document pixels.
    #[serde(default)]
    pub translate: Option<[f64; 2]>,
    /// Scale by `[sx, sy]` about `origin`.
    #[serde(default)]
    pub scale: Option<[f64; 2]>,
    /// Rotate clockwise by this many degrees about `origin`.
    #[serde(default)]
    pub rotate: Option<f64>,
    /// Skew by `[ax, ay]` degrees.
    #[serde(default)]
    pub skew: Option<[f64; 2]>,
    /// Replace the transform outright with `[a, b, c, d, e, f]`.
    #[serde(default)]
    pub matrix: Option<[f64; 6]>,
    /// Mirror: `horizontal`, `vertical` or `both`, about `origin`.
    #[serde(default)]
    pub flip: Option<super::canvas_ops::FlipAxis>,
    /// Pivot for scale, rotate and flip. Defaults to the document center.
    #[serde(default)]
    pub origin: Option<[f64; 2]>,
    /// Bake the result into the layer's pixels instead of keeping it as a live transform.
    #[serde(default)]
    pub bake: bool,
}

fn transform(project: &mut Project, args: TransformArgs, cx: &mut OpCx) -> Result<OpEffect> {
    let (doc, ids) = support::many_layers(project, cx, &args.target)?;
    let rd = project.raster(&doc)?;
    let origin = args
        .origin
        .unwrap_or([rd.width() as f64 / 2.0, rd.height() as f64 / 2.0]);
    let about = |m: Affine| {
        Affine::translate((origin[0], origin[1])) * m * Affine::translate((-origin[0], -origin[1]))
    };
    let mut delta = Affine::IDENTITY;
    let mut any = false;
    if let Some(t) = args.translate {
        delta = Affine::translate((t[0], t[1])) * delta;
        any = true;
    }
    if let Some(s) = args.scale {
        if s[0] == 0.0 || s[1] == 0.0 {
            return Err(Error::DegenerateGeometry(
                "scale by zero collapses the layer".into(),
            ));
        }
        delta = about(Affine::scale_non_uniform(s[0], s[1])) * delta;
        any = true;
    }
    if let Some(r) = args.rotate {
        delta = about(Affine::rotate(r.to_radians())) * delta;
        any = true;
    }
    if let Some(k) = args.skew {
        let (kx, ky) = (k[0].to_radians().tan(), k[1].to_radians().tan());
        delta = about(Affine::new([1.0, ky, kx, 1.0, 0.0, 0.0])) * delta;
        any = true;
    }
    if let Some(f) = args.flip {
        use super::canvas_ops::FlipAxis;
        let (sx, sy) = match f {
            FlipAxis::Horizontal => (-1.0, 1.0),
            FlipAxis::Vertical => (1.0, -1.0),
            FlipAxis::Both => (-1.0, -1.0),
        };
        delta = about(Affine::scale_non_uniform(sx, sy)) * delta;
        any = true;
    }
    if args.matrix.is_none() && !any {
        return Err(Error::Invalid(
            "transform needs at least one of translate, scale, rotate, skew, flip or matrix".into(),
        ));
    }

    // Baking needs the rendered result, so compute it before mutating anything.
    let mut baked: Vec<(LayerId, AssetRef)> = Vec::new();
    if args.bake {
        for id in &ids {
            let mut probe = project.clone();
            {
                let rd = probe.raster_mut(&doc)?;
                if let Some(l) = rd.layer_mut(id) {
                    l.transform = match args.matrix {
                        Some(m) => Transform(m),
                        None => l.transform.then(Transform::from_kurbo(delta)),
                    };
                }
            }
            let c = support::layer_content(&probe, &doc, id, cx.assets)?;
            baked.push((id.clone(), support::store_canvas(cx.assets, &c)?));
        }
    }
    if cx.dry_run {
        return Ok(OpEffect::changed(&doc));
    }
    let rd = project.raster_mut(&doc)?;
    for id in &ids {
        if args.bake {
            continue;
        }
        if let Some(l) = rd.layer_mut(id) {
            l.transform = match args.matrix {
                Some(m) => Transform(m),
                None => l.transform.then(Transform::from_kurbo(delta)),
            };
        }
    }
    for (id, asset) in baked {
        if let Some(l) = rd.layer_mut(&id) {
            l.transform = Transform::IDENTITY;
            l.mask = None;
            l.effects.clear();
            l.kind = LayerKind::Pixel {
                asset,
                offset: [0, 0],
            };
        }
    }
    Ok(OpEffect::changed(&doc))
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct GroupArgs {
    /// Layers to gather. They must be siblings.
    pub target: String,
    /// Name of the new group.
    #[serde(default)]
    pub name: Option<String>,
}

fn group(project: &mut Project, args: GroupArgs, cx: &mut OpCx) -> Result<OpEffect> {
    let (doc, ids) = support::many_layers(project, cx, &args.target)?;
    let rd = project.raster(&doc)?;
    let mut located: Vec<(Vec<usize>, usize, LayerId)> = Vec::new();
    for id in &ids {
        let (path, index) = support::locate(rd, id)
            .ok_or_else(|| Error::Invalid(format!("layer {id} is not in the stack")))?;
        located.push((path, index, id.clone()));
    }
    let first_path = located[0].0.clone();
    if located.iter().any(|(p, _, _)| p != &first_path) {
        return Err(Error::Invalid(
            "layers must be siblings to be grouped; group them per level".into(),
        ));
    }
    located.sort_by_key(|(_, i, _)| *i);
    let insert_at = located[0].1;
    let name = args.name.unwrap_or_else(|| "Group".to_string());
    let gid = support::fresh_id(rd, &name);
    if cx.dry_run {
        return Ok(OpEffect::changed(&doc).with_created(gid.to_string()));
    }
    let rd = project.raster_mut(&doc)?;
    let list = support::list_at(rd, &first_path)
        .ok_or_else(|| Error::Invalid("parent vanished".into()))?;
    let mut taken = Vec::new();
    for (_, index, _) in located.iter().rev() {
        taken.push(list.remove(*index));
    }
    taken.reverse();
    let group = Layer::new(gid.clone(), name, LayerKind::Group { layers: taken });
    list.insert(insert_at.min(list.len()), group);
    Ok(OpEffect::changed(&doc).with_created(gid.to_string()))
}

fn ungroup(project: &mut Project, args: TargetArgs, cx: &mut OpCx) -> Result<OpEffect> {
    let (doc, id) = support::one_layer(project, cx, &args.target)?;
    let rd = project.raster(&doc)?;
    let layer = support::layer_of(rd, &id)?;
    let LayerKind::Group { .. } = &layer.kind else {
        return Err(Error::Invalid(format!(
            "'{}' is not a group layer",
            layer.name
        )));
    };
    let has_decoration = layer.mask.is_some() || !layer.effects.is_empty();
    let (path, index) = support::locate(rd, &id)
        .ok_or_else(|| Error::Invalid(format!("layer {id} is not in the stack")))?;
    if cx.dry_run {
        return Ok(OpEffect::changed(&doc).with_removed(id.to_string()));
    }
    let rd = project.raster_mut(&doc)?;
    let list =
        support::list_at(rd, &path).ok_or_else(|| Error::Invalid("parent vanished".into()))?;
    let group = list.remove(index);
    let (gt, go) = (group.transform, group.opacity);
    let LayerKind::Group { layers } = group.kind else {
        unreachable!("checked above")
    };
    for (offset, mut child) in layers.into_iter().enumerate() {
        // The group's transform and opacity have to survive on each child.
        child.transform = child.transform.then(gt);
        child.opacity *= go;
        list.insert(index + offset, child);
    }
    let mut effect = OpEffect::changed(&doc).with_removed(id.to_string());
    if has_decoration {
        effect = effect.warn(
            "effects-dropped",
            id.to_string(),
            "the group's mask and effects cannot apply to loose layers and were dropped",
        );
    }
    Ok(effect)
}

fn merge_down(project: &mut Project, args: TargetArgs, cx: &mut OpCx) -> Result<OpEffect> {
    let (doc, id) = support::one_layer(project, cx, &args.target)?;
    let rd = project.raster(&doc)?;
    let (path, index) = support::locate(rd, &id)
        .ok_or_else(|| Error::Invalid(format!("layer {id} is not in the stack")))?;
    if index == 0 {
        return Err(Error::Invalid(
            "the bottom layer has nothing beneath it to merge into".into(),
        ));
    }
    let below_id = {
        let mut probe = project.clone();
        let rd = probe.raster_mut(&doc)?;
        let list =
            support::list_at(rd, &path).ok_or_else(|| Error::Invalid("parent vanished".into()))?;
        list[index - 1].id.clone()
    };
    let top = support::layer_of(rd, &id)?.clone();
    let bottom = support::layer_of(rd, &below_id)?.clone();
    let mut acc = support::layer_canvas(project, &doc, &below_id, cx.assets)?;
    let top_content = support::layer_content(project, &doc, &id, cx.assets)?;
    composite(
        &mut acc,
        &top_content,
        top.blend,
        top.opacity,
        &Coverage::Full,
        0,
    );
    let asset = support::store_canvas(cx.assets, &acc)?;
    let name = bottom.name.clone();
    if cx.dry_run {
        return Ok(OpEffect::changed(&doc).with_removed(id.to_string()));
    }
    let rd = project.raster_mut(&doc)?;
    let list =
        support::list_at(rd, &path).ok_or_else(|| Error::Invalid("parent vanished".into()))?;
    list.remove(index);
    let merged = &mut list[index - 1];
    merged.name = name;
    merged.kind = LayerKind::Pixel {
        asset,
        offset: [0, 0],
    };
    merged.transform = Transform::IDENTITY;
    merged.opacity = 1.0;
    merged.blend = BlendMode::Normal;
    merged.mask = None;
    merged.effects.clear();
    Ok(OpEffect::changed(&doc).with_removed(id.to_string()))
}

fn rasterize(project: &mut Project, args: TargetArgs, cx: &mut OpCx) -> Result<OpEffect> {
    let (doc, ids) = support::many_layers(project, cx, &args.target)?;
    let mut baked = Vec::new();
    for id in &ids {
        let c = support::layer_content(project, &doc, id, cx.assets)?;
        baked.push((id.clone(), support::store_canvas(cx.assets, &c)?));
    }
    if cx.dry_run {
        return Ok(OpEffect::changed(&doc));
    }
    let rd = project.raster_mut(&doc)?;
    for (id, asset) in baked {
        if let Some(l) = rd.layer_mut(&id) {
            l.kind = LayerKind::Pixel {
                asset,
                offset: [0, 0],
            };
            l.transform = Transform::IDENTITY;
            l.mask = None;
            l.effects.clear();
        }
    }
    Ok(OpEffect::changed(&doc))
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct FromSelectionArgs {
    /// Layer to take pixels from. Defaults to the flattened composite.
    #[serde(default)]
    pub source: Option<String>,
    /// Name of the new layer.
    #[serde(default)]
    pub name: Option<String>,
    /// Remove the selected pixels from the source layer (cut instead of copy).
    #[serde(default)]
    pub cut: bool,
}

fn from_selection(
    project: &mut Project,
    args: FromSelectionArgs,
    cx: &mut OpCx,
) -> Result<OpEffect> {
    let doc = support::doc_id(project, cx)?;
    let source = match &args.source {
        Some(sel) => Some(support::one_layer(project, cx, sel)?.1),
        None => None,
    };
    let rd = project.raster(&doc)?;
    let sel = select::resolve(rd, cx.assets, rd.width(), rd.height(), 1.0)?
        .ok_or_else(|| Error::Invalid("layer.from-selection needs a selection".into()))?;
    if sel.is_empty() {
        return Err(Error::Invalid("the current selection is empty".into()));
    }
    let src = match &source {
        Some(id) => support::layer_canvas(project, &doc, id, cx.assets)?,
        None => support::flatten_canvas(project, &doc, cx.assets)?,
    };
    let empty = Canvas::new(src.width, src.height);
    let lifted = select::composite_through(&empty, &src, Some(&sel), (0, 0));
    let asset = support::store_canvas(cx.assets, &lifted)?;
    let name = args.name.unwrap_or_else(|| "Selection".to_string());
    let id = support::fresh_id(rd, &name);

    // Cutting the source is a pixel edit, so it goes through the same blob-replacing path.
    let cut_asset = if args.cut {
        let Some(sid) = &source else {
            return Err(Error::Invalid("cut needs an explicit source layer".into()));
        };
        let (old, offset) = support::load_pixel(rd, sid, cx.assets)?;
        let mut out = old.clone();
        for y in 0..out.height {
            for x in 0..out.width {
                let (sx, sy) = (x as i64 + offset[0] as i64, y as i64 + offset[1] as i64);
                if sx < 0 || sy < 0 || sx >= sel.width as i64 || sy >= sel.height as i64 {
                    continue;
                }
                let m = sel.at(sx as u32, sy as u32);
                if m <= 0.0 {
                    continue;
                }
                let i = out.idx(x, y);
                for c in 0..4 {
                    out.data[i + c] *= 1.0 - m;
                }
            }
        }
        Some((sid.clone(), support::store_canvas(cx.assets, &out)?))
    } else {
        None
    };

    if cx.dry_run {
        return Ok(OpEffect::changed(&doc).with_created(id.to_string()));
    }
    let rd = project.raster_mut(&doc)?;
    if let Some((sid, asset)) = cut_asset {
        support::set_pixels(rd, &sid, asset, [0, 0])?;
    }
    let layer = Layer::new(
        id.clone(),
        name,
        LayerKind::Pixel {
            asset,
            offset: [0, 0],
        },
    );
    rd.layers.push(layer);
    Ok(OpEffect::changed(&doc).with_created(id.to_string()))
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct FlattenArgs {
    /// Name of the resulting single layer.
    #[serde(default)]
    pub name: Option<String>,
}

fn flatten(project: &mut Project, args: FlattenArgs, cx: &mut OpCx) -> Result<OpEffect> {
    let doc = support::doc_id(project, cx)?;
    let flat = support::flatten_canvas(project, &doc, cx.assets)?;
    let asset = support::store_canvas(cx.assets, &flat)?;
    let name = args.name.unwrap_or_else(|| "Flattened".to_string());
    let rd = project.raster(&doc)?;
    let removed: Vec<String> = rd.walk().iter().map(|l| l.id.to_string()).collect();
    let id = support::fresh_id(rd, &name);
    if cx.dry_run {
        return Ok(OpEffect::changed(&doc).with_created(id.to_string()));
    }
    let rd = project.raster_mut(&doc)?;
    rd.layers = vec![Layer::new(
        id.clone(),
        name,
        LayerKind::Pixel {
            asset,
            offset: [0, 0],
        },
    )];
    // The background is now baked into the pixels; keeping it would double it.
    rd.background = None;
    let mut effect = OpEffect::changed(&doc).with_created(id.to_string());
    for r in removed {
        effect = effect.with_removed(r);
    }
    Ok(effect)
}

raster_op!(
    Add,
    "raster.layer.add",
    "Add a layer of any kind to the stack",
    AddArgs,
    add
);
raster_op!(
    Remove,
    "raster.layer.remove",
    "Remove the matching layers",
    TargetArgs,
    remove
);
raster_op!(
    Duplicate,
    "raster.layer.duplicate",
    "Duplicate a layer above itself",
    TargetArgs,
    duplicate
);
raster_op!(
    Reorder,
    "raster.layer.reorder",
    "Move a layer within its siblings",
    ReorderArgs,
    reorder
);
raster_op!(
    Rename,
    "raster.layer.rename",
    "Rename a layer",
    RenameArgs,
    rename
);
raster_op!(
    Set,
    "raster.layer.set",
    "Set opacity, blend mode, visibility or lock",
    SetArgs,
    set
);
raster_op!(
    TransformOp,
    "raster.layer.transform",
    "Translate, scale, rotate, skew, flip or replace a layer transform",
    TransformArgs,
    transform
);
raster_op!(
    Group,
    "raster.layer.group",
    "Gather sibling layers into a new group",
    GroupArgs,
    group
);
raster_op!(
    Ungroup,
    "raster.layer.ungroup",
    "Dissolve a group, keeping its children",
    TargetArgs,
    ungroup
);
raster_op!(
    MergeDown,
    "raster.layer.merge-down",
    "Merge a layer into the layer beneath it",
    TargetArgs,
    merge_down
);
raster_op!(
    Rasterize,
    "raster.layer.rasterize",
    "Bake a layer (and its mask, transform and effects) to pixels",
    TargetArgs,
    rasterize
);
raster_op!(
    FromSelection,
    "raster.layer.from-selection",
    "Lift the current selection into a new pixel layer",
    FromSelectionArgs,
    from_selection
);
raster_op!(
    Flatten,
    "raster.doc.flatten",
    "Flatten the whole document into one pixel layer",
    FlattenArgs,
    flatten
);

pub fn ops() -> Vec<Box<dyn dpaint_core::Op>> {
    vec![
        Box::new(Add),
        Box::new(Remove),
        Box::new(Duplicate),
        Box::new(Reorder),
        Box::new(Rename),
        Box::new(Set),
        Box::new(TransformOp),
        Box::new(Group),
        Box::new(Ungroup),
        Box::new(MergeDown),
        Box::new(Rasterize),
        Box::new(FromSelection),
        Box::new(Flatten),
    ]
}
