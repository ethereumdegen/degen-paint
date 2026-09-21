//! Prices, credentials and the action table, as dispatch methods.
//!
//! The AI providers are an optional dependency here, exactly as they are in the product: a
//! build without the `ai` feature is a complete editor, so `quote` prices every op at zero
//! and no provider is configured. With the feature on, the numbers are the ones the ops
//! themselves use — `AiConfig::cost_of` and the project's budget — because a submit button
//! that quotes a different price than the op charges is worse than quoting nothing.
//!
//! A key is written through `dpaint_ai::keys::store`, read back only as a source name, and
//! never returned: the Settings screen is paste-only by design.

use crate::api::Studio;
use dpaint_core::{Error, Result};
use serde_json::{json, Value};

pub(crate) fn handles(method: &str) -> bool {
    matches!(
        method,
        "quote" | "providers.status" | "providers.set" | "shortcuts"
    )
}

pub(crate) fn dispatch_ext(studio: &Studio, method: &str, params: &Value) -> Option<Result<Value>> {
    match method {
        "quote" => Some(quote(studio, params)),
        "providers.status" => Some(Ok(status())),
        "providers.set" => Some(set(params)),
        "shortcuts" => Some(Ok(shortcuts())),
        _ => None,
    }
}

fn shortcuts() -> Value {
    let rows: Vec<Value> = crate::contract::SHORTCUTS
        .iter()
        .map(|s| json!({ "id": s.id, "label": s.label, "keys": s.keys, "scope": s.scope }))
        .collect();
    json!({ "shortcuts": rows })
}

/// What one call of `op` will cost and whether the project can afford it. The UI writes this
/// into the submit button's accessible name, which is what turns a paid action into a confirm
/// card quoting degen-paint's own price.
#[cfg(feature = "ai")]
fn quote(studio: &Studio, params: &Value) -> Result<Value> {
    let op = str_param(params, "op")?;
    // Keyed on the `ai.` prefix rather than the cost table alone, so a stray `[ai.cost]`
    // entry cannot put a price on an op that sends nothing anywhere.
    let estimate = match op.starts_with("ai.") {
        true => dpaint_ai::config::AiConfig::load().cost_of(&op),
        false => 0.0,
    };
    let budget = dpaint_ai::budget::Budget::load(&crate::jobs::project_root(studio)?);
    Ok(json!({
        "op": op,
        "estimateUsd": usd(estimate),
        "spentUsd": usd(budget.spent()),
        "ceilingUsd": budget.ceiling_usd,
        "wouldExceed": budget.check(estimate).is_err(),
    }))
}

/// The ledger's own rounding, so the submit button's price and `dpaint quote` print the same
/// number; `+ 0.0` keeps a signed zero from serialising as `-0.0` and reading as a debt.
#[cfg(feature = "ai")]
fn usd(v: f64) -> f64 {
    (v * 10_000.0).round() / 10_000.0 + 0.0
}

#[cfg(not(feature = "ai"))]
fn quote(_studio: &Studio, params: &Value) -> Result<Value> {
    let op = str_param(params, "op")?;
    Ok(json!({
        "op": op,
        "estimateUsd": 0.0,
        "spentUsd": 0.0,
        "ceilingUsd": Value::Null,
        "wouldExceed": false,
    }))
}

#[cfg(feature = "ai")]
fn status() -> Value {
    let cfg = dpaint_ai::config::AiConfig::load();
    let keys = dpaint_ai::keys::SystemKeys::new();
    let mut out = serde_json::Map::new();
    for s in dpaint_ai::keys::providers_status(&keys, &cfg) {
        let mut row = json!({
            "configured": s.configured,
            "source": s.source.map(source_id),
            "envVar": s.env_var,
        });
        if let Some(note) = s.note {
            row["note"] = json!(note);
        }
        out.insert(s.provider.to_string(), row);
    }
    Value::Object(out)
}

#[cfg(not(feature = "ai"))]
fn status() -> Value {
    let row = json!({
        "configured": false,
        "source": Value::Null,
        "note": "this build has no generation providers compiled in",
    });
    json!({ "fal": row, "quiver": row })
}

/// `config-file` is an implementation detail of the resolution chain; the UI and the pack
/// speak of `config`.
#[cfg(feature = "ai")]
fn source_id(source: &str) -> &str {
    match source {
        "config-file" => "config",
        other => other,
    }
}

#[cfg(feature = "ai")]
fn set(params: &Value) -> Result<Value> {
    let provider = match str_param(params, "provider")?.as_str() {
        "fal" => dpaint_ai::keys::Provider::Fal,
        "quiver" => dpaint_ai::keys::Provider::Quiver,
        other => {
            return Err(Error::Invalid(format!(
                "unknown provider '{other}'; expected 'fal' or 'quiver'"
            )))
        }
    };
    dpaint_ai::keys::store(provider, &str_param(params, "key")?)?;
    Ok(status())
}

#[cfg(not(feature = "ai"))]
fn set(params: &Value) -> Result<Value> {
    str_param(params, "provider")?;
    str_param(params, "key")?;
    Err(Error::Invalid(
        "this build has no generation providers compiled in, so there is nothing to key".into(),
    ))
}

fn str_param(params: &Value, key: &str) -> Result<String> {
    params
        .get(key)
        .and_then(|v| v.as_str())
        .map(String::from)
        .ok_or_else(|| Error::Invalid(format!("missing '{key}'")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use dpaint_core::doc::{Document, RasterDoc};
    use dpaint_core::{DocId, Project, Workspace};

    fn studio() -> (tempfile::TempDir, Studio) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("p.dpaint");
        let project = Project::new(
            "demo",
            Document::Raster(RasterDoc::new(DocId::from("doc_main"), "main", 16, 16)),
        );
        Workspace::create(&root, project).unwrap();
        let s = Studio::open(&root).unwrap();
        (tmp, s)
    }

    #[test]
    fn an_op_that_sends_nothing_anywhere_is_quoted_at_zero() {
        let (_t, s) = studio();
        let q = s
            .dispatch("quote", &json!({ "op": "raster.filter.gaussian-blur" }))
            .unwrap();
        assert_eq!(q["op"], "raster.filter.gaussian-blur");
        assert_eq!(q["estimateUsd"], 0.0);
        assert_eq!(q["wouldExceed"], false);
    }

    /// The button's price must be the provider's price, not a second table.
    #[cfg(feature = "ai")]
    #[test]
    fn a_paid_op_is_quoted_at_the_configured_cost() {
        let (_t, s) = studio();
        let q = s
            .dispatch("quote", &json!({ "op": "ai.image.generate" }))
            .unwrap();
        assert_eq!(
            q["estimateUsd"].as_f64().unwrap(),
            dpaint_ai::config::AiConfig::load().cost_of("ai.image.generate")
        );
        assert!(q["estimateUsd"].as_f64().unwrap() > 0.0);
        assert_eq!(q["spentUsd"], 0.0, "a fresh project has spent nothing");
    }

    #[test]
    fn provider_status_reports_configuration_without_a_key_anywhere_in_it() {
        let (_t, s) = studio();
        let st = s.dispatch("providers.status", &json!({})).unwrap();
        for p in ["fal", "quiver"] {
            assert!(st[p]["configured"].is_boolean(), "{st}");
        }
        let text = st.to_string();
        assert!(!text.contains("key\":\""), "{text}");
    }

    #[test]
    fn an_unknown_provider_is_refused_before_anything_is_stored() {
        let (_t, s) = studio();
        let err = s
            .dispatch(
                "providers.set",
                &json!({ "provider": "openai", "key": "sk-nope" }),
            )
            .unwrap_err();
        assert_eq!(err.code(), "invalid");
        assert!(!err.to_string().contains("sk-nope"), "{err}");
    }

    #[test]
    fn the_shortcut_table_is_what_the_ui_reads() {
        let (_t, s) = studio();
        let rows = s.dispatch("shortcuts", &json!({})).unwrap();
        let rows = rows["shortcuts"].as_array().unwrap();
        assert_eq!(rows.len(), crate::contract::SHORTCUTS.len());
        assert!(rows
            .iter()
            .any(|r| r["id"] == "project.new" && r["keys"] == "CmdOrCtrl+N"));
    }
}
