//! Text layers: shaping with `rustybuzz`, family resolution with `fontdb`, glyphs emitted as
//! outlines so a raster text layer and a vector text object have identical geometry.
//!
//! No system fonts are ever loaded. A project renders the same on every machine: fonts come
//! from `project.fonts` (embedded blobs) with `dpaint_core::FALLBACK_FONT` as the built-in
//! fallback — the same bytes the vector engine and the 3D extruder use, so one `TextSpec`
//! shapes identically in all three — and asking for a family that is not there produces a
//! `font-fallback` warning rather than a silent swap.

use crate::geom;
use dpaint_core::doc::common::{Rect, TextAlign, TextSpec};
use dpaint_core::kurbo::{BezPath, Shape};
use rustybuzz::ttf_parser::OutlineBuilder;
use dpaint_core::{AssetStore, Error, Project, Result};
use std::collections::BTreeMap;

/// The one canonical fallback face for the whole workspace, owned by `dpaint-core`.
pub use dpaint_core::FALLBACK_FAMILY;

/// The families served without a fallback warning when asked for generically.
fn generic(name: &str) -> Option<fontdb::Family<'static>> {
    match name.to_ascii_lowercase().as_str() {
        "sans-serif" | "sans" | "system-ui" | "ui-sans-serif" => Some(fontdb::Family::SansSerif),
        "serif" | "ui-serif" => Some(fontdb::Family::Serif),
        "monospace" | "mono" | "ui-monospace" => Some(fontdb::Family::Monospace),
        "cursive" => Some(fontdb::Family::Cursive),
        "fantasy" => Some(fontdb::Family::Fantasy),
        _ => None,
    }
}

/// Font database for one render: embedded fallbacks plus the project's registered fonts.
pub struct FontSet {
    db: fontdb::Database,
    /// Declared family (lowercased) -> the family name the font file actually reports.
    aliases: BTreeMap<String, String>,
}

impl FontSet {
    pub fn new(project: &Project, assets: &AssetStore) -> Self {
        let mut db = fontdb::Database::new();
        db.load_font_data(dpaint_core::FALLBACK_FONT.to_vec());
        // Every generic family resolves to the canonical face: there is exactly one
        // deterministic fallback, so a missing serif request is a reported fallback, not a
        // silent second typeface.
        db.set_sans_serif_family(FALLBACK_FAMILY);
        db.set_serif_family(FALLBACK_FAMILY);
        db.set_monospace_family(FALLBACK_FAMILY);
        db.set_cursive_family(FALLBACK_FAMILY);
        db.set_fantasy_family(FALLBACK_FAMILY);

        let mut aliases = BTreeMap::new();
        for entry in &project.fonts {
            let Ok(bytes) = assets.get(&entry.asset) else { continue };
            let before = db.len();
            db.load_font_data(bytes);
            if let Some(actual) = db
                .faces()
                .skip(before)
                .next()
                .and_then(|f| f.families.first().map(|(n, _)| n.clone()))
            {
                aliases.insert(entry.family.to_ascii_lowercase(), actual);
            }
        }
        Self { db, aliases }
    }

    fn has_family(&self, name: &str) -> Option<String> {
        self.db.faces().find_map(|f| {
            f.families
                .iter()
                .find(|(n, _)| n.eq_ignore_ascii_case(name))
                .map(|(n, _)| n.clone())
        })
    }

    /// Resolve a family to a concrete face. Returns the face, the family actually used and
    /// whether that was a fallback.
    fn pick(&self, spec: &TextSpec) -> Result<(fontdb::ID, String, bool)> {
        let requested = spec.family.trim();
        // `resolved` owns the family string the query borrows, so nothing has to be leaked
        // to manufacture a 'static lifetime.
        let alias = self.aliases.get(&requested.to_ascii_lowercase()).cloned();
        let (resolved, generic_family, fallback) = match alias.or_else(|| self.has_family(requested)) {
            Some(actual) => (Some(actual), None, false),
            None => match generic(requested) {
                Some(g) => (None, Some(g), false),
                None => (None, Some(fontdb::Family::SansSerif), true),
            },
        };
        let family = match (&resolved, generic_family) {
            (Some(name), _) => fontdb::Family::Name(name.as_str()),
            (None, Some(g)) => g,
            (None, None) => fontdb::Family::SansSerif,
        };
        let query = fontdb::Query {
            families: &[family],
            weight: fontdb::Weight(spec.weight),
            stretch: fontdb::Stretch::Normal,
            style: if spec.italic { fontdb::Style::Italic } else { fontdb::Style::Normal },
        };
        let id = self
            .db
            .query(&query)
            .or_else(|| self.db.faces().next().map(|f| f.id))
            .ok_or_else(|| Error::FontUnavailable(requested.to_string()))?;
        let used = self
            .db
            .face(id)
            .and_then(|f| f.families.first().map(|(n, _)| n.clone()))
            .unwrap_or_else(|| FALLBACK_FAMILY.to_string());
        Ok((id, used, fallback))
    }
}

/// The result of laying out a text spec: one outline in document coordinates.
pub struct TextLayout {
    pub outline: BezPath,
    pub bounds: Rect,
    pub lines: usize,
    pub used_family: String,
    pub fallback: bool,
    /// The laid-out text is taller than its box — `text.fit` exists to fix this.
    pub overflow: bool,
}

struct Outliner<'a> {
    path: &'a mut BezPath,
    scale: f64,
    x: f64,
    y: f64,
    open: bool,
}

impl rustybuzz::ttf_parser::OutlineBuilder for Outliner<'_> {
    fn move_to(&mut self, x: f32, y: f32) {
        if self.open {
            self.path.close_path();
        }
        self.path.move_to(self.map(x, y));
        self.open = true;
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.path.line_to(self.map(x, y));
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        self.path.quad_to(self.map(x1, y1), self.map(x, y));
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        self.path.curve_to(self.map(x1, y1), self.map(x2, y2), self.map(x, y));
    }
    fn close(&mut self) {
        if self.open {
            self.path.close_path();
            self.open = false;
        }
    }
}

impl Outliner<'_> {
    #[inline]
    fn map(&self, x: f32, y: f32) -> (f64, f64) {
        // Font space is y-up; document space is y-down.
        (self.x + x as f64 * self.scale, self.y - y as f64 * self.scale)
    }
}

struct Run {
    glyphs: Vec<(u16, f64, f64)>,
    advance: f64,
    spaces: usize,
}

fn shape_run(face: &rustybuzz::Face<'_>, text: &str, tracking: f64) -> Run {
    let upem = face.units_per_em() as f64;
    let mut buf = rustybuzz::UnicodeBuffer::new();
    buf.push_str(text);
    let out = rustybuzz::shape(face, &[], buf);
    let mut glyphs = Vec::with_capacity(out.len());
    let mut pen = 0.0f64;
    for (info, pos) in out.glyph_infos().iter().zip(out.glyph_positions()) {
        glyphs.push((
            info.glyph_id as u16,
            pen + pos.x_offset as f64 / upem,
            pos.y_offset as f64 / upem,
        ));
        pen += pos.x_advance as f64 / upem + tracking;
    }
    Run { glyphs, advance: pen, spaces: text.chars().filter(|c| *c == ' ').count() }
}

/// Lay out a text spec into a single outline.
///
/// Origin: with a `box`, the first baseline sits one ascender below the box top and lines
/// advance by `leading * size`; without a box, layout starts at `(0, ascender)` so the text
/// is on-canvas before any layer transform is applied.
pub fn layout(spec: &TextSpec, fonts: &FontSet) -> Result<TextLayout> {
    if spec.size <= 0.0 {
        return Err(Error::Invalid("text size must be positive".into()));
    }
    let (id, used_family, fallback) = fonts.pick(spec)?;
    let laid = fonts
        .db
        .with_face_data(id, |data, index| -> Result<(BezPath, usize, bool)> {
            let face = rustybuzz::Face::from_slice(data, index)
                .ok_or_else(|| Error::FontUnavailable(spec.family.clone()))?;
            Ok(layout_with_face(spec, &face))
        })
        .ok_or_else(|| Error::FontUnavailable(spec.family.clone()))??;
    let (outline, lines, overflow) = laid;
    let bb = outline.bounding_box();
    Ok(TextLayout {
        bounds: if outline.elements().is_empty() {
            Rect::default()
        } else {
            Rect::new(bb.x0, bb.y0, bb.width(), bb.height())
        },
        outline,
        lines,
        used_family,
        fallback,
        overflow,
    })
}

fn layout_with_face(spec: &TextSpec, face: &rustybuzz::Face<'_>) -> (BezPath, usize, bool) {
    let upem = face.units_per_em() as f64;
    let size = spec.size;
    let ascender = face.ascender() as f64 / upem * size;
    let line_height = spec.leading.max(0.01) * size;
    let tracking = spec.tracking / size; // em units, so it scales with the size

    let (ox, oy, box_w, box_h) = match spec.r#box {
        Some(b) => (b.x(), b.y(), b.w(), b.h()),
        None => (0.0, 0.0, 0.0, 0.0),
    };

    // Word wrap each hard-broken paragraph to the box width.
    let mut lines: Vec<String> = Vec::new();
    for para in spec.text.split('\n') {
        if box_w <= 0.0 {
            lines.push(para.to_string());
            continue;
        }
        let mut current = String::new();
        for word in para.split(' ') {
            let candidate =
                if current.is_empty() { word.to_string() } else { format!("{current} {word}") };
            let w = shape_run(face, &candidate, tracking).advance * size;
            if w > box_w && !current.is_empty() {
                lines.push(std::mem::take(&mut current));
                current = word.to_string();
            } else {
                current = candidate;
            }
        }
        lines.push(current);
    }

    let mut path = BezPath::new();
    let mut baseline = oy + ascender;
    let n_lines = lines.len();
    for (li, line) in lines.iter().enumerate() {
        let run = shape_run(face, line, tracking);
        let width = run.advance * size;
        let mut x = ox;
        let mut word_gap = 0.0;
        match spec.align {
            TextAlign::Left => {}
            TextAlign::Center => x += (box_w - width) / 2.0,
            TextAlign::Right => x += box_w - width,
            TextAlign::Justify => {
                let last = li + 1 == n_lines;
                if !last && run.spaces > 0 && box_w > width {
                    word_gap = (box_w - width) / run.spaces as f64;
                }
            }
        }
        let space_gid = face.glyph_index(' ').map(|g| g.0);
        let mut extra = 0.0;
        for (gid, gx, gy) in &run.glyphs {
            let mut o = Outliner {
                path: &mut path,
                scale: size / upem,
                x: x + gx * size + extra,
                y: baseline - gy * size,
                open: false,
            };
            face.outline_glyph(rustybuzz::ttf_parser::GlyphId(*gid), &mut o);
            o.close();
            if word_gap > 0.0 && Some(*gid) == space_gid {
                extra += word_gap;
            }
        }
        baseline += line_height;
    }
    let overflow = box_h > 0.0 && (n_lines as f64 * line_height) > box_h + 1e-6;
    (path, n_lines, overflow)
}

/// Largest size not exceeding the box, for `text.fit` (shrink-to-box).
pub fn fit_size(spec: &TextSpec, fonts: &FontSet, min: f64) -> Result<f64> {
    let Some(b) = spec.r#box else {
        return Err(Error::Invalid("text.fit needs the layer to have a box".into()));
    };
    let mut size = spec.size;
    let mut probe = spec.clone();
    while size > min {
        probe.size = size;
        let l = layout(&probe, fonts)?;
        if !l.overflow && l.bounds.w() <= b.w() + 0.5 {
            return Ok(size);
        }
        size -= (size * 0.05).max(0.25);
    }
    Ok(min)
}

/// Text outline as SVG path data, for `text.to-shape` and selection-from-text.
pub fn outline_d(spec: &TextSpec, fonts: &FontSet) -> Result<(String, bool, String)> {
    let l = layout(spec, fonts)?;
    if l.outline.elements().is_empty() {
        return Err(Error::DegenerateGeometry("text produced no glyph outlines".into()));
    }
    Ok((l.outline.to_svg(), l.fallback, l.used_family))
}

/// Coverage of laid-out text at a device scale.
pub fn text_coverage(spec: &TextSpec, fonts: &FontSet, w: u32, h: u32, scale: f64) -> Result<(Vec<f32>, TextLayout)> {
    let l = layout(spec, fonts)?;
    if l.outline.elements().is_empty() {
        return Ok((vec![0.0; w as usize * h as usize], l));
    }
    let sk = geom::to_sk(&l.outline, dpaint_core::kurbo::Affine::scale(scale))
        .ok_or_else(|| Error::DegenerateGeometry("text outline is empty".into()))?;
    let cov = geom::fill_coverage(&sk, w, h, dpaint_core::doc::common::FillRule::Nonzero);
    Ok((cov, l))
}

#[cfg(test)]
mod tests {
    use super::*;
    use dpaint_core::doc::{Document, RasterDoc};
    use dpaint_core::DocId;

    fn fonts() -> FontSet {
        let doc = Document::Raster(RasterDoc::new(DocId::from("doc_t"), "t", 4, 4));
        let project = Project::new("t", doc);
        FontSet::new(&project, &AssetStore::new(std::env::temp_dir().join("dpaint-fonts-test")))
    }

    #[test]
    fn missing_family_falls_back_and_still_produces_glyphs() {
        let f = fonts();
        let spec = TextSpec { family: "NoSuchFaceHere".into(), size: 24.0, ..TextSpec::new("Hi") };
        let l = layout(&spec, &f).unwrap();
        assert!(l.fallback, "a missing family must be reported as a fallback");
        assert_eq!(l.used_family, FALLBACK_FAMILY);
        assert!(l.bounds.w() > 1.0 && l.bounds.h() > 1.0, "glyphs still rendered: {:?}", l.bounds);
    }

    #[test]
    fn generic_sans_serif_is_not_a_fallback() {
        let f = fonts();
        let l = layout(&TextSpec::new("Hi"), &f).unwrap();
        assert!(!l.fallback, "'sans-serif' is a generic request, not a miss");
    }

    #[test]
    fn text_wraps_to_its_box_and_respects_leading() {
        let f = fonts();
        let mut spec = TextSpec::new("the quick brown fox jumps over the lazy dog");
        spec.size = 16.0;
        spec.r#box = Some(Rect::new(0.0, 0.0, 120.0, 400.0));
        let wrapped = layout(&spec, &f).unwrap();
        assert!(wrapped.lines > 2, "long text in a narrow box must wrap: {}", wrapped.lines);
        assert!(wrapped.bounds.w() <= 121.0, "wrapped text stays in the box: {:?}", wrapped.bounds);

        spec.leading = 3.0;
        let loose = layout(&spec, &f).unwrap();
        assert!(loose.bounds.h() > wrapped.bounds.h(), "more leading must be taller");
    }

    #[test]
    fn centered_text_is_centered_in_its_box() {
        let f = fonts();
        let mut spec = TextSpec::new("ab");
        spec.size = 20.0;
        spec.r#box = Some(Rect::new(0.0, 0.0, 200.0, 40.0));
        let left = layout(&spec, &f).unwrap();
        spec.align = TextAlign::Center;
        let center = layout(&spec, &f).unwrap();
        assert!(center.bounds.x() > left.bounds.x() + 50.0, "{:?} vs {:?}", left.bounds, center.bounds);
    }
}
