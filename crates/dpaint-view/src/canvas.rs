//! 2D viewport state for raster and vector documents.
//!
//! The engine rasterizes the document once; after that, panning and zooming never touch
//! the pixels again — they are two numbers handed to `dpaint_gpu::CanvasRenderer` as a
//! [`ViewState`]. That is the whole reason a 6000x6000 raster document can be dragged
//! around at display refresh rate.

use dpaint_gpu::ViewState;

/// Surface pixels per document pixel. The far end is "one document pixel fills a 64 px
/// block", which is where inspecting individual pixels stops being useful; the near end
/// keeps a 16k-wide document from vanishing into a dot.
pub const MIN_ZOOM: f32 = 0.02;
pub const MAX_ZOOM: f32 = 64.0;

/// Zoom multiplier per wheel notch.
pub const ZOOM_STEP: f32 = 1.15;

/// Fitting leaves a small margin so the document edge is visible against the checker.
pub const FIT_MARGIN: f32 = 0.96;

/// Pan/zoom for a 2D document.
///
/// `pan` is the offset, in surface pixels, of the document's centre from the viewport's
/// centre: `[0.0, 0.0]` is centred, positive x moves the document right.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CanvasView {
    pub zoom: f32,
    pub pan: [f32; 2],
    pub checker: bool,
    pub pixelated: bool,
}

impl Default for CanvasView {
    fn default() -> Self {
        Self {
            zoom: 1.0,
            pan: [0.0, 0.0],
            checker: true,
            pixelated: false,
        }
    }
}

impl CanvasView {
    /// What the renderer consumes.
    pub fn view_state(&self) -> ViewState {
        ViewState {
            zoom: self.zoom,
            pan: self.pan,
            checker: self.checker,
            pixelated: self.pixelated,
        }
    }

    /// `f`: fit the whole document in the window, centred.
    pub fn fit(&mut self, content: [u32; 2], viewport: [u32; 2]) {
        self.pan = [0.0, 0.0];
        if content[0] == 0 || content[1] == 0 || viewport[0] == 0 || viewport[1] == 0 {
            self.zoom = 1.0;
            return;
        }
        let sx = viewport[0] as f32 / content[0] as f32;
        let sy = viewport[1] as f32 / content[1] as f32;
        self.zoom = (sx.min(sy) * FIT_MARGIN).clamp(MIN_ZOOM, MAX_ZOOM);
    }

    /// `1`: one document pixel per surface pixel, centred. On a 2x display that is one
    /// document pixel per *physical* pixel, which is the honest "100%".
    pub fn reset(&mut self) {
        self.zoom = 1.0;
        self.pan = [0.0, 0.0];
    }

    pub fn pan_by(&mut self, dx: f32, dy: f32) {
        self.pan[0] += dx;
        self.pan[1] += dy;
    }

    /// Wheel, anchored on the cursor: the document point under the pointer stays under
    /// the pointer, which is the difference between a usable zoom and a maddening one.
    /// `cursor` and `viewport` are both in surface pixels.
    pub fn zoom_at(&mut self, cursor: [f32; 2], viewport: [u32; 2], steps: f32) {
        let before = self.zoom;
        let after = (before * ZOOM_STEP.powf(steps)).clamp(MIN_ZOOM, MAX_ZOOM);
        if after == before {
            return;
        }
        let ratio = after / before;
        let centre = [viewport[0] as f32 * 0.5, viewport[1] as f32 * 0.5];
        // The document point under the cursor sits at (cursor - centre - pan) / zoom;
        // hold it fixed and solve for the new pan.
        for i in 0..2 {
            let from_centre = cursor[i] - centre[i];
            self.pan[i] = from_centre - (from_centre - self.pan[i]) * ratio;
        }
        self.zoom = after;
    }

    /// Zoom as the status bar shows it.
    pub fn percent(&self) -> f32 {
        self.zoom * 100.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_scales_the_long_axis_and_centres_the_document() {
        let mut v = CanvasView {
            pan: [120.0, -40.0],
            ..Default::default()
        };
        v.fit([2000, 1000], [800, 800]);
        assert_eq!(v.pan, [0.0, 0.0], "fitting recentres");
        assert!(
            (v.zoom - 0.4 * FIT_MARGIN).abs() < 1e-6,
            "the wide axis decides: {}",
            v.zoom
        );
        // The fitted document must actually be inside the window.
        assert!(2000.0 * v.zoom <= 800.0);
        assert!(1000.0 * v.zoom <= 800.0);
    }

    #[test]
    fn fit_enlarges_a_document_smaller_than_the_window() {
        let mut v = CanvasView::default();
        v.fit([100, 100], [800, 600]);
        assert!(
            v.zoom > 1.0,
            "a 100 px document should fill more of a 800x600 window"
        );
        assert!(100.0 * v.zoom <= 600.0);
    }

    #[test]
    fn fit_of_a_degenerate_document_does_not_divide_by_zero() {
        let mut v = CanvasView::default();
        v.fit([0, 0], [800, 600]);
        assert_eq!(v.zoom, 1.0);
        v.fit([100, 100], [0, 0]);
        assert_eq!(v.zoom, 1.0);
    }

    #[test]
    fn zoom_clamps_at_both_ends() {
        let mut v = CanvasView::default();
        for _ in 0..500 {
            v.zoom_at([400.0, 300.0], [800, 600], 1.0);
        }
        assert_eq!(v.zoom, MAX_ZOOM);
        for _ in 0..1000 {
            v.zoom_at([400.0, 300.0], [800, 600], -1.0);
        }
        assert_eq!(v.zoom, MIN_ZOOM);
    }

    #[test]
    fn zooming_holds_the_document_point_under_the_cursor() {
        let mut v = CanvasView::default();
        let viewport = [800u32, 600u32];
        let cursor = [700.0f32, 120.0f32];
        let centre = [400.0f32, 300.0f32];

        let doc_point = |v: &CanvasView| {
            [
                (cursor[0] - centre[0] - v.pan[0]) / v.zoom,
                (cursor[1] - centre[1] - v.pan[1]) / v.zoom,
            ]
        };
        let before = doc_point(&v);
        for _ in 0..7 {
            v.zoom_at(cursor, viewport, 1.0);
        }
        let after = doc_point(&v);
        assert!(
            (before[0] - after[0]).abs() < 1e-3 && (before[1] - after[1]).abs() < 1e-3,
            "the pixel under the cursor moved: {before:?} -> {after:?}"
        );
        assert!(v.zoom > 1.0);
    }

    #[test]
    fn zooming_on_the_centre_never_moves_the_pan() {
        let mut v = CanvasView::default();
        v.zoom_at([400.0, 300.0], [800, 600], 3.0);
        assert!(
            v.pan[0].abs() < 1e-5 && v.pan[1].abs() < 1e-5,
            "{:?}",
            v.pan
        );
    }

    #[test]
    fn a_clamped_zoom_leaves_the_pan_alone() {
        let mut v = CanvasView {
            zoom: MAX_ZOOM,
            pan: [33.0, -12.0],
            ..Default::default()
        };
        v.zoom_at([700.0, 100.0], [800, 600], 5.0);
        assert_eq!(v.pan, [33.0, -12.0]);
        assert_eq!(v.zoom, MAX_ZOOM);
    }
}
