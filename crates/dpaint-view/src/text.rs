//! Status text rasterization.
//!
//! The overlay needs a few short lines of legible text on top of a GPU frame, and the
//! project already ships a deterministic face — `dpaint_core::FALLBACK_FONT`, the same
//! one raster, vector and 3D text fall back to. Outlining it with `ttf-parser` and
//! filling with `tiny-skia` reuses two dependencies the workspace already builds, which
//! is cheaper in every sense than adding a text-rendering stack for a status bar.

use dpaint_core::FALLBACK_FONT;
use tiny_skia::{FillRule, Paint, PathBuilder, Pixmap, Transform};

pub struct TextRaster {
    face: ttf_parser::Face<'static>,
    upem: f32,
}

struct Outliner {
    pb: PathBuilder,
    scale: f32,
    x: f32,
    baseline: f32,
}

impl ttf_parser::OutlineBuilder for Outliner {
    fn move_to(&mut self, x: f32, y: f32) {
        self.pb
            .move_to(self.x + x * self.scale, self.baseline - y * self.scale);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.pb
            .line_to(self.x + x * self.scale, self.baseline - y * self.scale);
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        self.pb.quad_to(
            self.x + x1 * self.scale,
            self.baseline - y1 * self.scale,
            self.x + x * self.scale,
            self.baseline - y * self.scale,
        );
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        self.pb.cubic_to(
            self.x + x1 * self.scale,
            self.baseline - y1 * self.scale,
            self.x + x2 * self.scale,
            self.baseline - y2 * self.scale,
            self.x + x * self.scale,
            self.baseline - y * self.scale,
        );
    }
    fn close(&mut self) {
        self.pb.close();
    }
}

impl TextRaster {
    pub fn new() -> Option<Self> {
        let face = ttf_parser::Face::parse(FALLBACK_FONT, 0).ok()?;
        let upem = face.units_per_em() as f32;
        Some(Self { face, upem })
    }

    pub fn width(&self, line: &str, px: f32) -> f32 {
        let scale = px / self.upem;
        line.chars()
            .filter_map(|c| self.face.glyph_index(c))
            .filter_map(|g| self.face.glyph_hor_advance(g))
            .map(|a| a as f32 * scale)
            .sum()
    }

    /// A translucent panel with the given lines drawn into it, premultiplied RGBA.
    pub fn panel(&self, lines: &[String], px: f32) -> Option<Pixmap> {
        let pad = (px * 0.5).round();
        let line_h = (px * 1.35).round();
        let w = lines
            .iter()
            .map(|l| self.width(l, px))
            .fold(0.0f32, f32::max);
        let width = (w + pad * 2.0).ceil().max(1.0) as u32;
        let height = (line_h * lines.len() as f32 + pad * 2.0).ceil().max(1.0) as u32;
        let mut pm = Pixmap::new(width, height)?;
        pm.fill(tiny_skia::Color::from_rgba8(8, 10, 14, 190));

        let mut paint = Paint::default();
        paint.set_color(tiny_skia::Color::from_rgba8(236, 240, 245, 255));
        paint.anti_alias = true;

        let scale = px / self.upem;
        for (i, line) in lines.iter().enumerate() {
            let baseline = pad + line_h * i as f32 + px;
            let mut o = Outliner {
                pb: PathBuilder::new(),
                scale,
                x: pad,
                baseline,
            };
            for ch in line.chars() {
                let Some(gid) = self.face.glyph_index(ch) else {
                    continue;
                };
                self.face.outline_glyph(gid, &mut o);
                o.x += self.face.glyph_hor_advance(gid).unwrap_or(0) as f32 * scale;
            }
            if let Some(path) = o.pb.finish() {
                pm.fill_path(
                    &path,
                    &paint,
                    FillRule::Winding,
                    Transform::identity(),
                    None,
                );
            }
        }
        Some(pm)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_panel_grows_with_the_longest_line_and_the_line_count() {
        let t = TextRaster::new().expect("the embedded face must parse");
        let one = t.panel(&["doc_scene · model".into()], 16.0).expect("panel");
        let two = t
            .panel(
                &["doc_scene · model".into(), "zoom 1.00x  yaw 35.0°".into()],
                16.0,
            )
            .expect("panel");
        assert!(
            two.height() > one.height(),
            "{} vs {}",
            two.height(),
            one.height()
        );
        assert!(
            two.width() > one.width(),
            "the panel must fit its widest line: {} vs {}",
            two.width(),
            one.width()
        );
    }

    #[test]
    fn text_actually_marks_pixels_that_the_empty_panel_does_not() {
        let t = TextRaster::new().expect("face");
        let blank = t.panel(&["    ".into()], 20.0).expect("panel");
        let text = t.panel(&["MMMM".into()], 20.0).expect("panel");
        assert_eq!(blank.height(), text.height());

        let bright = |pm: &Pixmap| {
            pm.pixels()
                .iter()
                .filter(|p| p.red() > 180 && p.green() > 180)
                .count()
        };
        assert_eq!(bright(&blank), 0, "spaces draw nothing");
        assert!(
            bright(&text) > 100,
            "four glyphs at 20 px should light up plenty of pixels, got {}",
            bright(&text)
        );
    }

    #[test]
    fn the_panel_is_opaque_enough_to_read_against_any_frame() {
        let t = TextRaster::new().expect("face");
        let pm = t.panel(&["status".into()], 16.0).expect("panel");
        let corner = pm.pixel(0, 0).expect("corner");
        assert!(
            corner.alpha() > 150,
            "the backing panel must be mostly opaque, got alpha {}",
            corner.alpha()
        );
    }

    #[test]
    fn measuring_is_monotonic_in_the_text_and_in_the_size() {
        let t = TextRaster::new().expect("face");
        assert!(t.width("ii", 16.0) < t.width("MM", 16.0));
        assert!(t.width("status", 16.0) < t.width("status", 32.0));
        assert_eq!(t.width("", 16.0), 0.0);
    }
}
