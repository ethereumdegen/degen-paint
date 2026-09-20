//! The WebGPU viewport, for browsers that have one.
//!
//! This is a *viewport*, not a renderer of record. `render.image`, `render.turntable`,
//! digests and goldens all still go through `dpaint_render` on the CPU, in this crate as
//! everywhere else — [`DpaintEngine::render_png`](crate::DpaintEngine::render_png) is
//! untouched. What lives here is the interactive path: the thing a human drags.
//!
//! The split it buys is the whole point. Today the studio swaps an `<img src="data:…">` on
//! every change, which means a pan costs a full re-render plus a PNG encode plus a base64
//! round trip. Here:
//!
//! * **model documents** upload their meshes once and orbit at the refresh rate — a drag
//!   moves `Scene::camera`, nothing re-enters the engine;
//! * **raster and vector documents** are rasterized once by the engine into a pixmap,
//!   uploaded as a texture, and from then on pan and zoom are two floats in a uniform.
//!
//! Nothing here is required. [`DpaintViewport::create`] resolves to `null` when the browser
//! has no adapter, and the page keeps the image path it has always had.

use dpaint_core::{DocId, Error, Result};
use dpaint_gpu::{
    scene_from_document, Camera, CanvasRenderer, Gpu, Lighting, Scene, SceneRenderer, ViewState,
};
use dpaint_render::RenderOptions;
use wasm_bindgen::prelude::*;
use web_sys::HtmlCanvasElement;

use crate::{to_js, DpaintEngine};

/// What the surface is currently showing. The two renderers are kept apart because they
/// share nothing but the device: one owns a depth buffer and mesh buffers, the other a
/// texture and a quad.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    Empty,
    Canvas,
    Model,
}

impl Mode {
    fn as_str(self) -> &'static str {
        match self {
            Mode::Empty => "empty",
            Mode::Canvas => "canvas",
            Mode::Model => "model",
        }
    }
}

/// A WebGPU surface bound to one `<canvas>`, plus whichever renderer the active document
/// needs.
#[wasm_bindgen]
pub struct DpaintViewport {
    gpu: Gpu,
    samples: u32,
    surface: Option<wgpu::Surface<'static>>,
    format: wgpu::TextureFormat,
    size: [u32; 2],
    mode: Mode,
    scene_renderer: Option<SceneRenderer>,
    canvas_renderer: Option<CanvasRenderer>,
    scene: Option<Scene>,
    /// Size of the uploaded pixmap, in texels. `[0, 0]` for model documents, which have no
    /// intrinsic pixel size. Kept so a caller that missed `set_document`'s return — a
    /// reattach, a reload — can still ask.
    content: [u32; 2],
    view: ViewState,
    /// Screen-pixel pan, kept separately from [`ViewState::pan`] so the model path can
    /// convert it into `Scene::pan`'s fitted-radius units without losing the original.
    pan_px: [f32; 2],
    zoom: f32,
}

#[wasm_bindgen]
impl DpaintViewport {
    /// Resolves to a viewport, or to `null` when this browser exposes no WebGPU adapter.
    ///
    /// Not an error: no adapter is an ordinary outcome, and the caller is expected to carry
    /// on with the CPU image path rather than show a failure.
    pub async fn create() -> Option<DpaintViewport> {
        console_error_panic_hook::set_once();
        let gpu = Gpu::new().await?;
        Some(DpaintViewport {
            gpu,
            // Settled in `attach`, where the surface format is finally known.
            samples: 1,
            surface: None,
            format: wgpu::TextureFormat::Bgra8Unorm,
            size: [1, 1],
            mode: Mode::Empty,
            scene_renderer: None,
            canvas_renderer: None,
            scene: None,
            content: [0, 0],
            view: ViewState {
                zoom: 1.0,
                pan: [0.0, 0.0],
                // The shader owns the transparency checkerboard; the page's CSS one is
                // hidden along with the image it used to frame.
                checker: true,
                pixelated: false,
            },
            pan_px: [0.0, 0.0],
            zoom: 1.0,
        })
    }

    /// Bind the viewport to a canvas and configure its swap chain. Safe to call again with
    /// a different canvas; the old surface is dropped.
    pub fn attach(&mut self, canvas: HtmlCanvasElement) -> std::result::Result<(), JsValue> {
        let width = canvas.width().max(1);
        let height = canvas.height().max(1);
        let surface = self
            .gpu
            .instance()
            .create_surface(wgpu::SurfaceTarget::Canvas(canvas))
            .map_err(|e| {
                to_js(Error::Invalid(format!(
                    "this canvas cannot carry a WebGPU surface: {e}"
                )))
            })?;
        let caps = surface.get_capabilities(self.gpu.adapter());
        // Deliberately *not* an sRGB format. Both renderers write sRGB-encoded values
        // directly, so an `*Srgb` surface would encode a second time and wash the whole
        // viewport out relative to the CPU image path.
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| !f.is_srgb())
            .or_else(|| caps.formats.first().copied())
            .ok_or_else(|| {
                to_js(Error::Invalid(
                    "the surface supports no texture format".into(),
                ))
            })?;
        self.format = format;
        self.samples = if self.gpu.supports_msaa4(format) {
            4
        } else {
            1
        };
        self.size = [width, height];
        self.surface = Some(surface);
        // Renderers are per-format; a re-attach to a differently-configured canvas rebuilds
        // them on next use.
        self.scene_renderer = None;
        self.canvas_renderer = None;
        self.configure();
        Ok(())
    }

    /// Point the viewport at a document. Raster and vector documents are rasterized once by
    /// the engine and uploaded; model documents are turned into a [`Scene`] once. Returns
    /// the content size in texels, `[0, 0]` for a model.
    ///
    /// This is the only call that touches the engine, and it is called when the document
    /// changes — not when the view does.
    pub fn set_document(
        &mut self,
        engine: &DpaintEngine,
        doc: Option<String>,
    ) -> std::result::Result<Vec<u32>, JsValue> {
        let content = self.load(engine, doc.as_deref()).map_err(to_js)?;
        self.content = content;
        Ok(content.to_vec())
    }

    /// Forget the document: the next frame clears to the background.
    pub fn clear_document(&mut self) {
        self.mode = Mode::Empty;
        self.scene = None;
        self.content = [0, 0];
    }

    /// Pan and zoom, in device pixels. `zoom` is device pixels per content pixel for a
    /// canvas document, and a distance multiplier for a model; `pan` is the offset of the
    /// content centre from the viewport centre.
    pub fn set_view(&mut self, zoom: f32, pan_x: f32, pan_y: f32, pixelated: bool) {
        self.zoom = if zoom.is_finite() && zoom > 0.0 {
            zoom
        } else {
            1.0
        };
        self.pan_px = [
            if pan_x.is_finite() { pan_x } else { 0.0 },
            if pan_y.is_finite() { pan_y } else { 0.0 },
        ];
        self.view.zoom = self.zoom;
        self.view.pan = self.pan_px;
        self.view.pixelated = pixelated;
        self.sync_camera();
    }

    /// Drag the camera around the subject. Degrees per device pixel is the caller's to
    /// choose; this takes the delta already scaled. No-op for 2D documents.
    pub fn orbit(&mut self, dx: f32, dy: f32) {
        let Some(scene) = self.scene.as_mut() else {
            return;
        };
        scene.camera.yaw = (scene.camera.yaw + dx).rem_euclid(360.0);
        scene.camera.pitch = (scene.camera.pitch + dy).clamp(-89.0, 89.0);
    }

    /// Absolute camera angles, for a UI that wants to show or restore them.
    pub fn set_orbit(&mut self, yaw: f32, pitch: f32) {
        if let Some(scene) = self.scene.as_mut() {
            scene.camera.yaw = yaw.rem_euclid(360.0);
            scene.camera.pitch = pitch.clamp(-89.0, 89.0);
        }
    }

    /// `[yaw, pitch]` in degrees, or an empty vector when no model is loaded.
    pub fn orbit_angles(&self) -> Vec<f32> {
        match &self.scene {
            Some(s) => vec![s.camera.yaw, s.camera.pitch],
            None => Vec::new(),
        }
    }

    /// Resize the swap chain. The caller owns the canvas's backing-store size; this must be
    /// told about it, in device pixels.
    pub fn resize(&mut self, width: u32, height: u32) {
        let size = [width.max(1), height.max(1)];
        if size == self.size {
            return;
        }
        self.size = size;
        self.sync_camera();
        self.configure();
    }

    /// Draw one frame. Cheap by construction: no engine call, no allocation of mesh or
    /// texture data, just a uniform write and a submit.
    pub fn frame(&mut self) -> std::result::Result<(), JsValue> {
        let Some(surface) = self.surface.as_ref() else {
            return Err(to_js(Error::Invalid(
                "the viewport has no canvas; call attach() first".into(),
            )));
        };
        use wgpu::CurrentSurfaceTexture as Cur;
        let frame = match surface.get_current_texture() {
            Cur::Success(f) | Cur::Suboptimal(f) => f,
            // Lost and Outdated are ordinary across a resize: reconfigure and skip this
            // frame rather than tearing the page down. Occluded and Timeout mean there is
            // nothing worth drawing right now.
            Cur::Lost | Cur::Outdated => {
                self.configure();
                return Ok(());
            }
            Cur::Timeout | Cur::Occluded => return Ok(()),
            other => {
                return Err(to_js(Error::Invalid(format!(
                    "the WebGPU surface failed: {other:?}"
                ))))
            }
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        match self.mode {
            Mode::Model => {
                if let Some(scene) = self.scene.as_ref() {
                    let r = self.scene_renderer.get_or_insert_with(|| {
                        SceneRenderer::new(&self.gpu, self.format, self.samples)
                    });
                    r.draw(&self.gpu, &view, self.size, scene);
                }
            }
            Mode::Canvas => {
                if let Some(r) = self.canvas_renderer.as_mut() {
                    r.draw(&self.gpu, &view, self.size, self.view);
                }
            }
            Mode::Empty => self.clear_to_background(&view),
        }
        self.gpu.queue().present(frame);
        Ok(())
    }

    /// `"canvas"`, `"model"` or `"empty"` — what the last `set_document` produced.
    pub fn mode(&self) -> String {
        self.mode.as_str().to_string()
    }

    /// `[width, height]` of the uploaded pixmap in texels, `[0, 0]` for a model.
    pub fn content_size(&self) -> Vec<u32> {
        self.content.to_vec()
    }

    /// `{ backend, name, deviceType, samples }`, for the status bar. Strings, so the page
    /// can show exactly what it got instead of claiming "GPU".
    pub fn info(&self) -> std::result::Result<JsValue, JsValue> {
        let i = self.gpu.info();
        let v = serde_json::json!({
            "backend": i.backend,
            "name": i.name,
            "deviceType": i.device_type,
            "samples": self.samples,
        });
        crate::to_js_value(&v)
    }
}

impl DpaintViewport {
    fn configure(&mut self) {
        let Some(surface) = self.surface.as_ref() else {
            return;
        };
        let caps = surface.get_capabilities(self.gpu.adapter());
        surface.configure(
            self.gpu.device(),
            &wgpu::SurfaceConfiguration {
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                format: self.format,
                // Plain sRGB: the shaders already encode, so the compositor must not.
                color_space: wgpu::SurfaceColorSpace::Srgb,
                width: self.size[0],
                height: self.size[1],
                present_mode: wgpu::PresentMode::Fifo,
                desired_maximum_frame_latency: 2,
                alpha_mode: caps
                    .alpha_modes
                    .first()
                    .copied()
                    .unwrap_or(wgpu::CompositeAlphaMode::Auto),
                view_formats: vec![],
            },
        );
    }

    /// The 2D pan/zoom the UI speaks is screen pixels. A model's is a camera. Translate
    /// once, here, so `set_view` means the same thing to a user in both modes.
    ///
    /// `preview3d` puts the eye at `radius / tan(yfov/2) * 1.6 * zoom`, so the visible
    /// half-height at the subject is exactly `1.6 * zoom` fitted radii — which makes the
    /// conversion from pixels exact rather than a fudge factor.
    fn sync_camera(&mut self) {
        let Some(scene) = self.scene.as_mut() else {
            return;
        };
        scene.camera.zoom = (1.0 / self.zoom).clamp(0.05, 40.0);
        let per_px = 3.2 * scene.camera.zoom / self.size[1].max(1) as f32;
        scene.pan = [-self.pan_px[0] * per_px, self.pan_px[1] * per_px];
    }

    fn load(&mut self, engine: &DpaintEngine, doc: Option<&str>) -> Result<[u32; 2]> {
        let ws = engine.workspace()?;
        let id = ws.project.resolve_doc(doc)?;
        if matches!(ws.project.doc(&id)?, dpaint_core::Document::Model(_)) {
            let scene = scene_from_document(
                &ws.project,
                &id,
                &engine.assets(),
                Camera::default(),
                Lighting::default(),
            )?;
            // Keep the angles the user had dragged to; only the geometry is new.
            let keep = self.scene.as_ref().map(|s| (s.camera.yaw, s.camera.pitch));
            self.scene = Some(scene);
            if let (Some((yaw, pitch)), Some(s)) = (keep, self.scene.as_mut()) {
                s.camera.yaw = yaw;
                s.camera.pitch = pitch;
            }
            self.sync_camera();
            self.mode = Mode::Model;
            self.scene_renderer
                .get_or_insert_with(|| SceneRenderer::new(&self.gpu, self.format, self.samples));
            return Ok([0, 0]);
        }
        let pixmap = self.rasterize(engine, &ws.project, &id)?;
        let size = [pixmap.width(), pixmap.height()];
        let r = self
            .canvas_renderer
            .get_or_insert_with(|| CanvasRenderer::new(&self.gpu, self.format, self.samples));
        r.upload(&self.gpu, &pixmap);
        self.mode = Mode::Canvas;
        Ok(size)
    }

    /// One CPU rasterization per document change — the same `render_document` the image path
    /// uses, so what the GPU shows is what `dpaint render` would write. Clamped to the
    /// device's texture limit rather than assuming 8192.
    fn rasterize(
        &self,
        engine: &DpaintEngine,
        project: &dpaint_core::Project,
        id: &DocId,
    ) -> Result<tiny_skia::Pixmap> {
        let limit = self.gpu.device().limits().max_texture_dimension_2d;
        let opts = match project.doc(id)?.size() {
            Some((w, h)) if w.max(h) > limit as f64 => {
                let k = limit as f64 / w.max(h);
                RenderOptions {
                    size: Some((
                        (w * k).round().clamp(1.0, limit as f64) as u32,
                        (h * k).round().clamp(1.0, limit as f64) as u32,
                    )),
                    ..Default::default()
                }
            }
            _ => RenderOptions::default(),
        };
        dpaint_render::render_document(project, id, &engine.assets(), &opts)
    }

    fn clear_to_background(&self, view: &wgpu::TextureView) {
        let mut enc = self
            .gpu
            .device()
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("dpaint-viewport-clear"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: 0.039,
                        g: 0.047,
                        b: 0.059,
                        a: 1.0,
                    }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        self.gpu.queue().submit(Some(enc.finish()));
    }
}
