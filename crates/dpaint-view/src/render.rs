//! The one draw path.
//!
//! The window and `--frames` both go through here, which is the point: the PNGs the
//! headless mode writes are produced by exactly the code that fills the window, so they
//! are evidence about the viewport rather than about a second renderer that happens to
//! live nearby.

use dpaint_gpu::{CanvasRenderer, Gpu, SceneRenderer, ViewState};

use crate::overlay::Overlay;
use crate::subject::Subject;
use crate::text::TextRaster;

/// Status text size in logical points; multiplied by the display scale factor so the
/// readout is the same physical size on a Retina panel as on a 1x monitor.
pub const STATUS_PT: f32 = 13.0;

/// Inset of the status panel from the top-left corner, in logical points.
pub const STATUS_MARGIN_PT: f32 = 12.0;

pub struct ViewRenderer {
    scene: SceneRenderer,
    canvas: CanvasRenderer,
    overlay: Overlay,
    text: Option<TextRaster>,
    /// The lines currently uploaded, so an unchanged readout costs no rasterization.
    shown: Vec<String>,
    shown_scale: f32,
    has_panel: bool,
}

impl ViewRenderer {
    pub fn new(gpu: &Gpu, format: wgpu::TextureFormat, samples: u32) -> Self {
        Self {
            scene: SceneRenderer::new(gpu, format, samples),
            canvas: CanvasRenderer::new(gpu, format, 1),
            overlay: Overlay::new(gpu.device(), format),
            text: TextRaster::new(),
            shown: Vec::new(),
            shown_scale: 0.0,
            has_panel: false,
        }
    }

    /// Multisample count the scene renderer actually got, which can be lower than the
    /// one asked for.
    pub fn samples(&self) -> u32 {
        self.scene.samples()
    }

    /// How many times mesh geometry has been pushed to the GPU. Orbiting must not move
    /// this: a drag is uniform writes, not a re-upload.
    pub fn geometry_uploads(&self) -> u64 {
        self.scene.geometry_uploads()
    }

    /// How many times the 2D document's pixmap has been uploaded. Panning and zooming
    /// must not move this either — that is the whole point of the canvas path.
    pub fn canvas_uploads(&self) -> u64 {
        self.canvas.uploads()
    }

    /// Hand the renderer a new subject. For a 2D document this is the single texture
    /// upload; for a model it is nothing, because the scene travels with the draw call.
    pub fn set_subject(&mut self, gpu: &Gpu, subject: &Subject) {
        if let Some(pm) = subject.pixmap() {
            self.canvas.upload(gpu, pm);
        }
    }

    /// Replace the status readout. Rasterizing is skipped when the wording and the scale
    /// are unchanged, so a still window re-uploads nothing.
    pub fn set_status(&mut self, gpu: &Gpu, lines: &[String], scale: f32) {
        if self.has_panel && self.shown == lines && (self.shown_scale - scale).abs() < 1e-3 {
            return;
        }
        let Some(text) = &self.text else { return };
        let Some(pm) = text.panel(lines, STATUS_PT * scale) else {
            return;
        };
        self.overlay.set_image(gpu.device(), gpu.queue(), &pm);
        self.shown = lines.to_vec();
        self.shown_scale = scale;
        self.has_panel = true;
    }

    pub fn clear_status(&mut self) {
        self.has_panel = false;
        self.shown.clear();
    }

    /// Draw one frame into `view`. The subject renderer owns the colour target — it
    /// clears and, when multisampled, resolves into `view` — and the status panel is
    /// blended on top in a second single-sample pass.
    pub fn draw(
        &mut self,
        gpu: &Gpu,
        view: &wgpu::TextureView,
        size: [u32; 2],
        subject: &Subject,
        canvas: ViewState,
        scale: f32,
    ) {
        match subject {
            Subject::Model(scene) => self.scene.draw(gpu, view, size, scene),
            Subject::Canvas(_) => self.canvas.draw(gpu, view, size, canvas),
        }
        if self.has_panel {
            self.overlay.draw(
                gpu.device(),
                gpu.queue(),
                view,
                size,
                STATUS_MARGIN_PT * scale,
            );
        }
    }
}
