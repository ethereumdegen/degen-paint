//! Orbit controls, as a pure reduction over drag deltas.
//!
//! The state is deliberately the same four numbers `dpaint_render::preview3d::Camera`
//! carries — yaw, pitch, zoom, field of view — plus the pan offset that lives on
//! `dpaint_gpu::Scene`. Nothing here knows about windows or events, which is what makes
//! "drag 90 px right and the yaw moves 36°" a thing a test can state.

use dpaint_render::preview3d::Camera;

/// Degrees of orbit per physical pixel of drag. 0.4 puts a full turn a little under
/// a thousand pixels away, which is a comfortable wrist's worth of movement.
pub const ORBIT_DEG_PER_PX: f32 = 0.4;

/// Pitch stops just short of the poles: at exactly ±90° the view basis
/// (`cross(forward, up)`) degenerates and the image flips.
pub const PITCH_LIMIT: f32 = 89.0;

/// `zoom` multiplies the fitted camera distance, so *smaller is closer*. The near end
/// matches `preview3d`'s own `zoom.max(0.05)` clamp: past it the subject is inside the
/// near plane and there is nothing to see.
pub const MIN_ZOOM: f32 = 0.05;
pub const MAX_ZOOM: f32 = 20.0;

/// Distance multiplier per wheel notch.
pub const ZOOM_STEP: f32 = 1.12;

/// Orbit camera state: a `preview3d::Camera` plus the pan the GPU scene applies to the
/// camera target.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OrbitCamera {
    /// Azimuth in degrees, wrapped to `[0, 360)`.
    pub yaw: f32,
    /// Elevation in degrees, clamped to ±[`PITCH_LIMIT`].
    pub pitch: f32,
    /// Multiplier on the fitted distance: 1.0 is "framed", smaller is closer.
    pub zoom: f32,
    /// Camera-target offset along camera right/up, in units of the fitted radius.
    pub pan: [f32; 2],
    /// Vertical field of view in radians.
    pub yfov: f32,
}

impl Default for OrbitCamera {
    fn default() -> Self {
        let c = Camera::default();
        Self {
            yaw: c.yaw,
            pitch: c.pitch,
            zoom: c.zoom,
            pan: [0.0, 0.0],
            yfov: c.yfov,
        }
    }
}

impl OrbitCamera {
    /// The camera `dpaint_gpu` and `dpaint_render` both take. Pan is not part of it —
    /// it rides on the scene, because `preview3d::Camera` must not grow a field the CPU
    /// renderer never reads.
    pub fn camera(&self) -> Camera {
        Camera {
            yaw: self.yaw,
            pitch: self.pitch,
            zoom: self.zoom,
            yfov: self.yfov,
        }
    }

    /// Left-drag. The subject follows the cursor: drag right and it spins right, which
    /// means the eye travels left, so both axes subtract.
    pub fn orbit(&mut self, dx: f32, dy: f32) {
        self.yaw = wrap_degrees(self.yaw - dx * ORBIT_DEG_PER_PX);
        self.pitch = (self.pitch - dy * ORBIT_DEG_PER_PX).clamp(-PITCH_LIMIT, PITCH_LIMIT);
    }

    /// Right-drag or shift-drag. `height` is the viewport height in the same physical
    /// pixels as the deltas.
    ///
    /// At the target plane the visible height is `2 * dist * tan(yfov/2)`, and
    /// `dist = radius / tan(yfov/2) * 1.6 * zoom`, so the field of view cancels and one
    /// pixel is always `3.2 * zoom / height` radii. Panning therefore tracks the cursor
    /// exactly, at any zoom and any field of view.
    pub fn pan_by(&mut self, dx: f32, dy: f32, height: f32) {
        if height <= 0.0 {
            return;
        }
        let per_px = 3.2 * self.zoom.max(MIN_ZOOM) / height;
        // Positive pan moves the *target*, so the subject moves the other way; negating
        // x and keeping y makes the subject travel with the cursor on both axes.
        self.pan[0] -= dx * per_px;
        self.pan[1] += dy * per_px;
    }

    /// Wheel. Positive `steps` is a scroll away from the user, which zooms in.
    pub fn zoom_by(&mut self, steps: f32) {
        self.zoom = (self.zoom * ZOOM_STEP.powf(-steps)).clamp(MIN_ZOOM, MAX_ZOOM);
    }

    /// `f`: frame the subject. The fit is derived from the scene's bounds, so framing is
    /// exactly "drop the pan and go back to the fitted distance" — the angle you chose is
    /// yours to keep.
    pub fn frame(&mut self) {
        self.zoom = 1.0;
        self.pan = [0.0, 0.0];
    }

    /// `1`: back to the default three-quarter view, the one turntables and
    /// `dpaint render` start from.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// True when the view is what [`frame`](Self::frame) would produce.
    pub fn is_framed(&self) -> bool {
        (self.zoom - 1.0).abs() < 1e-4 && self.pan[0].abs() < 1e-4 && self.pan[1].abs() < 1e-4
    }
}

fn wrap_degrees(d: f32) -> f32 {
    let r = d % 360.0;
    if r < 0.0 {
        r + 360.0
    } else {
        r
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_orbit_drag_turns_the_camera_by_the_documented_rate() {
        let mut c = OrbitCamera::default();
        let (yaw0, pitch0) = (c.yaw, c.pitch);
        c.orbit(90.0, 30.0);
        assert!(
            (c.yaw - wrap_degrees(yaw0 - 36.0)).abs() < 1e-4,
            "90 px right is 36 degrees of yaw, got {} from {yaw0}",
            c.yaw
        );
        assert!(
            (c.pitch - (pitch0 - 12.0)).abs() < 1e-4,
            "30 px down is 12 degrees of pitch, got {}",
            c.pitch
        );
    }

    #[test]
    fn orbiting_is_incremental_and_the_yaw_wraps_instead_of_running_away() {
        let mut c = OrbitCamera::default();
        for _ in 0..40 {
            c.orbit(-100.0, 0.0);
        }
        assert!(
            (0.0..360.0).contains(&c.yaw),
            "yaw must stay wrapped, got {}",
            c.yaw
        );
        // 40 * 100 px * 0.4 deg = 1600 deg = 4 turns + 160 deg, added to the 35 deg start.
        assert!(
            (c.yaw - wrap_degrees(35.0 + 1600.0)).abs() < 1e-2,
            "{}",
            c.yaw
        );
    }

    #[test]
    fn pitch_stops_short_of_the_poles_in_both_directions() {
        let mut c = OrbitCamera::default();
        c.orbit(0.0, -10_000.0);
        assert_eq!(c.pitch, PITCH_LIMIT);
        c.orbit(0.0, 10_000.0);
        assert_eq!(c.pitch, -PITCH_LIMIT);
    }

    #[test]
    fn zoom_clamps_at_both_ends_however_long_you_scroll() {
        let mut c = OrbitCamera::default();
        for _ in 0..200 {
            c.zoom_by(1.0);
        }
        assert_eq!(c.zoom, MIN_ZOOM, "scrolling in must stop at the near clamp");
        for _ in 0..400 {
            c.zoom_by(-1.0);
        }
        assert_eq!(c.zoom, MAX_ZOOM, "scrolling out must stop at the far clamp");
    }

    #[test]
    fn one_wheel_notch_in_then_out_returns_to_where_it_started() {
        let mut c = OrbitCamera::default();
        c.zoom_by(1.0);
        assert!(c.zoom < 1.0, "scrolling up moves the eye closer");
        c.zoom_by(-1.0);
        assert!((c.zoom - 1.0).abs() < 1e-5, "{}", c.zoom);
    }

    #[test]
    fn panning_moves_the_subject_with_the_cursor_and_scales_with_zoom() {
        let mut c = OrbitCamera::default();
        c.pan_by(100.0, 0.0, 800.0);
        assert!(
            c.pan[0] < 0.0,
            "dragging right moves the subject right, which is a negative target offset"
        );
        let far = c.pan[0];

        let mut z = OrbitCamera {
            zoom: 2.0,
            ..Default::default()
        };
        z.pan_by(100.0, 0.0, 800.0);
        assert!(
            (z.pan[0] - far * 2.0).abs() < 1e-6,
            "a pixel covers twice the world at twice the distance: {} vs {}",
            z.pan[0],
            far
        );
    }

    #[test]
    fn a_pan_of_a_whole_viewport_height_moves_the_target_by_the_expected_radii() {
        let mut c = OrbitCamera::default();
        c.pan_by(0.0, 800.0, 800.0);
        // 3.2 radii of subject height fill the frame at zoom 1.
        assert!((c.pan[1] - 3.2).abs() < 1e-5, "{}", c.pan[1]);
    }

    #[test]
    fn frame_refits_the_subject_but_keeps_the_angle_you_chose() {
        let mut c = OrbitCamera::default();
        c.orbit(200.0, 50.0);
        c.zoom_by(6.0);
        c.pan_by(300.0, -120.0, 720.0);
        let (yaw, pitch) = (c.yaw, c.pitch);
        assert!(!c.is_framed());

        c.frame();
        assert_eq!(c.zoom, 1.0);
        assert_eq!(c.pan, [0.0, 0.0]);
        assert!(c.is_framed());
        assert_eq!((c.yaw, c.pitch), (yaw, pitch), "framing is not a reset");
    }

    #[test]
    fn reset_returns_the_view_the_cpu_renderer_starts_from() {
        let mut c = OrbitCamera::default();
        c.orbit(200.0, 50.0);
        c.zoom_by(3.0);
        c.pan_by(10.0, 10.0, 600.0);
        c.reset();

        let d = Camera::default();
        assert_eq!(c.camera().yaw, d.yaw);
        assert_eq!(c.camera().pitch, d.pitch);
        assert_eq!(c.camera().zoom, d.zoom);
        assert_eq!(c.camera().yfov, d.yfov);
        assert_eq!(c.pan, [0.0, 0.0]);
    }

    #[test]
    fn the_camera_handed_to_the_renderer_carries_the_orbit_state() {
        let mut c = OrbitCamera::default();
        c.orbit(45.0, 0.0);
        c.zoom_by(2.0);
        let cam = c.camera();
        assert_eq!(cam.yaw, c.yaw);
        assert_eq!(cam.pitch, c.pitch);
        assert_eq!(cam.zoom, c.zoom);
    }
}
