//! A document turned into something drawable.
//!
//! This is the seam between the project and the viewport. Model documents become a
//! `dpaint_gpu::Scene` — geometry the GPU can re-shade every frame. Raster and vector
//! documents become one pixmap, rasterized by the engine exactly once: after that,
//! panning and zooming are GPU state and the CPU is idle.

use dpaint_core::{DocId, DocKind, Error, Result, Workspace};
use dpaint_gpu::{Camera, Lighting, Scene};
use dpaint_render::{natural_size, render_document, RenderOptions};
use tiny_skia::Pixmap;

use crate::input::Mode;

/// Backdrop painted behind a model document in the viewport. Neutral and dark, so a
/// light silhouette and a dark one are both readable against it.
pub const VIEWPORT_BACKDROP: dpaint_core::Color = dpaint_core::Color {
    r: 0.13,
    g: 0.14,
    b: 0.17,
    a: 1.0,
};

/// What the window is showing.
pub enum Subject {
    /// A model document: geometry, lights and a camera the GPU re-renders per frame.
    Model(Scene),
    /// A raster or vector document, rasterized once by `dpaint_render`.
    Canvas(Pixmap),
}

impl Subject {
    pub fn mode(&self) -> Mode {
        match self {
            Subject::Model(_) => Mode::Model,
            Subject::Canvas(_) => Mode::Canvas,
        }
    }

    /// Pixel extent of a 2D document; `None` for a model, which has no intrinsic size.
    pub fn content_size(&self) -> Option<[u32; 2]> {
        match self {
            Subject::Model(_) => None,
            Subject::Canvas(pm) => Some([pm.width(), pm.height()]),
        }
    }

    pub fn scene_mut(&mut self) -> Option<&mut Scene> {
        match self {
            Subject::Model(s) => Some(s),
            Subject::Canvas(_) => None,
        }
    }

    pub fn pixmap(&self) -> Option<&Pixmap> {
        match self {
            Subject::Model(_) => None,
            Subject::Canvas(pm) => Some(pm),
        }
    }
}

/// The scale a 2D document is rasterized at so its pixmap fits in one GPU texture.
///
/// Documents larger than the device's texture limit are rasterized down rather than
/// refused: a 16k-wide poster is still worth looking at, just not texel-for-texel.
pub fn upload_scale(content: (f64, f64), max_texture: u32) -> f64 {
    let longest = content.0.max(content.1);
    let limit = max_texture.max(1) as f64;
    if longest <= limit || longest <= 0.0 {
        1.0
    } else {
        limit / longest
    }
}

/// Build the drawable for a document. `max_texture` is the device's
/// `max_texture_dimension_2d`.
pub fn load(
    ws: &Workspace,
    doc: &DocId,
    camera: Camera,
    lighting: Lighting,
    max_texture: u32,
) -> Result<Subject> {
    let document = ws.project.doc(doc)?;
    match document.kind() {
        DocKind::Model => {
            let mut scene =
                dpaint_gpu::scene_from_document(&ws.project, doc, &ws.assets, camera, lighting)?;
            // A model document has no background of its own, and a transparent viewport
            // is a window onto nothing. `dpaint render` keeps the alpha, because a
            // turntable frame is an asset; a window wants a backdrop to judge the
            // silhouette against.
            if scene.background.is_none() {
                scene.background = Some(VIEWPORT_BACKDROP);
            }
            Ok(Subject::Model(scene))
        }
        DocKind::Raster | DocKind::Vector => {
            let mut opts = RenderOptions::default();
            let (w, h) = natural_size(document, &opts);
            opts.scale = upload_scale((w as f64, h as f64), max_texture);
            let pm = render_document(&ws.project, doc, &ws.assets, &opts)?;
            if pm.width() == 0 || pm.height() == 0 {
                return Err(Error::Invalid(format!(
                    "document {doc} rasterized to nothing"
                )));
            }
            Ok(Subject::Canvas(pm))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_document_within_the_texture_limit_is_uploaded_at_full_resolution() {
        assert_eq!(upload_scale((1920.0, 1080.0), 8192), 1.0);
        assert_eq!(upload_scale((8192.0, 100.0), 8192), 1.0);
    }

    #[test]
    fn an_oversized_document_is_scaled_to_exactly_fit_the_texture_limit() {
        let s = upload_scale((16384.0, 4096.0), 8192);
        assert!((s - 0.5).abs() < 1e-9, "{s}");
        assert!((16384.0 * s).round() as u32 <= 8192);

        let tall = upload_scale((1000.0, 20000.0), 8192);
        assert!((20000.0 * tall).round() as u32 <= 8192);
    }

    #[test]
    fn a_degenerate_size_or_limit_does_not_produce_a_zero_or_infinite_scale() {
        assert_eq!(upload_scale((0.0, 0.0), 8192), 1.0);
        let s = upload_scale((4000.0, 4000.0), 0);
        assert!(s > 0.0 && s.is_finite(), "{s}");
    }
}
