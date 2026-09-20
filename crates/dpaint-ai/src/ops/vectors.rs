//! Quiver-backed vector ops.
//!
//! What lands in the document is parsed geometry — real Bézier paths, fills, gradients,
//! groups — indistinguishable from hand-authored work. An SVG is never stored as a blob and
//! referenced; it is imported through the ordinary vector import path and then belongs to the
//! editor.

use super::*;
use crate::image as img;
use crate::keys::Provider;
use crate::quiver::{generation_body, vectorization_body};
use crate::Runtime;
use dpaint_core::doc::vector::{Artboard, VKind, VObject};
use dpaint_core::doc::{DocKind, Rect, Transform};
use dpaint_core::{parse_args, resolve_one, schema_for, ArtboardId, DocId, ObjectId, Project, VectorDoc};
use schemars::JsonSchema;
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};

/// Gap between variant artboards, so three options can be rendered side by side.
const ARTBOARD_GAP: f64 = 24.0;

fn quiver_model(rt: &Runtime, over: &Option<String>) -> String {
    over.clone().unwrap_or_else(|| rt.config.quiver.model.clone())
}

/// Import one SVG and merge it into the target document as its own artboard.
///
/// Returns the ids that were created, newest last.
fn merge_svg(
    target: &mut VectorDoc,
    svg: &str,
    label: &str,
    prov: &dpaint_core::doc::common::Provenance,
) -> Result<Vec<String>> {
    let imported = dpaint_vector::import_svg(svg, DocId::from("doc_import"), label)?;
    let (w, h) = imported.size();

    let x0 = target
        .artboards
        .iter()
        .map(|a| a.rect.right())
        .fold(0.0_f64, f64::max);
    let x0 = if target.artboards.is_empty() { 0.0 } else { x0 + ARTBOARD_GAP };

    let mut used: BTreeSet<String> = target.walk().iter().map(|o| o.id.to_string()).collect();
    let mut renames: BTreeMap<String, String> = BTreeMap::new();
    let mut objects = imported.objects;
    uniquify(&mut objects, &mut used, &mut renames);
    if !renames.is_empty() {
        retarget_clips(&mut objects, &renames);
    }
    stamp_provenance(&mut objects, prov);

    let mut created = Vec::new();
    let artboard_id = unique_artboard_id(target, label);
    target.artboards.push(Artboard {
        id: artboard_id.clone(),
        name: label.to_string(),
        rect: Rect::new(x0, 0.0, w, h),
        background: None,
    });
    created.push(artboard_id.to_string());

    let shift = Transform::translate(x0, 0.0);
    for mut o in objects {
        if x0 != 0.0 {
            o.transform = o.transform.then(shift);
        }
        created.push(o.id.to_string());
        target.objects.push(o);
    }
    Ok(created)
}

fn unique_artboard_id(doc: &VectorDoc, name: &str) -> ArtboardId {
    let base = ArtboardId::from_name(name);
    if !doc.artboards.iter().any(|a| a.id == base) {
        return base;
    }
    for n in 2..1000 {
        let candidate = ArtboardId::from(format!("{}-{n}", base.as_str()));
        if !doc.artboards.iter().any(|a| a.id == candidate) {
            return candidate;
        }
    }
    ArtboardId::generate()
}

fn uniquify(
    objects: &mut Vec<VObject>,
    used: &mut BTreeSet<String>,
    renames: &mut BTreeMap<String, String>,
) {
    for o in objects.iter_mut() {
        let original = o.id.to_string();
        if used.contains(&original) {
            let mut candidate = String::new();
            for n in 2..10_000 {
                candidate = format!("{original}-{n}");
                if !used.contains(&candidate) {
                    break;
                }
            }
            renames.insert(original, candidate.clone());
            o.id = ObjectId::from(candidate);
        }
        used.insert(o.id.to_string());
        if let VKind::Group { objects } = &mut o.kind {
            uniquify(objects, used, renames);
        }
    }
}

fn retarget_clips(objects: &mut Vec<VObject>, renames: &BTreeMap<String, String>) {
    for o in objects.iter_mut() {
        if let Some(clip) = &o.clip {
            if let Some(new) = renames.get(clip.as_str()) {
                o.clip = Some(ObjectId::from(new.clone()));
            }
        }
        if let VKind::Group { objects } = &mut o.kind {
            retarget_clips(objects, renames);
        }
    }
}

fn stamp_provenance(objects: &mut Vec<VObject>, prov: &dpaint_core::doc::common::Provenance) {
    for o in objects.iter_mut() {
        o.provenance = Some(prov.clone());
        if let VKind::Group { objects } = &mut o.kind {
            stamp_provenance(objects, prov);
        }
    }
}

// ---------------------------------------------------------------------------------------
// ai.vector.generate
// ---------------------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct VectorGenerateArgs {
    /// What to draw.
    pub prompt: String,
    /// Structural guidance, e.g. "clean geometry, production-ready SVG structure".
    #[serde(default)]
    pub instructions: Option<String>,
    /// Number of variants; each becomes its own artboard so an agent can render and pick.
    #[serde(default = "one")]
    pub n: u32,
    /// Seed, for a reproducible result.
    #[serde(default)]
    pub seed: Option<i64>,
    /// Base name for the artboards and objects.
    #[serde(default)]
    pub name: Option<String>,
    /// Override the configured Quiver model, e.g. `arrow-2-telos`.
    #[serde(default)]
    pub model: Option<String>,
}

fn one() -> u32 {
    1
}

pub(crate) struct VectorGenerate {
    pub rt: Runtime,
}

impl Op for VectorGenerate {
    fn id(&self) -> &'static str {
        "ai.vector.generate"
    }
    fn about(&self) -> &'static str {
        "Generate editable vector objects from a prompt; one artboard per variant (Quiver)."
    }
    fn schema(&self) -> Value {
        schema_for::<VectorGenerateArgs>()
    }
    fn modes(&self) -> &'static [DocKind] {
        &[DocKind::Vector]
    }
    fn is_network(&self) -> bool {
        true
    }

    fn apply(&self, project: &mut Project, args: Value, cx: &mut OpCx) -> Result<OpEffect> {
        let a: VectorGenerateArgs = parse_args(self.id(), args)?;
        if !(1..=8).contains(&a.n) {
            return Err(Error::SchemaViolation {
                op: self.id().to_string(),
                detail: format!("n={} is outside 1..=8", a.n),
            });
        }
        let doc_id = cx.target_doc(project)?;
        project.vector(&doc_id)?;
        let model = quiver_model(&self.rt, &a.model);
        let body = generation_body(&model, &a.prompt, a.instructions.as_deref(), a.n, a.seed);

        let url = crate::quiver::generations_url(self.rt.config.as_ref());

        if cx.dry_run {
            return dry_run_effect(&self.rt, cx, self.id(), Provider::Quiver, &model, &body);
        }

        let (gen, svgs) = quiver_call(
            &self.rt,
            cx,
            self.id(),
            &model,
            &url,
            &body,
            &body,
            &[],
        )?;
        let prov = provenance(Provider::Quiver, &model, Some(a.prompt.clone()), a.seed, &gen);

        let base = a.name.clone().unwrap_or_else(|| "generated".into());
        let doc = project.vector_mut(&doc_id)?;
        let mut created = Vec::new();
        for (i, svg) in svgs.iter().enumerate() {
            let label = if svgs.len() > 1 {
                format!("{base}-{}", i + 1)
            } else {
                base.clone()
            };
            created.extend(merge_svg(doc, svg, &label, &prov)?);
        }

        let mut effect = OpEffect::changed(&doc_id);
        for id in &created {
            effect = effect.with_created(id.clone());
        }
        effect.cost_usd = Some(gen.cost_usd);
        if svgs.len() < a.n as usize {
            effect = effect.warn(
                "fewer-variants",
                format!("#{doc_id}"),
                format!("asked for {} variants, the provider returned {}", a.n, svgs.len()),
            );
        }
        Ok(effect.with_data(json!({
            "variants": svgs.len(),
            "cached": gen.cached,
            "requestId": gen.request_id,
            "model": model,
        })))
    }
}

// ---------------------------------------------------------------------------------------
// ai.vector.vectorize
// ---------------------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct VectorizeArgs {
    /// Selector for the pixel layer to vectorize, e.g. `#lyr_sketch`.
    pub from: String,
    /// Document holding that layer, when it is not the active one.
    #[serde(default)]
    pub from_doc: Option<String>,
    /// Trim empty margins before tracing.
    #[serde(default)]
    pub auto_crop: bool,
    /// Name for the resulting artboard and objects.
    #[serde(default)]
    pub name: Option<String>,
    /// Override the configured Quiver model.
    #[serde(default)]
    pub model: Option<String>,
}

pub(crate) struct Vectorize {
    pub rt: Runtime,
}

impl Op for Vectorize {
    fn id(&self) -> &'static str {
        "ai.vector.vectorize"
    }
    fn about(&self) -> &'static str {
        "Trace a raster layer into editable vector objects (Quiver)."
    }
    fn schema(&self) -> Value {
        schema_for::<VectorizeArgs>()
    }
    fn modes(&self) -> &'static [DocKind] {
        &[DocKind::Vector]
    }
    fn is_network(&self) -> bool {
        true
    }

    fn apply(&self, project: &mut Project, args: Value, cx: &mut OpCx) -> Result<OpEffect> {
        let a: VectorizeArgs = parse_args(self.id(), args)?;
        let doc_id = cx.target_doc(project)?;
        project.vector(&doc_id)?;

        let source_doc = match &a.from_doc {
            Some(d) => project.resolve_doc(Some(d))?,
            None => project.active.clone(),
        };
        let m = resolve_one(project, &a.from, Some(&source_doc))?;
        let raster = project.raster(&m.document)?;
        let layer_id = dpaint_core::LayerId::from(m.id.as_str());
        let layer = raster
            .layer(&layer_id)
            .ok_or_else(|| Error::Invalid(format!("'{}' is not a layer of {}", m.id, m.document)))?;
        let asset = pixel_asset(self.id(), layer)?;
        let bytes = cx.assets.get(&asset)?;

        let model = quiver_model(&self.rt, &a.model);
        let body = vectorization_body(
            &model,
            &img::data_uri(&bytes, img::mime_for_ext(asset.ext())),
            a.auto_crop,
        );
        let key_params = json!({ "model": model, "auto_crop": a.auto_crop });
        let url = crate::quiver::vectorizations_url(self.rt.config.as_ref());

        if cx.dry_run {
            return dry_run_effect(&self.rt, cx, self.id(), Provider::Quiver, &model, &key_params);
        }

        let (gen, svgs) = quiver_call(
            &self.rt,
            cx,
            self.id(),
            &model,
            &url,
            &body,
            &key_params,
            &[bytes.clone()],
        )?;
        let prov = provenance(Provider::Quiver, &model, None, None, &gen);

        let label = a.name.clone().unwrap_or_else(|| layer.name.clone());
        let label = if label.trim().is_empty() { "traced".to_string() } else { label };
        let doc = project.vector_mut(&doc_id)?;
        let mut created = Vec::new();
        for svg in &svgs {
            created.extend(merge_svg(doc, svg, &label, &prov)?);
        }

        let mut effect = OpEffect::changed(&doc_id);
        for id in &created {
            effect = effect.with_created(id.clone());
        }
        effect.cost_usd = Some(gen.cost_usd);
        Ok(effect.with_data(json!({
            "source": m.id,
            "cached": gen.cached,
            "requestId": gen.request_id,
            "model": model,
        })))
    }
}
