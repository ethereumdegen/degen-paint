//! Window geometry. Every size the renderer sees is in *physical* pixels, because a
//! Retina window that configures its surface in logical points renders at half resolution
//! and looks soft — the bug this type exists to make impossible.

/// A drawing surface: its physical size and the display's scale factor.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Viewport {
    physical: [u32; 2],
    scale: f64,
}

impl Default for Viewport {
    fn default() -> Self {
        Self {
            physical: [1280, 800],
            scale: 1.0,
        }
    }
}

impl Viewport {
    /// From a physical size, as `winit` reports it: `Window::inner_size()` is already
    /// scaled, so this is the constructor the event loop uses.
    pub fn from_physical(physical: [u32; 2], scale: f64) -> Self {
        Self {
            physical,
            scale: if scale > 0.0 { scale } else { 1.0 },
        }
    }

    /// From a size in logical points. A 2x display turns 800x600 points into 1600x1200
    /// pixels, and the surface must be configured for the latter.
    pub fn from_logical(logical: [f64; 2], scale: f64) -> Self {
        let scale = if scale > 0.0 { scale } else { 1.0 };
        Self::from_physical(
            [
                (logical[0] * scale).round().max(0.0) as u32,
                (logical[1] * scale).round().max(0.0) as u32,
            ],
            scale,
        )
    }

    pub fn physical(&self) -> [u32; 2] {
        self.physical
    }

    pub fn scale(&self) -> f64 {
        self.scale
    }

    pub fn logical(&self) -> [f64; 2] {
        [
            self.physical[0] as f64 / self.scale,
            self.physical[1] as f64 / self.scale,
        ]
    }

    /// The aspect ratio the perspective projection is built from. A resize changes this
    /// and nothing else about the camera.
    pub fn aspect(&self) -> f32 {
        self.physical[0] as f32 / self.physical[1] as f32
    }

    pub fn width(&self) -> f32 {
        self.physical[0] as f32
    }

    pub fn height(&self) -> f32 {
        self.physical[1] as f32
    }

    pub fn center(&self) -> [f32; 2] {
        [self.width() * 0.5, self.height() * 0.5]
    }

    /// A physical size with a zero dimension means a minimized window; skip the frame
    /// rather than asking wgpu to configure a 0-wide surface.
    pub fn is_drawable(&self) -> bool {
        self.physical[0] > 0 && self.physical[1] > 0
    }

    pub fn resize(&mut self, physical: [u32; 2]) {
        self.physical = physical;
    }

    pub fn set_scale(&mut self, scale: f64) {
        if scale > 0.0 {
            self.scale = scale;
        }
    }
}

/// Parse a `WIDTHxHEIGHT` command-line size.
pub fn parse_size(s: &str) -> Result<[u32; 2], String> {
    let (w, h) = s
        .split_once(['x', 'X'])
        .ok_or_else(|| format!("expected WIDTHxHEIGHT, got {s:?}"))?;
    let parse = |v: &str, which: &str| -> Result<u32, String> {
        v.trim()
            .parse::<u32>()
            .map_err(|e| format!("{which} in {s:?}: {e}"))
            .and_then(|n| {
                if n == 0 {
                    Err(format!("{which} in {s:?} must be positive"))
                } else {
                    Ok(n)
                }
            })
    };
    Ok([parse(w, "width")?, parse(h, "height")?])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_two_x_display_renders_at_full_resolution() {
        let v = Viewport::from_logical([800.0, 600.0], 2.0);
        assert_eq!(
            v.physical(),
            [1600, 1200],
            "a 2x window must configure a 1600x1200 surface, not 800x600"
        );
        assert_eq!(v.logical(), [800.0, 600.0]);
    }

    #[test]
    fn scale_factor_does_not_change_the_projection_aspect() {
        let one = Viewport::from_logical([800.0, 600.0], 1.0);
        let two = Viewport::from_logical([800.0, 600.0], 2.0);
        assert_eq!(one.aspect(), two.aspect());
    }

    #[test]
    fn resizing_updates_the_projection_aspect() {
        let mut v = Viewport::from_physical([800, 800], 1.0);
        assert!((v.aspect() - 1.0).abs() < 1e-6);
        v.resize([1600, 800]);
        assert!(
            (v.aspect() - 2.0).abs() < 1e-6,
            "aspect after a widening resize: {}",
            v.aspect()
        );
        v.resize([400, 800]);
        assert!((v.aspect() - 0.5).abs() < 1e-6);
    }

    #[test]
    fn a_minimized_window_is_not_drawable() {
        assert!(!Viewport::from_physical([0, 600], 2.0).is_drawable());
        assert!(!Viewport::from_physical([800, 0], 2.0).is_drawable());
        assert!(Viewport::from_physical([1, 1], 1.0).is_drawable());
    }

    #[test]
    fn a_size_argument_parses_and_rejects_the_shapes_that_would_crash_wgpu() {
        assert_eq!(parse_size("1280x800"), Ok([1280, 800]));
        assert_eq!(parse_size("640X480"), Ok([640, 480]));
        assert!(parse_size("1280").is_err());
        assert!(parse_size("1280x").is_err());
        assert!(
            parse_size("0x800").is_err(),
            "a zero-wide texture is invalid"
        );
        assert!(parse_size("1280x0").is_err());
        assert!(parse_size("-4x8").is_err());
    }
}
