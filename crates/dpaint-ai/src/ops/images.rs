//! fal-backed raster ops. Every one of them produces an ordinary pixel layer or an ordinary
//! layer mask: nothing in the document remembers that a model was involved except its
//! provenance record.

use super::*;
use crate::image as img;
use crate::keys::Provider;
use crate::Runtime;
use dpaint_core::doc::raster::Mask;
use dpaint_core::doc::{DocKind, Transform};
use dpaint_core::{parse_args, resolve_one, schema_for, Project};
use schemars::JsonSchema;
use serde::Deserialize;

fn model_for(rt: &Runtime, op: &str, override_id: &Option<String>) -> String {
    override_id
        .clone()
        .unwrap_or_else(|| rt.config.fal.model_for(op).to_string())
}

/// Resolve a selector to a pixel layer's asset, before anything is mutated or sent.
fn source_layer(
    project: &Project,
    op: &str,
    selector: &str,
    doc: &dpaint_core::DocId,
) -> Result<(LayerId, AssetRef)> {
    let m = resolve_one(project, selector, Some(doc))?;
    let id = LayerId::from(m.id.as_str());
    let raster = project.raster(&m.document)?;
    let layer = raster
        .layer(&id)
        .ok_or_else(|| Error::Invalid(format!("'{}' is not a layer of {}", m.id, m.document)))?;
    Ok((id, pixel_asset(op, layer)?))
}

fn first_asset(gen: &Generated, op: &str) -> Result<AssetRef> {
    gen.assets
        .first()
        .cloned()
        .ok_or_else(|| Error::ProviderError {
            provider: "fal".into(),
            detail: format!("{op} produced no image"),
        })
}

// ---------------------------------------------------------------------------------------
// ai.image.generate
// ---------------------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct GenerateArgs {
    /// What to generate.
    pub prompt: String,
    /// Output size as WIDTHxHEIGHT. Defaults to the document's canvas size.
    #[serde(default)]
    pub size: Option<String>,
    /// Seed, for a reproducible result.
    #[serde(default)]
    pub seed: Option<i64>,
    /// Name of the new layer.
    #[serde(default)]
    pub name: Option<String>,
    /// Override the configured fal model id.
    #[serde(default)]
    pub model: Option<String>,
    /// Extra provider parameters, merged into the request body.
    #[serde(default)]
    pub extra: Option<serde_json::Value>,
}

pub(crate) struct Generate {
    pub rt: Runtime,
}

impl Op for Generate {
    fn id(&self) -> &'static str {
        "ai.image.generate"
    }
    fn about(&self) -> &'static str {
        "Generate a new pixel layer from a text prompt (fal)."
    }
    fn schema(&self) -> Value {
        schema_for::<GenerateArgs>()
    }
    fn modes(&self) -> &'static [DocKind] {
        &[DocKind::Raster]
    }
    fn is_network(&self) -> bool {
        true
    }

    fn apply(&self, project: &mut Project, args: Value, cx: &mut OpCx) -> Result<OpEffect> {
        let a: GenerateArgs = parse_args(self.id(), args)?;
        let doc_id = cx.target_doc(project)?;
        let doc = project.raster(&doc_id)?;
        let (w, h) = match &a.size {
            Some(s) => parse_size(self.id(), s)?,
            None => (doc.width(), doc.height()),
        };
        let model = model_for(&self.rt, self.id(), &a.model);

        let mut body = json!({
            "prompt": a.prompt,
            "image_size": { "width": w, "height": h },
            "num_images": 1,
        });
        if let Some(s) = a.seed {
            body["seed"] = json!(s);
        }
        merge_extra(&mut body, a.extra.clone());

        if cx.dry_run {
            return dry_run_effect(&self.rt, cx, self.id(), Provider::Fal, &model, &body);
        }

        let gen = fal_call(&self.rt, cx, self.id(), &model, &body, &body, &[])?;
        let asset = first_asset(&gen, self.id())?;
        let prov = provenance(Provider::Fal, &model, Some(a.prompt.clone()), a.seed, &gen);

        let name = a.name.clone().unwrap_or_else(|| "generated".to_string());
        let doc = project.raster_mut(&doc_id)?;
        let layer_id = unique_layer_id(doc, &name);
        let mut layer = Layer::new(
            layer_id.clone(),
            name,
            LayerKind::Pixel {
                asset: asset.clone(),
                offset: [0, 0],
            },
        );
        layer.provenance = Some(prov);
        doc.layers.push(layer);

        let mut effect = OpEffect::changed(&doc_id).with_created(layer_id.to_string());
        effect.cost_usd = Some(gen.cost_usd);
        effect = effect.with_data(json!({
            "asset": asset,
            "cached": gen.cached,
            "requestId": gen.request_id,
            "model": model,
        }));
        if let Ok(actual) = img::decode(&cx.assets.get(&asset)?) {
            if (actual.width(), actual.height()) != (w, h) {
                effect = effect.warn(
                    "size-mismatch",
                    format!("#{layer_id}"),
                    format!(
                        "requested {w}x{h}, the model returned {}x{}",
                        actual.width(),
                        actual.height()
                    ),
                );
            }
        }
        Ok(effect)
    }
}

// ---------------------------------------------------------------------------------------
// ai.image.edit
// ---------------------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct EditArgs {
    /// Selector for the pixel layer to edit, e.g. `#lyr_sky`.
    pub layer: String,
    /// The instruction, e.g. "make it golden hour".
    pub prompt: String,
    /// Seed, for a reproducible result.
    #[serde(default)]
    pub seed: Option<i64>,
    /// Replace the source layer's pixels instead of adding the result above it.
    #[serde(default)]
    pub replace: bool,
    /// Name of the new layer, when not replacing.
    #[serde(default)]
    pub name: Option<String>,
    /// Override the configured fal model id.
    #[serde(default)]
    pub model: Option<String>,
    /// Extra provider parameters, merged into the request body.
    #[serde(default)]
    pub extra: Option<serde_json::Value>,
}

pub(crate) struct Edit {
    pub rt: Runtime,
}

impl Op for Edit {
    fn id(&self) -> &'static str {
        "ai.image.edit"
    }
    fn about(&self) -> &'static str {
        "Rewrite a pixel layer from an instruction, as a new layer above it (fal)."
    }
    fn schema(&self) -> Value {
        schema_for::<EditArgs>()
    }
    fn modes(&self) -> &'static [DocKind] {
        &[DocKind::Raster]
    }
    fn is_network(&self) -> bool {
        true
    }

    fn apply(&self, project: &mut Project, args: Value, cx: &mut OpCx) -> Result<OpEffect> {
        let a: EditArgs = parse_args(self.id(), args)?;
        let doc_id = cx.target_doc(project)?;
        project.raster(&doc_id)?;
        let (src_id, src_asset) = source_layer(project, self.id(), &a.layer, &doc_id)?;
        let src_bytes = cx.assets.get(&src_asset)?;
        let model = model_for(&self.rt, self.id(), &a.model);

        let mut body = json!({
            "prompt": a.prompt,
            "image_url": img::data_uri(&src_bytes, img::mime_for_ext(src_asset.ext())),
        });
        if let Some(s) = a.seed {
            body["seed"] = json!(s);
        }
        merge_extra(&mut body, a.extra.clone());
        let key_params = json!({ "prompt": a.prompt, "seed": a.seed, "extra": a.extra });

        if cx.dry_run {
            return dry_run_effect(&self.rt, cx, self.id(), Provider::Fal, &model, &key_params);
        }

        let gen = fal_call(
            &self.rt,
            cx,
            self.id(),
            &model,
            &body,
            &key_params,
            std::slice::from_ref(&src_bytes),
        )?;
        let asset = first_asset(&gen, self.id())?;
        let prov = provenance(Provider::Fal, &model, Some(a.prompt.clone()), a.seed, &gen);

        let doc = project.raster_mut(&doc_id)?;
        let mut effect = OpEffect::changed(&doc_id);
        if a.replace {
            let layer = doc
                .layer_mut(&src_id)
                .ok_or_else(|| Error::Invalid(format!("layer '{src_id}' vanished")))?;
            let offset = match layer.kind {
                LayerKind::Pixel { offset, .. } => offset,
                _ => [0, 0],
            };
            layer.kind = LayerKind::Pixel {
                asset: asset.clone(),
                offset,
            };
            layer.provenance = Some(prov);
        } else {
            let name = a.name.clone().unwrap_or_else(|| {
                format!(
                    "{} edit",
                    doc.layer(&src_id)
                        .map(|l| l.name.clone())
                        .unwrap_or_default()
                )
            });
            let layer_id = unique_layer_id(doc, name.trim());
            let mut layer = Layer::new(
                layer_id.clone(),
                name.trim().to_string(),
                LayerKind::Pixel {
                    asset: asset.clone(),
                    offset: [0, 0],
                },
            );
            layer.provenance = Some(prov);
            if !insert_above(&mut doc.layers, &src_id, layer) {
                return Err(Error::Invalid(format!("layer '{src_id}' vanished")));
            }
            effect = effect.with_created(layer_id.to_string());
        }
        effect.cost_usd = Some(gen.cost_usd);
        Ok(effect.with_data(json!({
            "asset": asset,
            "cached": gen.cached,
            "requestId": gen.request_id,
            "model": model,
        })))
    }
}

// ---------------------------------------------------------------------------------------
// ai.image.inpaint
// ---------------------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct InpaintArgs {
    /// Selector for the pixel layer to paint into, e.g. `#lyr_sky`.
    pub layer: String,
    /// What to paint inside the selection.
    pub prompt: String,
    /// Seed, for a reproducible result.
    #[serde(default)]
    pub seed: Option<i64>,
    /// Name of the new layer.
    #[serde(default)]
    pub name: Option<String>,
    /// Override the configured fal model id.
    #[serde(default)]
    pub model: Option<String>,
    /// Extra provider parameters, merged into the request body.
    #[serde(default)]
    pub extra: Option<serde_json::Value>,
}

pub(crate) struct Inpaint {
    pub rt: Runtime,
}

impl Op for Inpaint {
    fn id(&self) -> &'static str {
        "ai.image.inpaint"
    }
    fn about(&self) -> &'static str {
        "Repaint the document's current selection; the selection is the mask (fal)."
    }
    fn schema(&self) -> Value {
        schema_for::<InpaintArgs>()
    }
    fn modes(&self) -> &'static [DocKind] {
        &[DocKind::Raster]
    }
    fn is_network(&self) -> bool {
        true
    }

    fn apply(&self, project: &mut Project, args: Value, cx: &mut OpCx) -> Result<OpEffect> {
        let a: InpaintArgs = parse_args(self.id(), args)?;
        let doc_id = cx.target_doc(project)?;
        let doc = project.raster(&doc_id)?;
        let (src_id, src_asset) = source_layer(project, self.id(), &a.layer, &doc_id)?;
        let src_bytes = cx.assets.get(&src_asset)?;

        // The editor's own selection tools drive the model. That is the whole point of
        // having them, so an empty or missing selection is an error, not a silent full-canvas
        // repaint.
        let mask_png = img::selection_mask_png(doc, cx.assets)?;
        let coverage = img::selection_coverage(&mask_png)?;
        if coverage <= 0.0 {
            return Err(Error::Invalid(format!(
                "the selection in '{doc_id}' covers no pixels; nothing to inpaint"
            )));
        }
        let model = model_for(&self.rt, self.id(), &a.model);

        let mut body = json!({
            "prompt": a.prompt,
            "image_url": img::data_uri(&src_bytes, img::mime_for_ext(src_asset.ext())),
            "mask_url": img::data_uri(&mask_png, "image/png"),
        });
        if let Some(s) = a.seed {
            body["seed"] = json!(s);
        }
        merge_extra(&mut body, a.extra.clone());
        let key_params = json!({ "prompt": a.prompt, "seed": a.seed, "extra": a.extra });

        if cx.dry_run {
            return dry_run_effect(&self.rt, cx, self.id(), Provider::Fal, &model, &key_params);
        }

        let gen = fal_call(
            &self.rt,
            cx,
            self.id(),
            &model,
            &body,
            &key_params,
            &[src_bytes.clone(), mask_png.clone()],
        )?;
        let asset = first_asset(&gen, self.id())?;
        let prov = provenance(Provider::Fal, &model, Some(a.prompt.clone()), a.seed, &gen);

        let doc = project.raster_mut(&doc_id)?;
        let name = a.name.clone().unwrap_or_else(|| {
            format!(
                "{} inpaint",
                doc.layer(&src_id)
                    .map(|l| l.name.clone())
                    .unwrap_or_default()
            )
        });
        let layer_id = unique_layer_id(doc, name.trim());
        let mut layer = Layer::new(
            layer_id.clone(),
            name.trim().to_string(),
            LayerKind::Pixel {
                asset: asset.clone(),
                offset: [0, 0],
            },
        );
        layer.provenance = Some(prov);
        if !insert_above(&mut doc.layers, &src_id, layer) {
            return Err(Error::Invalid(format!("layer '{src_id}' vanished")));
        }

        let mut effect = OpEffect::changed(&doc_id).with_created(layer_id.to_string());
        effect.cost_usd = Some(gen.cost_usd);
        Ok(effect.with_data(json!({
            "asset": asset,
            "cached": gen.cached,
            "requestId": gen.request_id,
            "model": model,
            "maskCoverage": coverage,
        })))
    }
}

// ---------------------------------------------------------------------------------------
// ai.image.outpaint
// ---------------------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct OutpaintArgs {
    /// Selector for the pixel layer to extend, e.g. `#lyr_sky`.
    pub layer: String,
    /// What belongs in the new area.
    pub prompt: String,
    /// Pixels to add on the left.
    #[serde(default)]
    pub left: u32,
    /// Pixels to add on the top.
    #[serde(default)]
    pub top: u32,
    /// Pixels to add on the right.
    #[serde(default)]
    pub right: u32,
    /// Pixels to add on the bottom.
    #[serde(default)]
    pub bottom: u32,
    /// Seed, for a reproducible result.
    #[serde(default)]
    pub seed: Option<i64>,
    /// Name of the new layer.
    #[serde(default)]
    pub name: Option<String>,
    /// Override the configured fal model id.
    #[serde(default)]
    pub model: Option<String>,
    /// Extra provider parameters, merged into the request body.
    #[serde(default)]
    pub extra: Option<serde_json::Value>,
}

pub(crate) struct Outpaint {
    pub rt: Runtime,
}

impl Op for Outpaint {
    fn id(&self) -> &'static str {
        "ai.image.outpaint"
    }
    fn about(&self) -> &'static str {
        "Grow the canvas and fill the new margin, keeping the original pixels (fal)."
    }
    fn schema(&self) -> Value {
        schema_for::<OutpaintArgs>()
    }
    fn modes(&self) -> &'static [DocKind] {
        &[DocKind::Raster]
    }
    fn is_network(&self) -> bool {
        true
    }

    fn apply(&self, project: &mut Project, args: Value, cx: &mut OpCx) -> Result<OpEffect> {
        let a: OutpaintArgs = parse_args(self.id(), args)?;
        if a.left + a.top + a.right + a.bottom == 0 {
            return Err(Error::SchemaViolation {
                op: self.id().to_string(),
                detail: "outpainting needs at least one non-zero margin".into(),
            });
        }
        let doc_id = cx.target_doc(project)?;
        project.raster(&doc_id)?;
        let (src_id, src_asset) = source_layer(project, self.id(), &a.layer, &doc_id)?;
        let src_bytes = cx.assets.get(&src_asset)?;
        let src = img::decode(&src_bytes)?;
        let (padded, mask) = img::pad(&src, a.left, a.top, a.right, a.bottom)?;
        let padded_png = img::encode_png(&padded)?;
        let mask_png = img::encode_png(&mask)?;
        let model = model_for(&self.rt, self.id(), &a.model);

        let mut body = json!({
            "prompt": a.prompt,
            "image_url": img::data_uri(&padded_png, "image/png"),
            "mask_url": img::data_uri(&mask_png, "image/png"),
            "image_size": { "width": padded.width(), "height": padded.height() },
        });
        if let Some(s) = a.seed {
            body["seed"] = json!(s);
        }
        merge_extra(&mut body, a.extra.clone());
        let key_params = json!({
            "prompt": a.prompt,
            "seed": a.seed,
            "margins": [a.left, a.top, a.right, a.bottom],
            "extra": a.extra,
        });

        if cx.dry_run {
            return dry_run_effect(&self.rt, cx, self.id(), Provider::Fal, &model, &key_params);
        }

        let gen = fal_call(
            &self.rt,
            cx,
            self.id(),
            &model,
            &body,
            &key_params,
            &[padded_png.clone(), mask_png.clone()],
        )?;
        let asset = first_asset(&gen, self.id())?;
        let prov = provenance(Provider::Fal, &model, Some(a.prompt.clone()), a.seed, &gen);

        let doc = project.raster_mut(&doc_id)?;
        doc.size = [
            doc.size[0] + a.left + a.right,
            doc.size[1] + a.top + a.bottom,
        ];
        // Existing content keeps its position relative to the original canvas.
        if a.left > 0 || a.top > 0 {
            let shift = Transform::translate(a.left as f64, a.top as f64);
            for layer in doc.layers.iter_mut() {
                layer.transform = layer.transform.then(shift);
            }
        }
        let had_selection = doc.selection.take().is_some();

        let name = a.name.clone().unwrap_or_else(|| {
            format!(
                "{} outpaint",
                doc.layer(&src_id)
                    .map(|l| l.name.clone())
                    .unwrap_or_default()
            )
        });
        let layer_id = unique_layer_id(doc, name.trim());
        let mut layer = Layer::new(
            layer_id.clone(),
            name.trim().to_string(),
            LayerKind::Pixel {
                asset: asset.clone(),
                offset: [0, 0],
            },
        );
        layer.provenance = Some(prov);
        doc.layers.push(layer);

        let mut effect = OpEffect::changed(&doc_id).with_created(layer_id.to_string());
        effect.cost_usd = Some(gen.cost_usd);
        if had_selection {
            effect = effect.warn(
                "selection-cleared",
                format!("#{doc_id}"),
                "the canvas grew, so the previous selection no longer applies",
            );
        }
        Ok(effect.with_data(json!({
            "asset": asset,
            "cached": gen.cached,
            "requestId": gen.request_id,
            "model": model,
            "size": [padded.width(), padded.height()],
        })))
    }
}

// ---------------------------------------------------------------------------------------
// ai.image.upscale
// ---------------------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct UpscaleArgs {
    /// Selector for the pixel layer to upscale.
    pub layer: String,
    /// Scale factor, 1 to 8.
    #[serde(default = "two")]
    pub factor: f64,
    /// Override the configured fal model id.
    #[serde(default)]
    pub model: Option<String>,
    /// Extra provider parameters, merged into the request body.
    #[serde(default)]
    pub extra: Option<serde_json::Value>,
}

fn two() -> f64 {
    2.0
}

pub(crate) struct Upscale {
    pub rt: Runtime,
}

impl Op for Upscale {
    fn id(&self) -> &'static str {
        "ai.image.upscale"
    }
    fn about(&self) -> &'static str {
        "Replace a pixel layer's blob with an upscaled one (fal)."
    }
    fn schema(&self) -> Value {
        schema_for::<UpscaleArgs>()
    }
    fn modes(&self) -> &'static [DocKind] {
        &[DocKind::Raster]
    }
    fn is_network(&self) -> bool {
        true
    }

    fn apply(&self, project: &mut Project, args: Value, cx: &mut OpCx) -> Result<OpEffect> {
        let a: UpscaleArgs = parse_args(self.id(), args)?;
        if !(1.0..=8.0).contains(&a.factor) {
            return Err(Error::SchemaViolation {
                op: self.id().to_string(),
                detail: format!("factor {} is outside 1..=8", a.factor),
            });
        }
        let doc_id = cx.target_doc(project)?;
        project.raster(&doc_id)?;
        let (src_id, src_asset) = source_layer(project, self.id(), &a.layer, &doc_id)?;
        let src_bytes = cx.assets.get(&src_asset)?;
        let model = model_for(&self.rt, self.id(), &a.model);

        let mut body = json!({
            "image_url": img::data_uri(&src_bytes, img::mime_for_ext(src_asset.ext())),
            "scale": a.factor,
        });
        merge_extra(&mut body, a.extra.clone());
        let key_params = json!({ "scale": a.factor, "extra": a.extra });

        if cx.dry_run {
            return dry_run_effect(&self.rt, cx, self.id(), Provider::Fal, &model, &key_params);
        }

        let gen = fal_call(
            &self.rt,
            cx,
            self.id(),
            &model,
            &body,
            &key_params,
            std::slice::from_ref(&src_bytes),
        )?;
        let asset = first_asset(&gen, self.id())?;
        let prov = provenance(Provider::Fal, &model, None, None, &gen);

        let doc = project.raster_mut(&doc_id)?;
        let layer = doc
            .layer_mut(&src_id)
            .ok_or_else(|| Error::Invalid(format!("layer '{src_id}' vanished")))?;
        let offset = match layer.kind {
            LayerKind::Pixel { offset, .. } => offset,
            _ => [0, 0],
        };
        layer.kind = LayerKind::Pixel {
            asset: asset.clone(),
            offset,
        };
        layer.provenance = Some(prov);
        // The layer now holds more pixels for the same document space.
        layer.transform = layer
            .transform
            .then(Transform::scale(1.0 / a.factor, 1.0 / a.factor));

        let mut effect = OpEffect::changed(&doc_id);
        effect.cost_usd = Some(gen.cost_usd);
        Ok(effect.with_data(json!({
            "asset": asset,
            "cached": gen.cached,
            "requestId": gen.request_id,
            "model": model,
            "factor": a.factor,
        })))
    }
}

// ---------------------------------------------------------------------------------------
// ai.image.remove-background
// ---------------------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct RemoveBackgroundArgs {
    /// Selector for the pixel layer whose subject should be cut out.
    pub layer: String,
    /// Override the configured fal model id.
    #[serde(default)]
    pub model: Option<String>,
    /// Extra provider parameters, merged into the request body.
    #[serde(default)]
    pub extra: Option<serde_json::Value>,
}

pub(crate) struct RemoveBackground {
    pub rt: Runtime,
}

impl Op for RemoveBackground {
    fn id(&self) -> &'static str {
        "ai.image.remove-background"
    }
    fn about(&self) -> &'static str {
        "Cut the subject out of a pixel layer; the result becomes that layer's mask (fal)."
    }
    fn schema(&self) -> Value {
        schema_for::<RemoveBackgroundArgs>()
    }
    fn modes(&self) -> &'static [DocKind] {
        &[DocKind::Raster]
    }
    fn is_network(&self) -> bool {
        true
    }

    fn apply(&self, project: &mut Project, args: Value, cx: &mut OpCx) -> Result<OpEffect> {
        let a: RemoveBackgroundArgs = parse_args(self.id(), args)?;
        let doc_id = cx.target_doc(project)?;
        project.raster(&doc_id)?;
        let (src_id, src_asset) = source_layer(project, self.id(), &a.layer, &doc_id)?;
        let src_bytes = cx.assets.get(&src_asset)?;
        let model = model_for(&self.rt, self.id(), &a.model);

        let mut body = json!({
            "image_url": img::data_uri(&src_bytes, img::mime_for_ext(src_asset.ext())),
        });
        merge_extra(&mut body, a.extra.clone());
        let key_params = json!({ "op": "remove-background", "extra": a.extra });

        if cx.dry_run {
            return dry_run_effect(&self.rt, cx, self.id(), Provider::Fal, &model, &key_params);
        }

        let gen = fal_call(
            &self.rt,
            cx,
            self.id(),
            &model,
            &body,
            &key_params,
            std::slice::from_ref(&src_bytes),
        )?;
        let cutout = first_asset(&gen, self.id())?;

        // Keep the pixels; the cutout becomes an ordinary, editable layer mask.
        let mask_png = img::alpha_mask_png(&img::decode(&cx.assets.get(&cutout)?)?)?;
        let mask_asset = cx.assets.put(&mask_png, "png")?;
        let prov = provenance(Provider::Fal, &model, None, None, &gen);

        let doc = project.raster_mut(&doc_id)?;
        let layer = doc
            .layer_mut(&src_id)
            .ok_or_else(|| Error::Invalid(format!("layer '{src_id}' vanished")))?;
        layer.mask = Some(Mask {
            asset: mask_asset.clone(),
            enabled: true,
            inverted: false,
            offset: [0, 0],
        });
        layer.provenance = Some(prov);

        let mut effect = OpEffect::changed(&doc_id);
        effect.cost_usd = Some(gen.cost_usd);
        Ok(effect.with_data(json!({
            "mask": mask_asset,
            "cutout": cutout,
            "cached": gen.cached,
            "requestId": gen.request_id,
            "model": model,
        })))
    }
}
