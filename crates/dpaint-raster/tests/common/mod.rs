//! Shared fixture: a one-document project with a real asset store on disk.
//!
//! Each test binary uses a different slice of this module, so unused helpers are expected.
#![allow(dead_code)]

use dpaint_core::doc::raster::{Layer, LayerKind};
use dpaint_core::{AssetStore, Document, LayerId, OpCx, OpEffect, Project, RasterDoc, Registry};
use dpaint_raster::Canvas;

pub struct Fixture {
    /// Held so the asset store's directory outlives the test.
    pub dir: tempfile::TempDir,
    pub project: Project,
    pub assets: AssetStore,
    pub registry: Registry,
}

pub const DOC: &str = "doc_main";

pub fn fixture(w: u32, h: u32) -> Fixture {
    let dir = tempfile::tempdir().expect("tempdir");
    let assets = AssetStore::new(dir.path());
    let doc = RasterDoc::new(DOC.into(), "main", w, h);
    let project = Project::new("test", Document::Raster(doc));
    let mut registry = Registry::new();
    registry.extend(dpaint_raster::ops());
    Fixture {
        dir,
        project,
        assets,
        registry,
    }
}

impl Fixture {
    pub fn doc(&self) -> &RasterDoc {
        self.project.raster(&DOC.into()).expect("raster doc")
    }

    pub fn doc_mut(&mut self) -> &mut RasterDoc {
        self.project.raster_mut(&DOC.into()).expect("raster doc")
    }

    /// Add a pixel layer whose content comes from `f(x, y) -> straight linear RGBA`.
    pub fn pixel_layer(&mut self, id: &str, f: impl Fn(u32, u32) -> [f32; 4]) -> LayerId {
        let (w, h) = (self.doc().width(), self.doc().height());
        let mut c = Canvas::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let i = c.idx(x, y);
                c.set_straight(i, f(x, y));
            }
        }
        self.pixel_layer_from(id, &c)
    }

    pub fn pixel_layer_from(&mut self, id: &str, c: &Canvas) -> LayerId {
        let asset = self
            .assets
            .put(&c.to_png().expect("encode"), "png")
            .expect("put");
        let lid = LayerId::from(id);
        let layer = Layer::new(
            lid.clone(),
            id,
            LayerKind::Pixel {
                asset,
                offset: [0, 0],
            },
        );
        self.doc_mut().layers.push(layer);
        lid
    }

    pub fn push_layer(&mut self, layer: Layer) -> LayerId {
        let id = layer.id.clone();
        self.doc_mut().layers.push(layer);
        id
    }

    /// Apply an op through the registry, exactly as the CLI and MCP server do.
    pub fn run(&mut self, id: &str, args: serde_json::Value) -> dpaint_core::Result<OpEffect> {
        let op = self.registry.get(id).expect("op is registered");
        let mut cx = OpCx::new(&self.assets).with_doc(Some(DOC.to_string()));
        op.apply(&mut self.project, args, &mut cx)
    }

    pub fn ok(&mut self, id: &str, args: serde_json::Value) -> OpEffect {
        match self.run(id, args) {
            Ok(e) => e,
            Err(e) => panic!("{id} failed: {e}"),
        }
    }

    pub fn render(&self, scale: f64) -> tiny_skia::Pixmap {
        let link = dpaint_raster::raster_only_link(&self.project, &self.assets);
        dpaint_raster::render_doc(&self.project, &DOC.into(), &self.assets, scale, &link)
            .expect("render")
    }

    pub fn canvas(&self, scale: f64) -> Canvas {
        let link = dpaint_raster::raster_only_link(&self.project, &self.assets);
        dpaint_raster::render_canvas(&self.project, &DOC.into(), &self.assets, scale, &link)
            .expect("render")
    }

    /// A layer's stored pixels, decoded.
    pub fn layer_pixels(&self, id: &LayerId) -> Canvas {
        let layer = self.doc().layer(id).expect("layer exists");
        let LayerKind::Pixel { asset, .. } = &layer.kind else {
            panic!("not a pixel layer")
        };
        Canvas::from_png(&self.assets.get(asset).expect("blob")).expect("decode")
    }

    pub fn layer_asset(&self, id: &LayerId) -> dpaint_core::AssetRef {
        let layer = self.doc().layer(id).expect("layer exists");
        let LayerKind::Pixel { asset, .. } = &layer.kind else {
            panic!("not a pixel layer")
        };
        asset.clone()
    }
}

/// Straight (non-premultiplied) linear RGBA of a rendered pixmap pixel.
pub fn at(pm: &tiny_skia::Pixmap, x: u32, y: u32) -> [f32; 4] {
    let p = pm.pixel(x, y).expect("in bounds").demultiply();
    [
        dpaint_core::color::srgb_to_linear(p.red() as f32 / 255.0),
        dpaint_core::color::srgb_to_linear(p.green() as f32 / 255.0),
        dpaint_core::color::srgb_to_linear(p.blue() as f32 / 255.0),
        p.alpha() as f32 / 255.0,
    ]
}

/// Display-encoded bytes of a rendered pixel, for "did this get darker" style assertions.
pub fn bytes_at(pm: &tiny_skia::Pixmap, x: u32, y: u32) -> [u8; 4] {
    let p = pm.pixel(x, y).expect("in bounds").demultiply();
    [p.red(), p.green(), p.blue(), p.alpha()]
}

pub fn gray(v: f32) -> [f32; 4] {
    [v, v, v, 1.0]
}
