//! Text: shaping with `rustybuzz`, font lookup through `fontdb`, outlines from `ttf-parser`.
//!
//! An embedded Roboto is the deterministic fallback: no system font is ever consulted, so a
//! render on one machine matches a render on another. Whenever the requested family is not
//! available the caller gets the substituted family name back and turns it into a
//! `font-fallback` warning.

use crate::geom::Subpath;
use dpaint_core::asset::AssetStore;
use dpaint_core::doc::common::{TextAlign, TextSpec};
use dpaint_core::error::{Error, Result};
use dpaint_core::kurbo::{Affine, BezPath, Line as KLine, ParamCurve, Point, Shape, Vec2};
use dpaint_core::project::Project;
use std::sync::LazyLock;

/// The fallback face, owned by `dpaint-core` so every mode shapes a `TextSpec` the same
/// way. Re-exported here because this crate's callers reach for it through `text::`.
pub use dpaint_core::{FALLBACK_FAMILY, FALLBACK_FONT};

/// Families that mean "whatever the renderer's default is" rather than a specific face.
const GENERIC: &[&str] = &["sans-serif", "sans", "default", "system-ui", FALLBACK_FAMILY];

pub struct Fonts {
    db: fontdb::Database,
    fallback: fontdb::ID,
}

/// The process-wide deterministic font set: the embedded fallback and nothing else.
pub fn fonts() -> &'static Fonts {
    static F: LazyLock<Fonts> = LazyLock::new(Fonts::new_embedded);
    &F
}

/// One resolved face plus the family that was actually used.
pub struct Selection {
    pub id: fontdb::ID,
    /// `Some(actual)` when the requested family was unavailable.
    pub substituted: Option<String>,
}

impl Fonts {
    pub fn new_embedded() -> Fonts {
        let mut db = fontdb::Database::new();
        db.load_font_data(FALLBACK_FONT.to_vec());
        let fallback = db
            .faces()
            .next()
            .expect("embedded fallback font must parse")
            .id;
        Fonts { db, fallback }
    }

    /// The embedded fallback plus every font registered into the project by `font.register`.
    /// Still never touches the system font list.
    pub fn for_project(project: &Project, assets: &AssetStore) -> Fonts {
        let mut f = Fonts::new_embedded();
        for entry in &project.fonts {
            if let Ok(bytes) = assets.get(&entry.asset) {
                f.db.load_font_data(bytes);
            }
        }
        f
    }

    pub fn db(&self) -> &fontdb::Database {
        &self.db
    }

    pub fn select(&self, family: &str, weight: u16, italic: bool) -> Selection {
        let wanted = family.trim();
        let generic = GENERIC.iter().any(|g| g.eq_ignore_ascii_case(wanted));
        if !generic {
            let q = fontdb::Query {
                families: &[fontdb::Family::Name(wanted)],
                weight: fontdb::Weight(weight),
                stretch: fontdb::Stretch::Normal,
                style: if italic {
                    fontdb::Style::Italic
                } else {
                    fontdb::Style::Normal
                },
            };
            if let Some(id) = self.db.query(&q) {
                let exact = self
                    .db
                    .face(id)
                    .map(|f| f.families.iter().any(|(n, _)| n.eq_ignore_ascii_case(wanted)))
                    .unwrap_or(false);
                if exact {
                    return Selection {
                        id,
                        substituted: None,
                    };
                }
            }
        }
        Selection {
            id: self.fallback,
            substituted: if generic {
                None
            } else {
                Some(FALLBACK_FAMILY.to_string())
            },
        }
    }

    fn with_face<R>(&self, id: fontdb::ID, f: impl FnOnce(&rustybuzz::Face) -> R) -> Option<R> {
        self.db.with_face_data(id, |data, index| {
            rustybuzz::Face::from_slice(data, index).map(|face| f(&face))
        })?
    }
}

/// One positioned glyph in text space (y down, origin at the line's baseline start).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Glyph {
    pub gid: u16,
    /// Pen position of the glyph origin relative to the start of its line.
    pub x: f64,
    pub y: f64,
    pub advance: f64,
    /// Byte index in the source string, for cluster-aware callers.
    pub cluster: u32,
}

#[derive(Debug, Clone, Default)]
pub struct Line {
    pub glyphs: Vec<Glyph>,
    pub width: f64,
}

/// Font metrics scaled to the requested size.
#[derive(Debug, Clone, Copy)]
pub struct Metrics {
    pub ascent: f64,
    pub descent: f64,
    pub line_height: f64,
    pub scale: f64,
}

#[derive(Debug, Clone)]
pub struct Shaped {
    pub lines: Vec<Line>,
    pub metrics: Metrics,
    /// Family actually used, when it differs from the one requested.
    pub substituted: Option<String>,
}

impl Shaped {
    pub fn total_advance(&self) -> f64 {
        self.lines.iter().map(|l| l.width).fold(0.0, f64::max)
    }
}

/// Shape `spec.text`, honouring explicit newlines and — when the spec has a box — greedy
/// word wrapping to the box width.
pub fn shape(fonts: &Fonts, spec: &TextSpec) -> Shaped {
    let sel = fonts.select(&spec.family, spec.weight, spec.italic);
    let wrap = spec.r#box.map(|b| b.w()).filter(|w| *w > 0.0);
    let out = fonts
        .with_face(sel.id, |face| {
            let upem = face.units_per_em() as f64;
            let scale = spec.size / upem.max(1.0);
            let metrics = Metrics {
                ascent: face.ascender() as f64 * scale,
                descent: -(face.descender() as f64) * scale,
                line_height: spec.size * spec.leading,
                scale,
            };
            let mut lines = Vec::new();
            for para in spec.text.split('\n') {
                match wrap {
                    Some(w) => lines.extend(wrap_paragraph(face, para, scale, spec.tracking, w)),
                    None => lines.push(shape_run(face, para, scale, spec.tracking)),
                }
            }
            if lines.is_empty() {
                lines.push(Line::default());
            }
            Shaped {
                lines,
                metrics,
                substituted: sel.substituted.clone(),
            }
        });
    out.unwrap_or_else(|| Shaped {
        lines: vec![Line::default()],
        metrics: Metrics {
            ascent: spec.size * 0.8,
            descent: spec.size * 0.2,
            line_height: spec.size * spec.leading,
            scale: 1.0,
        },
        substituted: Some(FALLBACK_FAMILY.to_string()),
    })
}

fn shape_run(face: &rustybuzz::Face, text: &str, scale: f64, tracking: f64) -> Line {
    let mut line = Line::default();
    if text.is_empty() {
        return line;
    }
    let mut buf = rustybuzz::UnicodeBuffer::new();
    buf.push_str(text);
    let g = rustybuzz::shape(face, &[], buf);
    let infos = g.glyph_infos();
    let pos = g.glyph_positions();
    let mut pen = 0.0;
    for (i, p) in infos.iter().zip(pos.iter()) {
        let adv = p.x_advance as f64 * scale + tracking;
        line.glyphs.push(Glyph {
            gid: i.glyph_id as u16,
            x: pen + p.x_offset as f64 * scale,
            y: -(p.y_offset as f64) * scale,
            advance: adv,
            cluster: i.cluster,
        });
        pen += adv;
    }
    line.width = pen;
    line
}

fn measure(face: &rustybuzz::Face, text: &str, scale: f64, tracking: f64) -> f64 {
    shape_run(face, text, scale, tracking).width
}

fn wrap_paragraph(
    face: &rustybuzz::Face,
    para: &str,
    scale: f64,
    tracking: f64,
    max_w: f64,
) -> Vec<Line> {
    let words: Vec<&str> = para.split_whitespace().collect();
    if words.is_empty() {
        return vec![Line::default()];
    }
    let mut out = Vec::new();
    let mut cur = String::new();
    for w in words {
        let cand = if cur.is_empty() {
            w.to_string()
        } else {
            format!("{cur} {w}")
        };
        if !cur.is_empty() && measure(face, &cand, scale, tracking) > max_w {
            out.push(shape_run(face, &cur, scale, tracking));
            cur = w.to_string();
        } else {
            cur = cand;
        }
    }
    if !cur.is_empty() {
        out.push(shape_run(face, &cur, scale, tracking));
    }
    out
}

/// Outline of one glyph in text space (y down), at the origin, already scaled.
pub fn glyph_outline(fonts: &Fonts, id: fontdb::ID, gid: u16, scale: f64) -> Option<BezPath> {
    fonts.with_face(id, |face| {
        let mut b = Builder::default();
        face.outline_glyph(ttf_parser::GlyphId(gid), &mut b)?;
        let mut p = b.path;
        // Font space is y-up; document space is y-down.
        p.apply_affine(Affine::new([scale, 0.0, 0.0, -scale, 0.0, 0.0]));
        Some(p)
    })?
}

#[derive(Default)]
struct Builder {
    path: BezPath,
    open: bool,
}

impl ttf_parser::OutlineBuilder for Builder {
    fn move_to(&mut self, x: f32, y: f32) {
        if self.open {
            self.path.close_path();
        }
        self.path.move_to((x as f64, y as f64));
        self.open = true;
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.path.line_to((x as f64, y as f64));
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        self.path
            .quad_to((x1 as f64, y1 as f64), (x as f64, y as f64));
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        self.path.curve_to(
            (x1 as f64, y1 as f64),
            (x2 as f64, y2 as f64),
            (x as f64, y as f64),
        );
    }
    fn close(&mut self) {
        if self.open {
            self.path.close_path();
            self.open = false;
        }
    }
}

/// Per-line outlines of a text block anchored at `origin` (top-left of the first line's
/// em box) or inside `spec.box` when one is set.
pub fn outline_block_lines(
    fonts: &Fonts,
    spec: &TextSpec,
    origin: (f64, f64),
) -> (Vec<BezPath>, Option<String>) {
    let sel = fonts.select(&spec.family, spec.weight, spec.italic);
    let shaped = shape(fonts, spec);
    let m = shaped.metrics;
    let (bx, by, bw) = match spec.r#box {
        Some(b) => (b.x(), b.y(), Some(b.w())),
        None => (origin.0, origin.1, None),
    };
    let mut out = Vec::new();
    for (i, line) in shaped.lines.iter().enumerate() {
        let baseline = by + m.ascent + m.line_height * i as f64;
        let dx = align_offset(spec.align, line.width, bw);
        let mut lp = BezPath::new();
        for g in &line.glyphs {
            if let Some(o) = glyph_outline(fonts, sel.id, g.gid, m.scale) {
                if o.elements().is_empty() {
                    continue;
                }
                lp.extend(Affine::translate((bx + dx + g.x, baseline + g.y)) * o);
            }
        }
        out.push(lp);
    }
    (out, shaped.substituted)
}

fn align_offset(align: TextAlign, line_w: f64, box_w: Option<f64>) -> f64 {
    match (align, box_w) {
        (TextAlign::Left, _) | (TextAlign::Justify, _) => 0.0,
        (TextAlign::Center, Some(w)) => (w - line_w) / 2.0,
        (TextAlign::Center, None) => -line_w / 2.0,
        (TextAlign::Right, Some(w)) => w - line_w,
        (TextAlign::Right, None) => -line_w,
    }
}

/// Whole text block as one path.
pub fn outline_block(fonts: &Fonts, spec: &TextSpec, origin: (f64, f64)) -> (BezPath, Option<String>) {
    let (lines, sub) = outline_block_lines(fonts, spec, origin);
    let mut p = BezPath::new();
    for l in lines {
        p.extend(l);
    }
    (p, sub)
}

/// Where the first and last glyph of a run landed, in document space. Text-on-path tests
/// and the `text.on-path` op both report this.
#[derive(Debug, Clone, Copy)]
pub struct RunSpan {
    pub start: Point,
    pub end: Point,
    pub path_length: f64,
    pub advance: f64,
}

/// Place each glyph of a single-line run along `path`, rotated to the local tangent.
///
/// `offset` shifts the run forward along the arc; `right` flips the glyphs to the other
/// side of the curve. Glyph n sits at arc length `offset + pen_x + advance/2`, so the run
/// advances by arc length exactly as it would on a straight baseline.
pub fn outline_on_path(
    fonts: &Fonts,
    spec: &TextSpec,
    path: &[Subpath],
    offset: f64,
    right: bool,
) -> (BezPath, Option<String>, Option<RunSpan>) {
    let sel = fonts.select(&spec.family, spec.weight, spec.italic);
    let shaped = shape(fonts, spec);
    let m = shaped.metrics;
    let walker = ArcWalker::new(path);
    let Some(total) = walker.total() else {
        return (BezPath::new(), shaped.substituted, None);
    };
    let line = shaped.lines.first().cloned().unwrap_or_default();
    let base = offset
        + match spec.align {
            TextAlign::Center => (total - line.width) / 2.0,
            TextAlign::Right => total - line.width,
            _ => 0.0,
        };
    let mut out = BezPath::new();
    let mut first = None;
    let mut last = None;
    for g in &line.glyphs {
        let s = base + g.x;
        let Some((pt, tan)) = walker.at(s) else { continue };
        let normal = Vec2::new(-tan.y, tan.x);
        let side = if right { -1.0 } else { 1.0 };
        // The baseline sits on the curve; glyphs stand on the normal side.
        let origin = pt + normal * (side * 0.0) + Vec2::new(0.0, 0.0);
        let angle = tan.y.atan2(tan.x) + if right { std::f64::consts::PI } else { 0.0 };
        let at = Affine::translate((origin.x, origin.y))
            * Affine::rotate(angle)
            * Affine::translate((0.0, g.y));
        if first.is_none() {
            first = Some(pt);
        }
        last = Some(walker.at(s + g.advance).map(|(p, _)| p).unwrap_or(pt));
        if let Some(o) = glyph_outline(fonts, sel.id, g.gid, m.scale) {
            if !o.elements().is_empty() {
                out.extend(at * o);
            }
        }
    }
    let span = match (first, last) {
        (Some(a), Some(b)) => Some(RunSpan {
            start: a,
            end: b,
            path_length: total,
            advance: line.width,
        }),
        _ => None,
    };
    (out, shaped.substituted, span)
}

/// Arc-length parameterisation of a flattened path, shared by text-on-path and the
/// measure ops.
pub struct ArcWalker {
    pts: Vec<Point>,
    cum: Vec<f64>,
}

impl ArcWalker {
    pub fn new(subpaths: &[Subpath]) -> Self {
        let mut pts = Vec::new();
        for sp in subpaths {
            pts.extend_from_slice(&sp.points);
            if sp.closed {
                if let Some(f) = sp.points.first() {
                    pts.push(*f);
                }
            }
        }
        let mut cum = Vec::with_capacity(pts.len());
        let mut acc = 0.0;
        for (i, p) in pts.iter().enumerate() {
            if i > 0 {
                acc += pts[i - 1].distance(*p);
            }
            cum.push(acc);
        }
        Self { pts, cum }
    }

    pub fn total(&self) -> Option<f64> {
        let t = *self.cum.last()?;
        if t > 0.0 {
            Some(t)
        } else {
            None
        }
    }

    /// Position and unit tangent at arc length `s`, clamped to the ends.
    pub fn at(&self, s: f64) -> Option<(Point, Vec2)> {
        let total = self.total()?;
        let s = s.clamp(0.0, total);
        let idx = match self
            .cum
            .binary_search_by(|v| v.partial_cmp(&s).unwrap_or(std::cmp::Ordering::Equal))
        {
            Ok(i) => i.min(self.pts.len() - 1),
            Err(i) => i.max(1) - 1,
        };
        let i = idx.min(self.pts.len() - 2);
        let (a, b) = (self.pts[i], self.pts[i + 1]);
        let seg = self.cum[i + 1] - self.cum[i];
        let t = if seg > 0.0 { (s - self.cum[i]) / seg } else { 0.0 };
        let dir = b - a;
        let dir = if dir.hypot() < 1e-12 {
            Vec2::new(1.0, 0.0)
        } else {
            dir.normalize()
        };
        Some((a.lerp(b, t), dir))
    }
}

/// Flow text into an arbitrary shape: lines are clipped to the shape's horizontal spans.
/// Returns the outline, any font substitution, and the words that did not fit.
pub fn flow_in_shape(
    fonts: &Fonts,
    spec: &TextSpec,
    shape_path: &BezPath,
    even_odd: bool,
) -> Result<(BezPath, Option<String>, Vec<String>)> {
    let bounds = shape_path.bounding_box();
    if bounds.width() <= 0.0 || bounds.height() <= 0.0 {
        return Err(Error::DegenerateGeometry(
            "cannot flow text into an empty shape".into(),
        ));
    }
    let sel = fonts.select(&spec.family, spec.weight, spec.italic);
    let sub = sel.substituted.clone();
    let words: Vec<String> = spec
        .text
        .split_whitespace()
        .map(|s| s.to_string())
        .collect();
    let result = fonts.with_face(sel.id, |face| {
        let upem = face.units_per_em() as f64;
        let scale = spec.size / upem.max(1.0);
        let ascent = face.ascender() as f64 * scale;
        let descent = -(face.descender() as f64) * scale;
        let lh = spec.size * spec.leading;
        let mut placed: Vec<(f64, f64, String)> = Vec::new();
        let mut queue = words.clone();
        let mut baseline = bounds.y0 + ascent;
        while baseline - ascent < bounds.y1 && !queue.is_empty() {
            let span = widest_span(shape_path, baseline - ascent, baseline + descent, even_odd);
            if let Some((x0, x1)) = span {
                let max_w = x1 - x0;
                let mut cur = String::new();
                while let Some(w) = queue.first() {
                    let cand = if cur.is_empty() {
                        w.clone()
                    } else {
                        format!("{cur} {w}")
                    };
                    if !cur.is_empty() && measure(face, &cand, scale, spec.tracking) > max_w {
                        break;
                    }
                    if cur.is_empty() && measure(face, &cand, scale, spec.tracking) > max_w {
                        // A single word wider than the span: drop it to the overflow list
                        // instead of spilling outside the shape.
                        break;
                    }
                    cur = cand;
                    queue.remove(0);
                }
                if !cur.is_empty() {
                    let w = measure(face, &cur, scale, spec.tracking);
                    let dx = match spec.align {
                        TextAlign::Center => (max_w - w) / 2.0,
                        TextAlign::Right => max_w - w,
                        _ => 0.0,
                    };
                    placed.push((x0 + dx, baseline, cur));
                }
            }
            baseline += lh;
        }
        let mut out = BezPath::new();
        for (x, y, s) in &placed {
            let line = shape_run(face, s, scale, spec.tracking);
            for g in &line.glyphs {
                let mut b = Builder::default();
                if face.outline_glyph(ttf_parser::GlyphId(g.gid), &mut b).is_none() {
                    continue;
                }
                let mut gp = b.path;
                gp.apply_affine(Affine::new([scale, 0.0, 0.0, -scale, 0.0, 0.0]));
                out.extend(Affine::translate((x + g.x, y + g.y)) * gp);
            }
        }
        (out, queue)
    });
    let (out, left) = result.unwrap_or_else(|| (BezPath::new(), words));
    Ok((out, sub, left))
}

/// Widest horizontal run of interior available across the whole band `[y_top, y_bot]`.
fn widest_span(path: &BezPath, y_top: f64, y_bot: f64, even_odd: bool) -> Option<(f64, f64)> {
    let a = spans_at(path, y_top.min(y_bot) + 1e-6, even_odd);
    let b = spans_at(path, y_bot.max(y_top) - 1e-6, even_odd);
    let mut best: Option<(f64, f64)> = None;
    for (a0, a1) in &a {
        for (b0, b1) in &b {
            let lo = a0.max(*b0);
            let hi = a1.min(*b1);
            if hi > lo && best.map(|(l, h)| hi - lo > h - l).unwrap_or(true) {
                best = Some((lo, hi));
            }
        }
    }
    best
}

fn spans_at(path: &BezPath, y: f64, even_odd: bool) -> Vec<(f64, f64)> {
    let b = path.bounding_box();
    let line = KLine::new((b.x0 - 1.0, y), (b.x1 + 1.0, y));
    let mut xs: Vec<f64> = Vec::new();
    for seg in path.segments() {
        for hit in seg.intersect_line(line) {
            xs.push(seg.eval(hit.segment_t).x);
        }
    }
    xs.sort_by(|p, q| p.partial_cmp(q).unwrap_or(std::cmp::Ordering::Equal));
    xs.dedup_by(|a, b| (*a - *b).abs() < 1e-9);
    let mut out = Vec::new();
    for w in xs.windows(2) {
        let mid = Point::new((w[0] + w[1]) / 2.0, y);
        let wind = path.winding(mid);
        let inside = if even_odd { wind % 2 != 0 } else { wind != 0 };
        if inside {
            out.push((w[0], w[1]));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom;

    #[test]
    fn shaping_advances_grow_with_the_string() {
        let f = fonts();
        let mut spec = TextSpec::new("i");
        spec.size = 64.0;
        let narrow = shape(f, &spec).total_advance();
        spec.text = "WWW".into();
        let wide = shape(f, &spec).total_advance();
        assert!(wide > narrow * 3.0, "W is wider than i: {wide} vs {narrow}");
    }

    #[test]
    fn glyph_outlines_are_real_contours_of_the_right_size() {
        let f = fonts();
        let mut spec = TextSpec::new("H");
        spec.size = 100.0;
        let (p, sub) = outline_block(f, &spec, (0.0, 0.0));
        assert!(sub.is_none(), "sans-serif maps to the embedded face without a warning");
        let b = geom::bbox(&p).expect("H has an outline");
        assert!(b.height() > 50.0 && b.height() < 100.0, "cap height plausible: {b:?}");
        assert!(geom::area(&p) > 1000.0);
    }

    #[test]
    fn an_unavailable_family_reports_the_substitution() {
        let f = fonts();
        let mut spec = TextSpec::new("x");
        spec.family = "Definitely Not Installed".into();
        let (_, sub) = outline_block(f, &spec, (0.0, 0.0));
        assert_eq!(sub.as_deref(), Some(FALLBACK_FAMILY));
    }

    #[test]
    fn a_box_wraps_words_onto_several_lines() {
        let f = fonts();
        let mut spec = TextSpec::new("alpha beta gamma delta epsilon");
        spec.size = 20.0;
        spec.r#box = Some(dpaint_core::doc::common::Rect::new(0.0, 0.0, 80.0, 200.0));
        let s = shape(f, &spec);
        assert!(s.lines.len() >= 3, "wrapped into {} lines", s.lines.len());
        assert!(s.lines.iter().all(|l| l.width <= 80.0 || l.glyphs.len() <= 1));
    }

    #[test]
    fn newlines_start_new_lines() {
        let f = fonts();
        let spec = TextSpec::new("a\nb\nc");
        assert_eq!(shape(fonts(), &spec).lines.len(), 3);
        let _ = f;
    }
}
