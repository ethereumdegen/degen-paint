//! `ai.texture.generate` — a PBR map set wired into a glTF material.
//!
//! Each map becomes a raster document with a pixel layer, and the material binds that
//! document. So a generated normal map is an ordinary image an agent can inspect, adjust,
//! blur or repaint with the raster ops, and the binding updates with it.

use super::*;
use crate::keys::Provider;
use crate::Runtime;
use dpaint_core::doc::model::{TextureBinding, TextureSlot, TextureSource};
use dpaint_core::doc::DocKind;
use dpaint_core::{
    parse_args, resolve_one, schema_for, DocId, Document, MaterialId, Project, RasterDoc,
};
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct TextureArgs {
    /// Selector for the material to texture, e.g. `#mat_gold`.
    pub material: String,
    /// What the surface looks like, e.g. "brushed gold, fine scratches".
    pub prompt: String,
    /// Which maps to generate: base, normal, roughness, occlusion, emissive.
    #[serde(default = "default_maps")]
    pub maps: Vec<String>,
    /// Square texture size in pixels.
    #[serde(default = "default_size")]
    pub size: u32,
    /// Seed, for a reproducible result.
    #[serde(default)]
    pub seed: Option<i64>,
    /// Override the configured fal model id.
    #[serde(default)]
    pub model: Option<String>,
    /// Extra provider parameters, merged into every request body.
    #[serde(default)]
    pub extra: Option<Value>,
}

fn default_maps() -> Vec<String> {
    vec!["base".into(), "normal".into(), "roughness".into()]
}

fn default_size() -> u32 {
    1024
}

/// The slot a map name binds to, and the wording that makes a general image model produce
/// that kind of map.
fn map_spec(name: &str) -> Option<(TextureSlot, &'static str)> {
    match name.to_ascii_lowercase().as_str() {
        "base" | "basecolor" | "base-color" | "albedo" | "diffuse" => (
            TextureSlot::BaseColor,
            "seamless tileable PBR base colour texture, flat even lighting, no shadows",
        )
            .into(),
        "normal" => (
            TextureSlot::Normal,
            "seamless tileable tangent-space normal map, blue-violet, surface detail only",
        )
            .into(),
        "roughness" | "metallicroughness" | "metallic-roughness" => (
            TextureSlot::MetallicRoughness,
            "seamless tileable greyscale roughness map, white is rough, black is glossy",
        )
            .into(),
        "occlusion" | "ao" => (
            TextureSlot::Occlusion,
            "seamless tileable greyscale ambient occlusion map, white is open, black is occluded",
        )
            .into(),
        "emissive" | "emission" => (
            TextureSlot::Emissive,
            "seamless tileable emissive map, black where the surface does not glow",
        )
            .into(),
        _ => None,
    }
}

fn slot_name(slot: TextureSlot) -> &'static str {
    match slot {
        TextureSlot::BaseColor => "base",
        TextureSlot::Normal => "normal",
        TextureSlot::MetallicRoughness => "roughness",
        TextureSlot::Occlusion => "occlusion",
        TextureSlot::Emissive => "emissive",
    }
}

fn unique_doc_id(project: &Project, name: &str) -> DocId {
    let base = DocId::from_name(name);
    if !project.documents.contains_key(&base) {
        return base;
    }
    for n in 2..1000 {
        let candidate = DocId::from(format!("{}-{n}", base.as_str()));
        if !project.documents.contains_key(&candidate) {
            return candidate;
        }
    }
    DocId::generate()
}

pub(crate) struct TextureGenerate {
    pub rt: Runtime,
}

impl Op for TextureGenerate {
    fn id(&self) -> &'static str {
        "ai.texture.generate"
    }
    fn about(&self) -> &'static str {
        "Generate a PBR map set and wire it into a material as editable raster documents (fal)."
    }
    fn schema(&self) -> Value {
        schema_for::<TextureArgs>()
    }
    fn modes(&self) -> &'static [DocKind] {
        &[DocKind::Model]
    }
    fn is_network(&self) -> bool {
        true
    }

    fn apply(&self, project: &mut Project, args: Value, cx: &mut OpCx) -> Result<OpEffect> {
        let a: TextureArgs = parse_args(self.id(), args)?;
        if a.maps.is_empty() {
            return Err(Error::SchemaViolation {
                op: self.id().to_string(),
                detail: "no maps requested".into(),
            });
        }
        if !(16..=8192).contains(&a.size) {
            return Err(Error::SchemaViolation {
                op: self.id().to_string(),
                detail: format!("size {} is outside 16..=8192", a.size),
            });
        }
        let mut specs = Vec::new();
        for m in &a.maps {
            let spec = map_spec(m).ok_or_else(|| Error::SchemaViolation {
                op: self.id().to_string(),
                detail: format!(
                    "unknown map '{m}' (expected base, normal, roughness, occlusion or emissive)"
                ),
            })?;
            specs.push(spec);
        }

        let doc_id = cx.target_doc(project)?;
        let model_doc = project.model(&doc_id)?;
        let m = resolve_one(project, &a.material, Some(&doc_id))?;
        let material_id = MaterialId::from(m.id.as_str());
        let material = model_doc
            .materials
            .iter()
            .find(|mat| mat.id == material_id)
            .ok_or_else(|| Error::Invalid(format!("'{}' is not a material of {doc_id}", m.id)))?;
        let material_name = if material.name.is_empty() {
            material.id.to_string()
        } else {
            material.name.clone()
        };
        let fal_model = a
            .model
            .clone()
            .unwrap_or_else(|| self.rt.config.fal.model_for(self.id()).to_string());

        if cx.dry_run {
            let planned: Vec<Value> = specs
                .iter()
                .map(|(slot, suffix)| {
                    json!({ "map": slot_name(*slot), "prompt": format!("{}, {suffix}", a.prompt) })
                })
                .collect();
            let mut effect = dry_run_effect(
                &self.rt,
                cx,
                self.id(),
                Provider::Fal,
                &fal_model,
                &json!({ "maps": planned, "size": a.size }),
            )?;
            if let Some(data) = effect.data.as_mut() {
                data["estimatedCostUsd"] =
                    json!(self.rt.config.cost_of(self.id()) * specs.len() as f64);
            }
            return Ok(effect);
        }

        // Every map is a separate call; each one is cached and budget-checked on its own.
        let mut generated = Vec::new();
        for (slot, suffix) in &specs {
            let prompt = format!("{}, {suffix}", a.prompt);
            let mut body = json!({
                "prompt": prompt,
                "image_size": { "width": a.size, "height": a.size },
                "num_images": 1,
            });
            if let Some(s) = a.seed {
                body["seed"] = json!(s);
            }
            merge_extra(&mut body, a.extra.clone());
            let gen = fal_call(&self.rt, cx, self.id(), &fal_model, &body, &body, &[])?;
            let asset = gen
                .assets
                .first()
                .cloned()
                .ok_or_else(|| Error::ProviderError {
                    provider: "fal".into(),
                    detail: format!("{} map produced no image", slot_name(*slot)),
                })?;
            generated.push((*slot, prompt, asset, gen));
        }

        let mut effect = OpEffect::changed(&doc_id);
        let mut total = 0.0;
        let mut bindings = Vec::new();
        let mut cached_all = true;

        for (slot, prompt, asset, gen) in generated {
            let tex_doc_id =
                unique_doc_id(project, &format!("tex {material_name} {}", slot_name(slot)));
            let mut tex = RasterDoc::new(
                tex_doc_id.clone(),
                format!("{material_name} {}", slot_name(slot)),
                a.size,
                a.size,
            );
            let layer_id = unique_layer_id(&tex, slot_name(slot));
            let mut layer = Layer::new(
                layer_id.clone(),
                slot_name(slot),
                LayerKind::Pixel {
                    asset: asset.clone(),
                    offset: [0, 0],
                },
            );
            layer.provenance = Some(provenance(
                Provider::Fal,
                &fal_model,
                Some(prompt),
                a.seed,
                &gen,
            ));
            tex.layers.push(layer);
            project.add_document(Document::Raster(tex));

            total += gen.cost_usd;
            cached_all &= gen.cached;
            effect = effect
                .with_created(tex_doc_id.to_string())
                .with_created(layer_id.to_string());
            bindings.push((slot, tex_doc_id));
        }

        let model_doc = project.model_mut(&doc_id)?;
        let material = model_doc
            .materials
            .iter_mut()
            .find(|mat| mat.id == material_id)
            .ok_or_else(|| Error::Invalid(format!("material '{material_id}' vanished")))?;
        let mut maps = Vec::new();
        for (slot, tex_doc_id) in bindings {
            material.textures.retain(|t| t.slot != slot);
            material.textures.push(TextureBinding {
                slot,
                source: TextureSource::Document {
                    document: tex_doc_id.clone(),
                },
                scale: 1.0,
                uv_set: 0,
            });
            maps.push(json!({ "map": slot_name(slot), "document": tex_doc_id }));
        }

        effect.changed.push(doc_id.clone());
        effect.changed.dedup();
        effect.cost_usd = Some(total);
        Ok(effect.with_data(json!({
            "material": material_id.to_string(),
            "maps": maps,
            "cached": cached_all,
            "model": fal_model,
        })))
    }
}
