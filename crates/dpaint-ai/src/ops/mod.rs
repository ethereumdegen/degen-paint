//! The `ai.*` op catalog and the machinery every generation op shares: key checks, the
//! request cache, the budget ledger, and provenance.

mod images;
mod meta;
mod textures;
mod vectors;

use crate::budget::{Budget, Spend};
use crate::cache::{cache_key, Cache, CacheEntry};
use crate::keys::{ApiKey, Provider};
use crate::{Blob, Runtime};
use dpaint_core::doc::common::Provenance;
use dpaint_core::doc::raster::{Layer, LayerKind};
use dpaint_core::{now_iso, AssetRef, Error, LayerId, Op, OpCx, OpEffect, Result};
use serde_json::{json, Value};

pub(crate) fn catalog(rt: Runtime) -> Vec<Box<dyn Op>> {
    vec![
        Box::new(images::Generate { rt: rt.clone() }),
        Box::new(images::Edit { rt: rt.clone() }),
        Box::new(images::Inpaint { rt: rt.clone() }),
        Box::new(images::Outpaint { rt: rt.clone() }),
        Box::new(images::Upscale { rt: rt.clone() }),
        Box::new(images::RemoveBackground { rt: rt.clone() }),
        Box::new(textures::TextureGenerate { rt: rt.clone() }),
        Box::new(vectors::VectorGenerate { rt: rt.clone() }),
        Box::new(vectors::Vectorize { rt: rt.clone() }),
        Box::new(meta::ProviderStatusOp { rt }),
        Box::new(meta::BudgetSet),
        Box::new(meta::BudgetStatus),
    ]
}

/// One completed provider call: what it produced, what it cost, and whether it cost anything
/// at all.
pub(crate) struct Generated {
    pub assets: Vec<AssetRef>,
    pub request_id: Option<String>,
    pub cost_usd: f64,
    pub cached: bool,
}

pub(crate) fn require_key(rt: &Runtime, provider: Provider) -> Result<ApiKey> {
    rt.keys
        .key(provider)
        .ok_or_else(|| Error::ProviderUnconfigured(provider.id().to_string()))
}

// These are internal helpers whose parameters are genuinely independent; bundling them
// into a struct at a couple of call sites would add indirection, not clarity.
#[allow(clippy::too_many_arguments)]
/// Cache lookup, budget pre-check, the call itself, then accounting — in that order, so a
/// repeat never bills and an over-budget request never leaves the machine.
pub(crate) fn run_cached<F>(
    rt: &Runtime,
    cx: &OpCx,
    op_id: &str,
    provider: Provider,
    model: &str,
    params: &Value,
    inputs: &[Vec<u8>],
    call: F,
) -> Result<Generated>
where
    F: FnOnce(&ApiKey) -> Result<(Option<String>, Option<f64>, Vec<Blob>)>,
{
    let key = require_key(rt, provider)?;
    let root = cx.assets.root().to_path_buf();
    let ck = cache_key(provider.id(), model, params, inputs);

    let mut cache = Cache::load(&root);
    if let Some(hit) = cache.get(&ck, cx.assets) {
        return Ok(Generated {
            assets: hit.assets.clone(),
            request_id: hit.request_id.clone(),
            cost_usd: 0.0,
            cached: true,
        });
    }

    let estimate = rt.config.cost_of(op_id);
    let mut budget = Budget::load(&root);
    budget.check(estimate)?;

    let (request_id, reported, blobs) = call(&key)?;
    let cost = reported.unwrap_or(estimate);

    let assets = blobs
        .iter()
        .map(|b| cx.assets.put(&b.bytes, &b.ext))
        .collect::<Result<Vec<_>>>()?;

    budget.record(Spend {
        at: now_iso(),
        op: op_id.to_string(),
        provider: provider.id().to_string(),
        model: model.to_string(),
        cost_usd: cost,
        request_id: request_id.clone(),
    })?;
    cache.insert(
        ck,
        CacheEntry {
            provider: provider.id().to_string(),
            model: model.to_string(),
            request_id: request_id.clone(),
            assets: assets.clone(),
            cost_usd: cost,
            at: now_iso(),
        },
    )?;

    Ok(Generated {
        assets,
        request_id,
        cost_usd: cost,
        cached: false,
    })
}

/// A fal call: submit, poll, download, cache, account.
///
/// `body` is what goes on the wire; `key_params` is the semantic request used for the cache
/// key, so two calls that differ only in how an input was encoded still hit the same entry.
pub(crate) fn fal_call(
    rt: &Runtime,
    cx: &OpCx,
    op_id: &str,
    model: &str,
    body: &Value,
    key_params: &Value,
    inputs: &[Vec<u8>],
) -> Result<Generated> {
    run_cached(
        rt,
        cx,
        op_id,
        Provider::Fal,
        model,
        key_params,
        inputs,
        |key| {
            let client =
                crate::fal::FalClient::new(rt.transport.as_ref(), rt.config.as_ref(), key.expose());
            let run = client.run(model, body)?;
            let blobs = client.download_images(&run.payload)?;
            Ok((Some(run.request_id), run.reported_cost, blobs))
        },
    )
}

// These are internal helpers whose parameters are genuinely independent; bundling them
// into a struct at a couple of call sites would add indirection, not clarity.
#[allow(clippy::too_many_arguments)]
/// A Quiver call, returning the SVG markup. The markup is stored so a cache hit can replay
/// it, but what lands in the document is parsed geometry, never the blob.
pub(crate) fn quiver_call(
    rt: &Runtime,
    cx: &OpCx,
    op_id: &str,
    model: &str,
    url: &str,
    body: &Value,
    key_params: &Value,
    inputs: &[Vec<u8>],
) -> Result<(Generated, Vec<String>)> {
    let keyed = json!({ "endpoint": url, "request": key_params });
    let gen = run_cached(
        rt,
        cx,
        op_id,
        Provider::Quiver,
        model,
        &keyed,
        inputs,
        |key| {
            let client = crate::quiver::QuiverClient::new(
                rt.transport.as_ref(),
                rt.config.as_ref(),
                key.expose(),
            );
            let run = client.post(url, body)?;
            let blobs = run
                .svgs
                .iter()
                .map(|s| Blob {
                    bytes: s.clone().into_bytes(),
                    ext: "svg".into(),
                })
                .collect();
            Ok((run.request_id, run.reported_cost, blobs))
        },
    )?;
    let svgs = gen
        .assets
        .iter()
        .map(|a| {
            cx.assets
                .get(a)
                .map(|b| String::from_utf8_lossy(&b).into_owned())
        })
        .collect::<Result<Vec<_>>>()?;
    Ok((gen, svgs))
}

/// What `--dry-run` reports: the request that would be sent and what it would cost. Nothing
/// is mutated, nothing is sent, and a missing key is still an error — a dry run that lies
/// about being configured is worse than no dry run.
pub(crate) fn dry_run_effect(
    rt: &Runtime,
    cx: &OpCx,
    op_id: &str,
    provider: Provider,
    model: &str,
    params: &Value,
) -> Result<OpEffect> {
    require_key(rt, provider)?;
    let estimate = rt.config.cost_of(op_id);
    let budget = Budget::load(cx.assets.root());
    budget.check(estimate)?;
    Ok(OpEffect::default().with_data(json!({
        "dryRun": true,
        "provider": provider.id(),
        "model": model,
        "request": params,
        "estimatedCostUsd": estimate,
        "spentUsd": budget.spent(),
        "ceilingUsd": budget.ceiling_usd,
    })))
}

pub(crate) fn provenance(
    provider: Provider,
    model: &str,
    prompt: Option<String>,
    seed: Option<i64>,
    gen: &Generated,
) -> Provenance {
    Provenance {
        provider: provider.id().to_string(),
        model: model.to_string(),
        prompt,
        seed,
        request_id: gen.request_id.clone(),
        at: now_iso(),
        cost_usd: Some(gen.cost_usd),
    }
}

/// `"1536x1024"` → `(1536, 1024)`.
pub(crate) fn parse_size(op: &str, s: &str) -> Result<(u32, u32)> {
    let (w, h) = s
        .split_once(['x', 'X', '*'])
        .ok_or_else(|| Error::SchemaViolation {
            op: op.to_string(),
            detail: format!("size '{s}' is not WIDTHxHEIGHT"),
        })?;
    let parse = |v: &str| {
        v.trim()
            .parse::<u32>()
            .ok()
            .filter(|n| *n > 0)
            .ok_or_else(|| Error::SchemaViolation {
                op: op.to_string(),
                detail: format!("size '{s}' is not WIDTHxHEIGHT"),
            })
    };
    Ok((parse(w)?, parse(h)?))
}

/// Merge caller-supplied passthrough parameters, so an agent can drive an endpoint this
/// crate has never heard of without waiting for a release.
pub(crate) fn merge_extra(params: &mut Value, extra: Option<Value>) {
    let Some(Value::Object(extra)) = extra else {
        return;
    };
    if let Some(obj) = params.as_object_mut() {
        for (k, v) in extra {
            obj.insert(k, v);
        }
    }
}

/// A layer id derived from a name, made unique within the document.
pub(crate) fn unique_layer_id(doc: &dpaint_core::RasterDoc, name: &str) -> LayerId {
    let base = LayerId::from_name(name);
    if doc.layer(&base).is_none() {
        return base;
    }
    for n in 2..1000 {
        let candidate = LayerId::from(format!("{}-{n}", base.as_str()));
        if doc.layer(&candidate).is_none() {
            return candidate;
        }
    }
    LayerId::generate()
}

/// Insert `new` directly above `target`, wherever `target` lives in the group tree.
pub(crate) fn insert_above(layers: &mut Vec<Layer>, target: &LayerId, new: Layer) -> bool {
    if let Some(i) = layers.iter().position(|l| &l.id == target) {
        layers.insert(i + 1, new);
        return true;
    }
    for l in layers.iter_mut() {
        if let LayerKind::Group { layers } = &mut l.kind {
            if insert_above(layers, target, new.clone()) {
                return true;
            }
        }
    }
    false
}

/// The pixels behind a layer. Generation ops read their input from the asset store rather
/// than from a live composite, so they never depend on a renderer.
pub(crate) fn pixel_asset(op: &str, layer: &Layer) -> Result<AssetRef> {
    match &layer.kind {
        LayerKind::Pixel { asset, .. } => Ok(asset.clone()),
        _ => Err(Error::Invalid(format!(
            "{op} needs a pixel layer; '{}' is a {} layer — rasterize it first \
             (raster.layer.rasterize)",
            layer.id,
            layer.type_name()
        ))),
    }
}
