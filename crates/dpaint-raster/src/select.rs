//! Selections: an 8-bit coverage field plus the vector outline it came from.
//!
//! A selection is the scope of every raster edit. Ops never write pixels directly; they
//! produce a candidate buffer and hand it to [`composite_through`], which keeps pixels
//! outside the selection bit-identical.

use crate::canvas::{decode_gray_png, encode_gray_png, Canvas};
use crate::geom;
use dpaint_core::color::linear_to_srgb;
use dpaint_core::doc::common::{FillRule, Rect};
use dpaint_core::doc::raster::Selection;
use dpaint_core::kurbo::Affine;
use dpaint_core::{AssetStore, Error, RasterDoc, Result};

/// Per-pixel selection coverage in device space.
#[derive(Debug, Clone, PartialEq)]
pub struct SelMask {
    pub width: u32,
    pub height: u32,
    pub cov: Vec<f32>,
}

impl SelMask {
    pub fn new(width: u32, height: u32, fill: f32) -> Self {
        Self {
            width,
            height,
            cov: vec![fill; width as usize * height as usize],
        }
    }

    pub fn from_cov(width: u32, height: u32, cov: Vec<f32>) -> Self {
        debug_assert_eq!(cov.len(), width as usize * height as usize);
        Self { width, height, cov }
    }

    #[inline]
    pub fn at(&self, x: u32, y: u32) -> f32 {
        self.cov[y as usize * self.width as usize + x as usize]
    }

    pub fn invert(&mut self) {
        for v in self.cov.iter_mut() {
            *v = 1.0 - *v;
        }
    }

    /// Separable gaussian on the coverage field — `select.feather` and `Selection::feather`.
    pub fn feather(&mut self, sigma: f32) {
        if sigma <= 0.0 {
            return;
        }
        let k = crate::filters::gauss_kernel(sigma);
        let r = (k.len() / 2) as i64;
        let (w, h) = (self.width as i64, self.height as i64);
        let sample = |buf: &[f32], x: i64, y: i64| -> f32 {
            let x = x.clamp(0, w - 1);
            let y = y.clamp(0, h - 1);
            buf[(y * w + x) as usize]
        };
        let mut tmp = vec![0.0f32; self.cov.len()];
        for y in 0..h {
            for x in 0..w {
                let mut acc = 0.0;
                for (i, kv) in k.iter().enumerate() {
                    acc += sample(&self.cov, x + i as i64 - r, y) * kv;
                }
                tmp[(y * w + x) as usize] = acc;
            }
        }
        for y in 0..h {
            for x in 0..w {
                let mut acc = 0.0;
                for (i, kv) in k.iter().enumerate() {
                    acc += sample(&tmp, x, y + i as i64 - r) * kv;
                }
                self.cov[(y * w + x) as usize] = acc.clamp(0.0, 1.0);
            }
        }
    }

    /// Disk max filter (`grow`) or min filter (`shrink`).
    pub fn morph(&mut self, radius: u32, grow: bool) {
        if radius == 0 {
            return;
        }
        let r = radius as i64;
        let r2 = (r * r) as f32;
        let (w, h) = (self.width as i64, self.height as i64);
        let src = self.cov.clone();
        for y in 0..h {
            for x in 0..w {
                let mut best = src[(y * w + x) as usize];
                for dy in -r..=r {
                    for dx in -r..=r {
                        if (dx * dx + dy * dy) as f32 > r2 {
                            continue;
                        }
                        let sx = (x + dx).clamp(0, w - 1);
                        let sy = (y + dy).clamp(0, h - 1);
                        let v = src[(sy * w + sx) as usize];
                        if (grow && v > best) || (!grow && v < best) {
                            best = v;
                        }
                    }
                }
                self.cov[(y * w + x) as usize] = best;
            }
        }
    }

    pub fn bounds(&self) -> Rect {
        let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0u32, 0u32);
        for y in 0..self.height {
            for x in 0..self.width {
                if self.at(x, y) > 0.002 {
                    x0 = x0.min(x);
                    y0 = y0.min(y);
                    x1 = x1.max(x);
                    y1 = y1.max(y);
                }
            }
        }
        if x0 == u32::MAX {
            Rect::default()
        } else {
            Rect::new(
                x0 as f64,
                y0 as f64,
                (x1 - x0 + 1) as f64,
                (y1 - y0 + 1) as f64,
            )
        }
    }

    pub fn area(&self) -> f64 {
        self.cov.iter().map(|v| *v as f64).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.cov.iter().all(|v| *v <= 0.002)
    }

    pub fn to_png(&self) -> Result<Vec<u8>> {
        encode_gray_png(self.width, self.height, &self.cov)
    }

    /// Rescale to a different device size (used when rendering at a non-unit scale).
    pub fn resized(&self, w: u32, h: u32) -> Self {
        if w == self.width && h == self.height {
            return self.clone();
        }
        let mut out = SelMask::new(w, h, 0.0);
        for y in 0..h {
            let sy = ((y as f32 + 0.5) * self.height as f32 / h as f32).floor() as u32;
            let sy = sy.min(self.height - 1);
            for x in 0..w {
                let sx = ((x as f32 + 0.5) * self.width as f32 / w as f32).floor() as u32;
                let sx = sx.min(self.width - 1);
                out.cov[y as usize * w as usize + x as usize] = self.at(sx, sy);
            }
        }
        out
    }
}

/// Rasterize a selection outline at `scale`.
pub fn mask_from_d(d: &str, w: u32, h: u32, scale: f64) -> Result<SelMask> {
    let path = geom::parse_d(d)?;
    let sk = geom::to_sk(&path, Affine::scale(scale))
        .ok_or_else(|| Error::DegenerateGeometry("selection path is empty".into()))?;
    Ok(SelMask::from_cov(
        w,
        h,
        geom::fill_coverage(&sk, w, h, FillRule::Nonzero),
    ))
}

/// Build the effective selection mask for a document at a given device size.
/// Returns `None` when there is no selection, meaning "the whole canvas".
pub fn resolve(
    doc: &RasterDoc,
    assets: &AssetStore,
    w: u32,
    h: u32,
    scale: f64,
) -> Result<Option<SelMask>> {
    let Some(sel) = &doc.selection else {
        return Ok(None);
    };
    let mut mask = match (&sel.mask, &sel.d) {
        (Some(asset), _) => {
            let (mw, mh, cov) = decode_gray_png(&assets.get(asset)?)?;
            SelMask::from_cov(mw, mh, cov).resized(w, h)
        }
        (None, Some(d)) => mask_from_d(d, w, h, scale)?,
        (None, None) => return Ok(None),
    };
    if sel.feather > 0.0 {
        mask.feather((sel.feather * scale) as f32);
    }
    if sel.inverted {
        mask.invert();
    }
    Ok(Some(mask))
}

/// Store a vector outline as the current selection, keeping it resolution-independent.
pub fn store_outline(doc: &mut RasterDoc, d: String, bounds: Rect, feather: f64) {
    doc.selection = Some(Selection {
        d: Some(d),
        mask: None,
        feather,
        inverted: false,
        bounds,
    });
}

/// Blend `new` over `old` through the selection. Pixels with zero coverage are copied
/// verbatim, so a scoped filter provably cannot touch anything outside the selection.
pub fn composite_through(
    old: &Canvas,
    new: &Canvas,
    sel: Option<&SelMask>,
    offset: (i64, i64),
) -> Canvas {
    let Some(sel) = sel else { return new.clone() };
    let mut out = old.clone();
    for y in 0..old.height {
        for x in 0..old.width {
            let sx = x as i64 + offset.0;
            let sy = y as i64 + offset.1;
            let m = if sx < 0 || sy < 0 || sx >= sel.width as i64 || sy >= sel.height as i64 {
                0.0
            } else {
                sel.at(sx as u32, sy as u32)
            };
            if m <= 0.0 {
                continue;
            }
            let i = old.idx(x, y);
            if m >= 1.0 {
                out.data[i..i + 4].copy_from_slice(&new.data[i..i + 4]);
                continue;
            }
            for c in 0..4 {
                let o = old.data[i + c];
                out.data[i + c] = o + (new.data[i + c] - o) * m;
            }
        }
    }
    out
}

/// Display-space RGB distance, normalized so 1.0 is black-to-white on all three channels.
#[inline]
fn color_distance(a: [f32; 4], b: [f32; 4]) -> f32 {
    let mut acc = 0.0;
    for c in 0..3 {
        let d = linear_to_srgb(a[c].clamp(0.0, 1.0)) - linear_to_srgb(b[c].clamp(0.0, 1.0));
        acc += d * d;
    }
    let da = a[3] - b[3];
    (acc + da * da).sqrt() / 2.0
}

/// Magic wand: flood fill from a seed with a tolerance, optionally over the whole image
/// instead of just the connected region.
pub fn wand(c: &Canvas, x: u32, y: u32, tolerance: f32, contiguous: bool) -> SelMask {
    let mut out = SelMask::new(c.width, c.height, 0.0);
    let seed = c.straight(c.idx(x, y));
    if !contiguous {
        for py in 0..c.height {
            for px in 0..c.width {
                let d = color_distance(seed, c.straight(c.idx(px, py)));
                if d <= tolerance {
                    out.cov[py as usize * c.width as usize + px as usize] = 1.0;
                }
            }
        }
        return out;
    }
    let w = c.width as usize;
    let mut stack = vec![(x, y)];
    let mut seen = vec![false; c.pixel_count()];
    seen[y as usize * w + x as usize] = true;
    while let Some((cx, cy)) = stack.pop() {
        let idx = cy as usize * w + cx as usize;
        if color_distance(seed, c.straight(idx * 4)) > tolerance {
            continue;
        }
        out.cov[idx] = 1.0;
        let neighbours = [
            (cx.wrapping_sub(1), cy),
            (cx + 1, cy),
            (cx, cy.wrapping_sub(1)),
            (cx, cy + 1),
        ];
        for (nx, ny) in neighbours {
            if nx >= c.width || ny >= c.height {
                continue;
            }
            let ni = ny as usize * w + nx as usize;
            if seen[ni] {
                continue;
            }
            seen[ni] = true;
            stack.push((nx, ny));
        }
    }
    out
}

/// Select every pixel within `tolerance` of a target color, with a soft edge over `fuzz`.
pub fn color_range(c: &Canvas, target: [f32; 4], tolerance: f32, fuzz: f32) -> SelMask {
    let mut out = SelMask::new(c.width, c.height, 0.0);
    for i in 0..c.pixel_count() {
        let d = color_distance(target, c.straight(i * 4));
        out.cov[i] = if d <= tolerance {
            1.0
        } else if fuzz > 0.0 && d < tolerance + fuzz {
            1.0 - (d - tolerance) / fuzz
        } else {
            0.0
        };
    }
    out
}

pub fn from_alpha(c: &Canvas) -> SelMask {
    SelMask::from_cov(
        c.width,
        c.height,
        (0..c.pixel_count()).map(|i| c.data[i * 4 + 3]).collect(),
    )
}

/// Trace the 0.5 iso-contour of a coverage field into SVG path data.
///
/// Marching squares on the pixel grid: each cell contributes the edges implied by its
/// four corner states, then the segments are chained into closed rings. Good enough to
/// convert a wand selection into an editable vector outline, which is the point.
pub fn to_path_d(mask: &SelMask) -> String {
    let (w, h) = (mask.width as i64, mask.height as i64);
    let inside = |x: i64, y: i64| -> bool {
        if x < 0 || y < 0 || x >= w || y >= h {
            false
        } else {
            mask.at(x as u32, y as u32) >= 0.5
        }
    };
    // Collect boundary edges, oriented so the interior is on the left.
    let mut edges: Vec<((i64, i64), (i64, i64))> = Vec::new();
    for y in 0..=h {
        for x in 0..=w {
            let c = inside(x, y);
            if c != inside(x, y - 1) {
                // Horizontal edge on the top of this pixel.
                if c {
                    edges.push(((x, y), (x + 1, y)));
                } else {
                    edges.push(((x + 1, y), (x, y)));
                }
            }
            if c != inside(x - 1, y) {
                if c {
                    edges.push(((x, y + 1), (x, y)));
                } else {
                    edges.push(((x, y), (x, y + 1)));
                }
            }
        }
    }
    let mut starts: std::collections::BTreeMap<(i64, i64), Vec<(i64, i64)>> = Default::default();
    for (a, b) in edges {
        starts.entry(a).or_default().push(b);
    }
    let mut d = String::new();
    while let Some((&start, _)) = starts.iter().next() {
        let mut ring = vec![start];
        let mut cur = start;
        loop {
            let Some(nexts) = starts.get_mut(&cur) else {
                break;
            };
            let Some(next) = nexts.pop() else {
                starts.remove(&cur);
                break;
            };
            if nexts.is_empty() {
                starts.remove(&cur);
            }
            if next == start {
                break;
            }
            ring.push(next);
            cur = next;
        }
        if ring.len() < 3 {
            continue;
        }
        // Drop collinear midpoints so the emitted path is a clean polygon.
        let mut pts: Vec<(i64, i64)> = Vec::with_capacity(ring.len());
        for i in 0..ring.len() {
            let p = ring[i];
            let a = ring[(i + ring.len() - 1) % ring.len()];
            let b = ring[(i + 1) % ring.len()];
            let d1 = (p.0 - a.0, p.1 - a.1);
            let d2 = (b.0 - p.0, b.1 - p.1);
            if d1.0 * d2.1 - d1.1 * d2.0 != 0 {
                pts.push(p);
            }
        }
        if pts.len() < 3 {
            continue;
        }
        d.push_str(&format!("M{} {}", pts[0].0, pts[0].1));
        for p in &pts[1..] {
            d.push_str(&format!(" L{} {}", p.0, p.1));
        }
        d.push_str(" Z");
    }
    d
}

#[cfg(test)]
mod tests {
    use super::*;

    fn two_tone(w: u32, h: u32) -> Canvas {
        let mut c = Canvas::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let v = if x < w / 2 { 0.0 } else { 1.0 };
                c.set(x, y, [v, v, v, 1.0]);
            }
        }
        c
    }

    #[test]
    fn wand_selects_exactly_one_of_two_regions() {
        let c = two_tone(10, 4);
        let m = wand(&c, 1, 1, 0.05, true);
        assert_eq!(m.area() as u32, 20, "left half only");
        for y in 0..4 {
            for x in 0..10 {
                assert_eq!(m.at(x, y), if x < 5 { 1.0 } else { 0.0 }, "at {x},{y}");
            }
        }
    }

    #[test]
    fn traced_contour_of_a_rectangle_is_a_closed_quad() {
        let mut m = SelMask::new(8, 8, 0.0);
        for y in 2..6 {
            for x in 2..6 {
                m.cov[y * 8 + x] = 1.0;
            }
        }
        let d = to_path_d(&m);
        assert_eq!(d.matches('Z').count(), 1, "one ring: {d}");
        assert_eq!(
            d.matches('L').count(),
            3,
            "a rectangle has four corners: {d}"
        );
        let path = geom::parse_d(&d).unwrap();
        let bb = dpaint_core::kurbo::Shape::bounding_box(&path);
        assert_eq!((bb.x0, bb.y0, bb.x1, bb.y1), (2.0, 2.0, 6.0, 6.0));
    }
}
