//! What keeps a looping agent with an API key from becoming a billing incident: a missing
//! key changes nothing, a repeat costs nothing, and a ceiling is enforced before the call.

mod common;

use common::*;
use dpaint_core::doc::raster::LayerKind;
use serde_json::json;
use std::sync::Arc;

#[test]
fn a_missing_key_is_provider_unconfigured_and_mutates_nothing() {
    let transport = Arc::new(fal_job("fal-ai/flux/dev", png(8, 8, [1, 1, 1, 255])));
    let mut fx = Fixture::new(transport.clone(), dpaint_ai::StaticKeys::new());
    let before = fx.snapshot();

    let err = fx
        .engine
        .apply(
            "ai.image.generate",
            json!({"prompt": "a barn", "size": "8x8"}),
            None,
            false,
        )
        .unwrap_err();

    assert_eq!(err.code(), "provider_unconfigured");
    assert_eq!(err.exit_code(), 5);
    assert_eq!(transport.call_count(), 0, "no request left the machine");
    assert_eq!(fx.snapshot(), before, "the project is byte-identical");
    assert_eq!(fx.journal_len(), 0, "nothing was journaled");
    assert_eq!(fx.raster().layers.len(), 1, "no layer was created");
}

#[test]
fn an_identical_repeat_is_served_from_cache_at_zero_cost_and_without_a_request() {
    let transport = Arc::new(fal_job("fal-ai/flux/dev", png(16, 16, [3, 4, 5, 255])));
    let mut fx = Fixture::new(transport.clone(), fal_keys());
    let args = json!({"prompt": "same thing", "size": "16x16", "seed": 1});

    let first = fx
        .engine
        .apply("ai.image.generate", args.clone(), None, false)
        .unwrap();
    let calls_after_first = transport.call_count();
    assert!(calls_after_first > 0);
    assert_eq!(first.effect.cost_usd, Some(0.025));

    let second = fx
        .engine
        .apply("ai.image.generate", args, None, false)
        .unwrap();

    assert_eq!(second.effect.cost_usd, Some(0.0), "a cache hit is free");
    assert_eq!(
        transport.call_count(),
        calls_after_first,
        "the provider was not contacted a second time"
    );
    assert_eq!(second.effect.data.unwrap()["cached"], true);

    // Both layers reference the same blob in the content-addressed store.
    let layers = &fx.raster().layers;
    assert_eq!(layers.len(), 3);
    let asset_of = |i: usize| match &layers[i].kind {
        LayerKind::Pixel { asset, .. } => asset.clone(),
        _ => panic!("expected a pixel layer"),
    };
    assert_eq!(asset_of(1), asset_of(2));

    // And the spend ledger recorded one call, not two.
    let status = fx
        .engine
        .apply("ai.budget.status", json!({}), None, false)
        .unwrap()
        .effect
        .data
        .unwrap();
    assert_eq!(status["calls"], 1);
    assert_eq!(status["spentUsd"], 0.025);
}

#[test]
fn different_arguments_are_a_different_request() {
    let transport = Arc::new(fal_job("fal-ai/flux/dev", png(16, 16, [3, 4, 5, 255])));
    let mut fx = Fixture::new(transport.clone(), fal_keys());

    fx.engine
        .apply(
            "ai.image.generate",
            json!({"prompt": "one", "size": "16x16"}),
            None,
            false,
        )
        .unwrap();
    let after_first = transport.call_count();
    fx.engine
        .apply(
            "ai.image.generate",
            json!({"prompt": "two", "size": "16x16"}),
            None,
            false,
        )
        .unwrap();

    assert!(
        transport.call_count() > after_first,
        "a new prompt is a new call"
    );
}

#[test]
fn a_request_that_would_cross_the_ceiling_fails_with_budget_exceeded() {
    let transport = Arc::new(fal_job("fal-ai/flux/dev", png(8, 8, [9, 9, 9, 255])));
    let mut fx = Fixture::new(transport.clone(), fal_keys());

    fx.engine
        .apply("ai.budget.set", json!({"limit-usd": 0.01}), None, false)
        .unwrap();
    let before = fx.snapshot();

    let err = fx
        .engine
        .apply(
            "ai.image.generate",
            json!({"prompt": "expensive", "size": "8x8"}),
            None,
            false,
        )
        .unwrap_err();

    assert_eq!(err.code(), "budget_exceeded");
    assert_eq!(err.exit_code(), 6);
    assert_eq!(
        transport.call_count(),
        0,
        "refused before the request was sent"
    );
    assert_eq!(fx.snapshot(), before);

    // Raising the ceiling lets the same call through.
    fx.engine
        .apply("ai.budget.set", json!({"limit-usd": 1.0}), None, false)
        .unwrap();
    fx.engine
        .apply(
            "ai.image.generate",
            json!({"prompt": "expensive", "size": "8x8"}),
            None,
            false,
        )
        .unwrap();
    assert!(transport.call_count() > 0);
}

#[test]
fn budget_status_accounts_for_spend_by_provider_model_and_op() {
    let transport = Arc::new(fal_job("fal-ai/flux/dev", png(8, 8, [9, 9, 9, 255])));
    let mut fx = Fixture::new(transport.clone(), fal_keys());

    fx.engine
        .apply("ai.budget.set", json!({"limit-usd": 2.0}), None, false)
        .unwrap();
    fx.engine
        .apply(
            "ai.image.generate",
            json!({"prompt": "one", "size": "8x8"}),
            None,
            false,
        )
        .unwrap();

    let data = fx
        .engine
        .apply("ai.budget.status", json!({}), None, false)
        .unwrap()
        .effect
        .data
        .unwrap();

    assert_eq!(data["ceilingUsd"], 2.0);
    assert_eq!(data["spentUsd"], 0.025);
    assert_eq!(data["remainingUsd"], 1.975);
    assert_eq!(data["byProvider"]["fal"], 0.025);
    assert_eq!(data["byModel"]["fal-ai/flux/dev"], 0.025);
    assert_eq!(data["byOp"]["ai.image.generate"], 0.025);
}

#[test]
fn provider_status_names_the_source_and_never_the_key() {
    let transport = Arc::new(dpaint_ai::RecordedTransport::new());
    let mut fx = Fixture::new(transport, fal_keys());

    let data = fx
        .engine
        .apply("ai.provider.status", json!({}), None, false)
        .unwrap()
        .effect
        .data
        .unwrap();

    let providers = data["providers"].as_array().unwrap();
    let fal = providers.iter().find(|p| p["provider"] == "fal").unwrap();
    assert_eq!(fal["configured"], true);
    assert_eq!(fal["source"], "explicit");
    assert_eq!(fal["models"]["inpaint"], "fal-ai/flux-general/inpainting");
    let quiver = providers
        .iter()
        .find(|p| p["provider"] == "quiver")
        .unwrap();
    assert_eq!(quiver["configured"], false);
    assert!(!data.to_string().contains("sk-test"));
}

#[test]
fn a_selector_that_matches_nothing_never_reaches_the_provider() {
    let transport = Arc::new(fal_job(
        "fal-ai/flux-pro/kontext",
        png(8, 8, [0, 0, 0, 255]),
    ));
    let mut fx = Fixture::new(transport.clone(), fal_keys());

    let err = fx
        .engine
        .apply(
            "ai.image.edit",
            json!({"layer": "#lyr_nope", "prompt": "golden hour"}),
            None,
            false,
        )
        .unwrap_err();

    assert_eq!(err.code(), "selector_no_match");
    assert_eq!(err.exit_code(), 3);
    assert_eq!(transport.call_count(), 0);
}
