//! Fixtures shared by the provider tests: a real project on disk, a runtime wired to a
//! recorded transport, and small images to send back.

#![allow(dead_code)]

use dpaint_ai::{AiConfig, RecordedTransport, Runtime, StaticKeys};
use dpaint_core::doc::raster::{Layer, LayerKind};
use dpaint_core::doc::Document;
use dpaint_core::{
    AssetStore, DocId, Engine, LayerId, Project, RasterDoc, Registry, VectorDoc, Workspace,
};
use std::sync::Arc;
use tempfile::TempDir;

pub const SRC_LAYER: &str = "lyr_src";

/// A solid PNG of the given size and colour.
pub fn png(w: u32, h: u32, rgba: [u8; 4]) -> Vec<u8> {
    let mut pm = tiny_skia::Pixmap::new(w, h).unwrap();
    pm.fill(tiny_skia::Color::from_rgba8(rgba[0], rgba[1], rgba[2], rgba[3]));
    pm.encode_png().unwrap()
}

/// A PNG whose left half is opaque and whose right half is transparent — a cutout.
pub fn cutout_png(w: u32, h: u32) -> Vec<u8> {
    let mut pm = tiny_skia::Pixmap::new(w, h).unwrap();
    let width = pm.width();
    for (i, px) in pm.pixels_mut().iter_mut().enumerate() {
        let x = i as u32 % width;
        *px = if x < width / 2 {
            tiny_skia::PremultipliedColorU8::from_rgba(200, 40, 40, 255).unwrap()
        } else {
            tiny_skia::PremultipliedColorU8::from_rgba(0, 0, 0, 0).unwrap()
        };
    }
    pm.encode_png().unwrap()
}

pub fn config() -> AiConfig {
    // No sleeping between polls: the tests assert the poll sequence, not the clock.
    AiConfig { poll_interval_ms: 0, ..AiConfig::default() }
}

pub fn runtime(transport: Arc<RecordedTransport>, keys: StaticKeys) -> Runtime {
    Runtime::new(transport, Arc::new(keys), Arc::new(config()))
}

pub struct Fixture {
    pub dir: TempDir,
    pub engine: Engine,
    pub transport: Arc<RecordedTransport>,
}

impl Fixture {
    /// A project with a 32x32 raster document holding one pixel layer, plus an empty vector
    /// document, opened through the real engine so ops go through validation, journalling
    /// and atomic save exactly as they do in production.
    pub fn new(transport: Arc<RecordedTransport>, keys: StaticKeys) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let raster = RasterDoc::new(DocId::from("doc_main"), "main", 32, 32);
        let mut project = Project::new("test", Document::Raster(raster));
        project.add_document(Document::Vector(VectorDoc::new(
            DocId::from("doc_art"),
            "art",
            100.0,
            100.0,
        )));

        let mut ws = Workspace::create(dir.path(), project).unwrap();
        let asset = ws.assets.put(&png(32, 32, [20, 60, 120, 255]), "png").unwrap();
        ws.project
            .raster_mut(&DocId::from("doc_main"))
            .unwrap()
            .layers
            .push(Layer::new(
                LayerId::from(SRC_LAYER),
                "src",
                LayerKind::Pixel { asset, offset: [0, 0] },
            ));
        ws.save().unwrap();

        let mut registry = Registry::new();
        registry.extend(dpaint_ai::ops_with(runtime(transport.clone(), keys)));
        Self { dir, engine: Engine::new(registry, ws), transport }
    }

    pub fn assets(&self) -> &AssetStore {
        &self.engine.workspace.assets
    }

    pub fn project(&self) -> &Project {
        &self.engine.workspace.project
    }

    pub fn raster(&self) -> &RasterDoc {
        self.project().raster(&DocId::from("doc_main")).unwrap()
    }

    pub fn vector(&self) -> &VectorDoc {
        self.project().vector(&DocId::from("doc_art")).unwrap()
    }

    /// The serialized project, for "nothing was mutated" assertions.
    pub fn snapshot(&self) -> String {
        std::fs::read_to_string(self.dir.path().join("project.json")).unwrap()
    }

    pub fn journal_len(&self) -> usize {
        std::fs::read_to_string(self.dir.path().join("history.jsonl"))
            .map(|t| t.lines().filter(|l| !l.trim().is_empty()).count())
            .unwrap_or(0)
    }
}

pub fn fal_keys() -> StaticKeys {
    StaticKeys::new().with(dpaint_ai::Provider::Fal, "sk-test")
}

pub fn quiver_keys() -> StaticKeys {
    StaticKeys::new().with(dpaint_ai::Provider::Quiver, "qv-test")
}

pub fn all_keys() -> StaticKeys {
    StaticKeys::new()
        .with(dpaint_ai::Provider::Fal, "sk-test")
        .with(dpaint_ai::Provider::Quiver, "qv-test")
}

/// A transport that plays a complete fal job: submit, one in-progress poll, completion, and
/// the image download.
pub fn fal_job(model: &str, image: Vec<u8>) -> RecordedTransport {
    use dpaint_ai::Method;
    use serde_json::json;
    let base = format!("https://queue.fal.run/{model}");
    RecordedTransport::new()
        .on_json(
            Method::Post,
            &base,
            json!({
                "request_id": "req-777",
                "status_url": format!("{base}/requests/req-777/status"),
                "response_url": format!("{base}/requests/req-777"),
            }),
        )
        .on_json(Method::Get, "/requests/req-777/status", json!({"status": "IN_PROGRESS"}))
        .on_json(Method::Get, "/requests/req-777/status", json!({"status": "COMPLETED"}))
        .on_json(
            Method::Get,
            &format!("{base}/requests/req-777"),
            json!({"images": [{"url": "https://cdn.fal.media/out.png", "content_type": "image/png"}]}),
        )
        .on_bytes("https://cdn.fal.media/out.png", "image/png", image)
}
