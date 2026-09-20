//! fal-backed ops, end to end through the real engine with a recorded transport.

mod common;

use common::*;
use dpaint_ai::Method;
use dpaint_core::doc::raster::LayerKind;
use serde_json::json;
use std::sync::Arc;

#[test]
fn generate_submits_polls_to_completion_and_lands_a_layer_with_provenance() {
    let transport = Arc::new(fal_job("fal-ai/flux/dev", png(16, 16, [10, 200, 30, 255])));
    let mut fx = Fixture::new(transport.clone(), fal_keys());

    let applied = fx
        .engine
        .apply(
            "ai.image.generate",
            json!({"prompt": "storm light over a wheat field, 35mm", "size": "16x16", "seed": 7, "name": "sky"}),
            None,
            false,
        )
        .unwrap();

    // --- the wire ---
    let calls = transport.calls();
    assert_eq!(calls[0].url, "https://queue.fal.run/fal-ai/flux/dev");
    assert_eq!(calls[0].header_value("authorization"), Some("Key sk-test"));
    let body = calls[0].json().unwrap();
    assert_eq!(body["prompt"], "storm light over a wheat field, 35mm");
    assert_eq!(body["image_size"], json!({"width": 16, "height": 16}));
    assert_eq!(body["seed"], 7);
    assert_eq!(body["num_images"], 1);
    // Polling stopped at COMPLETED instead of running to the attempt limit.
    assert_eq!(transport.calls_matching("/status").len(), 2);
    assert_eq!(transport.calls_matching("cdn.fal.media").len(), 1);

    // --- the document ---
    let layer = fx.raster().layers.last().unwrap();
    assert_eq!(layer.name, "sky");
    let LayerKind::Pixel { asset, .. } = &layer.kind else {
        panic!("generated layer is {}", layer.type_name());
    };
    assert_eq!(
        fx.assets().get(asset).unwrap(),
        png(16, 16, [10, 200, 30, 255]),
        "the downloaded pixels are in the content-addressed store"
    );

    let p = layer.provenance.as_ref().expect("provenance recorded");
    assert_eq!(p.provider, "fal");
    assert_eq!(p.model, "fal-ai/flux/dev");
    assert_eq!(
        p.prompt.as_deref(),
        Some("storm light over a wheat field, 35mm")
    );
    assert_eq!(p.seed, Some(7));
    assert_eq!(p.request_id.as_deref(), Some("req-777"));
    assert_eq!(p.cost_usd, Some(0.025));
    assert_eq!(applied.effect.cost_usd, Some(0.025));
    assert_eq!(applied.effect.created, vec!["lyr_sky".to_string()]);

    // Nothing anywhere carries the credential.
    assert!(!fx.snapshot().contains("sk-test"));
}

#[test]
fn the_model_id_comes_from_configuration_and_can_be_overridden_per_call() {
    let transport = Arc::new(fal_job("fal-ai/recraft-v3", png(8, 8, [1, 2, 3, 255])));
    let mut fx = Fixture::new(transport.clone(), fal_keys());

    fx.engine
        .apply(
            "ai.image.generate",
            json!({"prompt": "a mark", "size": "8x8", "model": "fal-ai/recraft-v3"}),
            None,
            false,
        )
        .unwrap();

    assert_eq!(
        transport.calls()[0].url,
        "https://queue.fal.run/fal-ai/recraft-v3"
    );
    assert_eq!(
        fx.raster()
            .layers
            .last()
            .unwrap()
            .provenance
            .as_ref()
            .unwrap()
            .model,
        "fal-ai/recraft-v3"
    );
}

#[test]
fn inpaint_sends_the_documents_current_selection_as_the_mask() {
    let transport = Arc::new(fal_job(
        "fal-ai/flux-general/inpainting",
        png(32, 32, [90, 90, 90, 255]),
    ));
    let mut fx = Fixture::new(transport.clone(), fal_keys());

    // Select the top-left quadrant, exactly as a selection op would.
    fx.engine
        .workspace
        .project
        .raster_mut(&dpaint_core::DocId::from("doc_main"))
        .unwrap()
        .selection = Some(dpaint_core::doc::raster::Selection {
        d: Some("M0 0 H16 V16 H0 Z".into()),
        mask: None,
        feather: 0.0,
        inverted: false,
        bounds: dpaint_core::doc::Rect::new(0.0, 0.0, 16.0, 16.0),
    });

    fx.engine
        .apply(
            "ai.image.inpaint",
            json!({"layer": "#lyr_src", "prompt": "add a distant barn"}),
            None,
            false,
        )
        .unwrap();

    let body = transport.calls()[0].json().unwrap();
    let mask_uri = body["mask_url"].as_str().expect("a mask was sent");
    assert!(mask_uri.starts_with("data:image/png;base64,"));

    use base64::Engine as _;
    let mask_png = base64::engine::general_purpose::STANDARD
        .decode(mask_uri.trim_start_matches("data:image/png;base64,"))
        .unwrap();
    let mask = image::load_from_memory(&mask_png).unwrap().to_rgba8();
    assert_eq!(mask.dimensions(), (32, 32), "the mask covers the canvas");
    assert_eq!(
        mask.get_pixel(8, 8).0[0],
        255,
        "the selected quadrant is paintable"
    );
    assert_eq!(mask.get_pixel(24, 24).0[0], 0, "the rest is protected");

    // And the source pixels went along with it.
    assert!(body["image_url"]
        .as_str()
        .unwrap()
        .starts_with("data:image/png;base64,"));
}

#[test]
fn inpaint_without_a_selection_fails_before_any_request_is_made() {
    let transport = Arc::new(fal_job(
        "fal-ai/flux-general/inpainting",
        png(8, 8, [0, 0, 0, 255]),
    ));
    let mut fx = Fixture::new(transport.clone(), fal_keys());
    let before = fx.snapshot();

    let err = fx
        .engine
        .apply(
            "ai.image.inpaint",
            json!({"layer": "#lyr_src", "prompt": "a barn"}),
            None,
            false,
        )
        .unwrap_err();

    assert!(err.to_string().contains("no selection"), "{err}");
    assert_eq!(transport.call_count(), 0, "nothing was billed");
    assert_eq!(fx.snapshot(), before);
}

#[test]
fn remove_background_turns_the_cutout_into_a_layer_mask() {
    let transport = Arc::new(fal_job("fal-ai/birefnet", cutout_png(32, 32)));
    let mut fx = Fixture::new(transport.clone(), fal_keys());

    fx.engine
        .apply(
            "ai.image.remove-background",
            json!({"layer": "#lyr_src"}),
            None,
            false,
        )
        .unwrap();

    let layer = fx
        .raster()
        .layer(&dpaint_core::LayerId::from(SRC_LAYER))
        .unwrap();
    let mask = layer.mask.as_ref().expect("the result became a layer mask");
    assert!(mask.enabled && !mask.inverted);

    // The original pixels are untouched: only a mask was added.
    let LayerKind::Pixel { asset, .. } = &layer.kind else {
        panic!("not a pixel layer")
    };
    assert_eq!(
        fx.assets().get(asset).unwrap(),
        png(32, 32, [20, 60, 120, 255])
    );

    let m = image::load_from_memory(&fx.assets().get(&mask.asset).unwrap())
        .unwrap()
        .to_rgba8();
    assert_eq!(
        m.get_pixel(4, 4).0[0],
        255,
        "subject side is opaque in the mask"
    );
    assert_eq!(m.get_pixel(28, 4).0[0], 0, "cut-away side is masked out");
    assert!(layer.provenance.is_some());
}

#[test]
fn upscale_replaces_the_blob_and_keeps_the_layer_the_same_size_on_canvas() {
    let transport = Arc::new(fal_job(
        "fal-ai/clarity-upscaler",
        png(64, 64, [7, 7, 7, 255]),
    ));
    let mut fx = Fixture::new(transport.clone(), fal_keys());
    let before = match &fx
        .raster()
        .layer(&dpaint_core::LayerId::from(SRC_LAYER))
        .unwrap()
        .kind
    {
        LayerKind::Pixel { asset, .. } => asset.clone(),
        _ => unreachable!(),
    };

    fx.engine
        .apply(
            "ai.image.upscale",
            json!({"layer": "#lyr_src", "factor": 2}),
            None,
            false,
        )
        .unwrap();

    assert_eq!(transport.calls()[0].json().unwrap()["scale"], 2.0);
    let layer = fx
        .raster()
        .layer(&dpaint_core::LayerId::from(SRC_LAYER))
        .unwrap();
    let LayerKind::Pixel { asset, .. } = &layer.kind else {
        panic!("not a pixel layer")
    };
    assert_ne!(asset, &before, "the layer points at the upscaled blob");
    let pm = image::load_from_memory(&fx.assets().get(asset).unwrap()).unwrap();
    assert_eq!((pm.width(), pm.height()), (64, 64));
    // Twice the pixels, half the scale: the layer still covers 32x32 of document space.
    assert_eq!(layer.transform.0[0], 0.5);
}

#[test]
fn outpaint_grows_the_canvas_and_shifts_existing_content() {
    let transport = Arc::new(fal_job(
        "fal-ai/flux-general/inpainting",
        png(48, 32, [5, 5, 5, 255]),
    ));
    let mut fx = Fixture::new(transport.clone(), fal_keys());

    fx.engine
        .apply(
            "ai.image.outpaint",
            json!({"layer": "#lyr_src", "prompt": "more field", "left": 16}),
            None,
            false,
        )
        .unwrap();

    assert_eq!(fx.raster().size, [48, 32], "the canvas grew by the margin");
    let src = fx
        .raster()
        .layer(&dpaint_core::LayerId::from(SRC_LAYER))
        .unwrap();
    assert_eq!(src.transform.0[4], 16.0, "existing content kept its place");

    // The mask protects the original area and opens the new margin.
    use base64::Engine as _;
    let body = transport.calls()[0].json().unwrap();
    let mask_png = base64::engine::general_purpose::STANDARD
        .decode(
            body["mask_url"]
                .as_str()
                .unwrap()
                .trim_start_matches("data:image/png;base64,"),
        )
        .unwrap();
    let mask = image::load_from_memory(&mask_png).unwrap().to_rgba8();
    assert_eq!(mask.dimensions(), (48, 32));
    assert_eq!(mask.get_pixel(4, 16).0[0], 255);
    assert_eq!(mask.get_pixel(32, 16).0[0], 0);
}

#[test]
fn texture_generate_binds_editable_raster_documents_to_the_material() {
    use dpaint_core::doc::model::{Material, ModelDoc, TextureSlot, TextureSource};
    use dpaint_core::doc::Document;

    let transport = Arc::new(fal_job("fal-ai/flux/dev", png(16, 16, [80, 80, 10, 255])));
    let mut fx = Fixture::new(transport.clone(), fal_keys());

    let mut model = ModelDoc::new(dpaint_core::DocId::from("doc_scene"), "scene");
    model.materials.push(Material::new(
        dpaint_core::MaterialId::from("mat_gold"),
        "gold",
    ));
    fx.engine
        .workspace
        .project
        .add_document(Document::Model(model));

    let applied = fx
        .engine
        .apply(
            "ai.texture.generate",
            json!({
                "material": "#mat_gold",
                "prompt": "brushed gold, fine scratches",
                "maps": ["base", "roughness"],
                "size": 16
            }),
            Some("doc_scene".into()),
            false,
        )
        .unwrap();

    // One submitted job per map.
    let submits = transport
        .calls()
        .iter()
        .filter(|c| c.method == Method::Post)
        .count();
    assert_eq!(submits, 2);
    assert_eq!(applied.effect.cost_usd, Some(0.05));

    let material = &fx
        .project()
        .model(&dpaint_core::DocId::from("doc_scene"))
        .unwrap()
        .materials[0];
    assert_eq!(material.textures.len(), 2);
    let base = material
        .textures
        .iter()
        .find(|t| t.slot == TextureSlot::BaseColor)
        .expect("base colour bound");
    let TextureSource::Document { document } = &base.source else {
        panic!("a generated map should be an editable document, not a bare asset");
    };
    let tex = fx.project().raster(document).unwrap();
    assert_eq!(tex.size, [16, 16]);
    assert!(
        tex.layers[0].provenance.is_some(),
        "the map layer records where it came from"
    );
}

#[test]
fn a_dry_run_reports_the_request_without_sending_it() {
    let transport = Arc::new(fal_job("fal-ai/flux/dev", png(8, 8, [0, 0, 0, 255])));
    let mut fx = Fixture::new(transport.clone(), fal_keys());
    let before = fx.snapshot();

    let applied = fx
        .engine
        .apply(
            "ai.image.generate",
            json!({"prompt": "a barn", "size": "8x8"}),
            None,
            true,
        )
        .unwrap();

    let data = applied.effect.data.unwrap();
    assert_eq!(data["dryRun"], true);
    assert_eq!(data["model"], "fal-ai/flux/dev");
    assert_eq!(data["request"]["prompt"], "a barn");
    assert_eq!(data["estimatedCostUsd"], 0.025);
    assert_eq!(transport.call_count(), 0);
    assert_eq!(fx.snapshot(), before);
}

#[test]
fn a_provider_failure_leaves_the_project_untouched() {
    use dpaint_ai::HttpResponse;
    let transport = Arc::new(dpaint_ai::RecordedTransport::new().on(
        Method::Post,
        "queue.fal.run",
        HttpResponse::bytes(429, "text/plain", b"rate limit exceeded".to_vec()),
    ));
    let mut fx = Fixture::new(transport.clone(), fal_keys());
    let before = fx.snapshot();

    let err = fx
        .engine
        .apply(
            "ai.image.generate",
            json!({"prompt": "a barn"}),
            None,
            false,
        )
        .unwrap_err();

    assert_eq!(err.code(), "provider_error");
    assert_eq!(err.exit_code(), 5);
    assert!(err.to_string().contains("rate limit exceeded"), "{err}");
    assert_eq!(fx.snapshot(), before);
    assert_eq!(fx.journal_len(), 0);
}
