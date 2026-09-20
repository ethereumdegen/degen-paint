//! Quiver-backed ops. The point of these tests: what lands in the document is editable
//! geometry, not a stored blob.

mod common;

use common::*;
use dpaint_ai::{Method, RecordedTransport};
use dpaint_core::doc::vector::VKind;
use serde_json::json;
use std::sync::Arc;

const CREST: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 100" width="100" height="100">
  <path id="body" d="M10 10 L90 10 L50 90 Z" fill="#fb8500"/>
  <rect id="bar" x="20" y="20" width="60" height="10" fill="#023047"/>
</svg>"##;

const MARK: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 100" width="100" height="100">
  <path id="ring" d="M50 5 A45 45 0 1 1 49 5 Z" fill="#8ecae6"/>
</svg>"##;

fn quiver(endpoint: &str, payload: serde_json::Value) -> Arc<RecordedTransport> {
    Arc::new(RecordedTransport::new().on_json(Method::Post, endpoint, payload))
}

#[test]
fn a_generated_svg_lands_as_editable_vector_objects_not_as_an_image_blob() {
    let transport = quiver(
        "/svgs/generations",
        json!({
            "id": "gen_9",
            "svgs": [{"content": CREST}],
            "usage": {"cost_usd": 0.05}
        }),
    );
    let mut fx = Fixture::new(transport.clone(), quiver_keys());

    let applied = fx
        .engine
        .apply(
            "ai.vector.generate",
            json!({
                "prompt": "heraldic lion crest",
                "instructions": "clean geometry",
                "name": "crest"
            }),
            Some("doc_art".into()),
            false,
        )
        .unwrap();

    // --- the wire ---
    let call = &transport.calls()[0];
    assert_eq!(call.url, "https://api.quiver.ai/v1/svgs/generations");
    assert_eq!(call.header_value("authorization"), Some("Bearer qv-test"));
    let body = call.json().unwrap();
    assert_eq!(body["model"], "arrow-2");
    assert_eq!(body["prompt"], "heraldic lion crest");
    assert_eq!(body["instructions"], "clean geometry");

    // --- the document ---
    let doc = fx.vector();
    let objects = doc.walk();
    assert_eq!(objects.len(), 2, "both SVG shapes became objects: {objects:?}");

    let path_d: Vec<String> = objects
        .iter()
        .filter_map(|o| match &o.kind {
            VKind::Path { d, .. } => Some(d.clone()),
            _ => None,
        })
        .collect();
    assert!(
        path_d.iter().any(|d| d.contains('M') && d.len() > 4),
        "a real path with geometry was imported, got {path_d:?}"
    );

    for o in &objects {
        let p = o.provenance.as_ref().unwrap_or_else(|| panic!("{} has no provenance", o.id));
        assert_eq!(p.provider, "quiver");
        assert_eq!(p.model, "arrow-2");
        assert_eq!(p.prompt.as_deref(), Some("heraldic lion crest"));
        assert_eq!(p.cost_usd, Some(0.05));
    }

    // One artboard was added for the variant, beside the document's existing one.
    assert_eq!(doc.artboards.len(), 2);
    assert!(doc.artboards[1].rect.x() >= 100.0, "variants sit side by side");
    assert!(applied.effect.created.len() >= 3);
    assert_eq!(applied.effect.cost_usd, Some(0.05));

    // Nothing was stashed as an opaque image.
    let json = serde_json::to_string(fx.project()).unwrap();
    assert!(!json.contains(".svg"), "the SVG is not referenced as a blob: {json}");
}

#[test]
fn n_variants_become_one_artboard_each() {
    let transport = quiver(
        "/svgs/generations",
        json!({"id": "gen_10", "svgs": [{"content": CREST}, {"content": MARK}]}),
    );
    let mut fx = Fixture::new(transport.clone(), quiver_keys());

    fx.engine
        .apply(
            "ai.vector.generate",
            json!({"prompt": "a mark", "n": 2, "name": "opt"}),
            Some("doc_art".into()),
            false,
        )
        .unwrap();

    assert_eq!(transport.calls()[0].json().unwrap()["n"], 2);
    let doc = fx.vector();
    assert_eq!(doc.artboards.len(), 3, "one artboard per variant");
    assert_eq!(doc.artboards[1].name, "opt-1");
    assert_eq!(doc.artboards[2].name, "opt-2");
    assert_eq!(doc.walk().len(), 3, "2 objects from the first + 1 from the second");
    // Variants do not overlap.
    assert!(doc.artboards[2].rect.x() >= doc.artboards[1].rect.right());
}

#[test]
fn vectorize_sends_the_raster_layer_and_imports_the_result() {
    let transport = quiver(
        "/svgs/vectorizations",
        json!({"id": "vec_3", "svgs": [{"content": MARK}]}),
    );
    let mut fx = Fixture::new(transport.clone(), quiver_keys());

    fx.engine
        .apply(
            "ai.vector.vectorize",
            json!({"from": "doc_main:#lyr_src", "auto-crop": true, "name": "traced"}),
            Some("doc_art".into()),
            false,
        )
        .unwrap();

    let body = transport.calls()[0].json().unwrap();
    assert_eq!(transport.calls()[0].url, "https://api.quiver.ai/v1/svgs/vectorizations");
    assert_eq!(body["auto_crop"], true);
    assert!(
        body["image"].as_str().unwrap().starts_with("data:image/png;base64,"),
        "the layer's pixels were sent"
    );

    let doc = fx.vector();
    assert_eq!(doc.walk().len(), 1);
    assert!(matches!(doc.walk()[0].kind, VKind::Path { .. }));
    assert_eq!(doc.walk()[0].provenance.as_ref().unwrap().provider, "quiver");
}

#[test]
fn generating_into_a_raster_document_is_a_wrong_document_kind_error() {
    let transport = quiver("/svgs/generations", json!({"svgs": [{"content": MARK}]}));
    let mut fx = Fixture::new(transport.clone(), quiver_keys());

    let err = fx
        .engine
        .apply(
            "ai.vector.generate",
            json!({"prompt": "a mark"}),
            Some("doc_main".into()),
            false,
        )
        .unwrap_err();

    assert_eq!(err.code(), "wrong_document_kind");
    assert_eq!(transport.call_count(), 0);
}

#[test]
fn a_missing_quiver_key_leaves_the_document_alone() {
    let transport = quiver("/svgs/generations", json!({"svgs": [{"content": MARK}]}));
    let mut fx = Fixture::new(transport.clone(), fal_keys());
    let before = fx.snapshot();

    let err = fx
        .engine
        .apply(
            "ai.vector.generate",
            json!({"prompt": "a mark"}),
            Some("doc_art".into()),
            false,
        )
        .unwrap_err();

    assert_eq!(err.code(), "provider_unconfigured");
    assert_eq!(err.exit_code(), 5);
    assert_eq!(transport.call_count(), 0);
    assert_eq!(fx.snapshot(), before);
}
