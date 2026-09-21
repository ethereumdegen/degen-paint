//! The render digest: what an agent reads instead of looking at the image.

use dpaint_core::doc::{Document, Rect};
use dpaint_core::{AssetStore, Color, DocId, Project, Result};
use dpaint_render::{encode, RenderOptions};
use image::RgbaImage;
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct Digest {
    pub document: String,
    pub kind: String,
    pub size: [u32; 2],
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dpi: Option<f32>,
    pub render_ms: u128,
    pub tree: Vec<NodeDigest>,
    pub histogram: Histogram,
    pub dominant_colors: Vec<DominantColor>,
    /// Fraction of the canvas with any opacity at all.
    pub alpha_coverage: f64,
    pub mean_color: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct NodeDigest {
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub type_name: String,
    pub depth: usize,
    /// World-space bounds of what this object actually paints, `[x, y, w, h]`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bbox: Option<[f64; 4]>,
    pub visible: bool,
    pub opacity: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blend: Option<String>,
    /// Fraction of its own bbox this object actually covers; 0.0 means it paints nothing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coverage: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mean_color: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// The family the text was actually laid out in.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub font: Option<String>,
    /// The family that was *asked* for, when it is not the one used — absent when the
    /// request was honoured. An agent cannot see that its brand face silently became the
    /// fallback, so the digest is where that gets said (PLAN §4, "fonts that fell back").
    #[serde(rename = "fontFallback", skip_serializing_if = "Option::is_none")]
    pub font_fallback: Option<String>,
    /// Contrast ratio of this object's paint against what is behind it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contrast_vs_backdrop: Option<f32>,
    /// Where this object came from, when it did not come from a human: the provider, model,
    /// prompt and cost an `ai.*` op recorded, or the sidecar an import read. Without it an
    /// agent that imports a take cannot verify through the digest *which* take it imported.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provenance: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Histogram {
    pub r: Vec<u32>,
    pub g: Vec<u32>,
    pub b: Vec<u32>,
    pub l: Vec<u32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DominantColor {
    pub color: String,
    pub fraction: f64,
}

#[derive(Debug, Clone)]
pub struct DigestOptions {
    /// Per-object bounds require rendering each object in isolation; skip for speed.
    pub per_object: bool,
    pub histogram_bins: usize,
    pub render: RenderOptions,
}

impl Default for DigestOptions {
    fn default() -> Self {
        Self {
            per_object: true,
            histogram_bins: 16,
            render: RenderOptions::default(),
        }
    }
}

pub fn digest(
    project: &Project,
    doc_id: &DocId,
    assets: &AssetStore,
    opts: &DigestOptions,
) -> Result<Digest> {
    // `web_time::Instant` is `std::time::Instant` everywhere except wasm32, where the std
    // clock is unimplemented and panics.
    let start = web_time::Instant::now();
    let pm = dpaint_render::render_document(project, doc_id, assets, &opts.render)?;
    let img = encode::to_rgba(&pm);
    let render_ms = start.elapsed().as_millis();

    let document = project.doc(doc_id)?;
    let tree = if opts.per_object {
        object_digests(project, doc_id, assets, opts, &img)?
    } else {
        shallow_tree(document)
    };

    Ok(Digest {
        document: doc_id.to_string(),
        kind: document.kind().as_str().to_string(),
        size: [img.width(), img.height()],
        dpi: document.as_raster().map(|d| d.dpi),
        render_ms,
        tree,
        histogram: histogram(&img, opts.histogram_bins),
        dominant_colors: dominant_colors(&img, 6),
        alpha_coverage: alpha_coverage(&img),
        mean_color: mean_color(&img).to_hex(),
    })
}

/// Bounds of the non-transparent region of an image, in pixels.
pub fn opaque_bbox(img: &RgbaImage) -> Option<[f64; 4]> {
    let (w, h) = img.dimensions();
    let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0u32, 0u32);
    let mut any = false;
    for y in 0..h {
        for x in 0..w {
            if img.get_pixel(x, y).0[3] > 0 {
                any = true;
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x);
                y1 = y1.max(y);
            }
        }
    }
    any.then(|| {
        [
            x0 as f64,
            y0 as f64,
            (x1 - x0 + 1) as f64,
            (y1 - y0 + 1) as f64,
        ]
    })
}

fn shallow_tree(document: &Document) -> Vec<NodeDigest> {
    dpaint_core::selector::candidates(document)
        .into_iter()
        .map(|c| NodeDigest {
            id: c.id,
            name: c.name,
            type_name: c.type_name,
            depth: c.depth,
            bbox: None,
            visible: c
                .attrs
                .get("visible")
                .and_then(|v| v.as_bool())
                .unwrap_or(true),
            opacity: c
                .attrs
                .get("opacity")
                .and_then(|v| v.as_f64())
                .unwrap_or(1.0) as f32,
            blend: c
                .attrs
                .get("blend")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            coverage: None,
            mean_color: None,
            text: c
                .attrs
                .get("text")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            // Resolved in `object_digests`, which is the only place that knows which face
            // the request actually landed on.
            font: None,
            font_fallback: None,
            contrast_vs_backdrop: None,
            provenance: c.attrs.get("provenance").cloned(),
        })
        .collect()
}

/// Render each object in isolation to learn what it actually paints. Expensive but exact:
/// it accounts for masks, effects, clipping and blend, which static inspection cannot.
fn object_digests(
    project: &Project,
    doc_id: &DocId,
    assets: &AssetStore,
    opts: &DigestOptions,
    full: &RgbaImage,
) -> Result<Vec<NodeDigest>> {
    let mut out = shallow_tree(project.doc(doc_id)?);
    let Some(_) = project.doc(doc_id)?.as_raster() else {
        // Vector and model objects: isolation rendering is raster-specific, so report the
        // structural tree and the composite statistics only.
        return Ok(out);
    };

    // One font database for the whole document rather than one per text layer: building it
    // loads every embedded and registered face, and a poster with twenty captions would
    // otherwise pay for that twenty times.
    let fonts = dpaint_raster::text::FontSet::new(project, assets);
    let raster_doc = project.raster(doc_id)?.clone();

    for node in out.iter_mut() {
        resolve_font(node, &raster_doc, &fonts);
        let mut probe = project.clone();
        let Ok(raster) = probe.raster_mut(doc_id) else {
            continue;
        };
        let ids: Vec<String> = dpaint_core::selector::candidates(&Document::Raster(raster.clone()))
            .into_iter()
            .map(|c| c.id)
            .collect();
        // Decide visibility before mutating: a layer inside a group only renders if its
        // ancestors stay visible too.
        let visibility: Vec<(String, bool)> = ids
            .iter()
            .map(|id| {
                (
                    id.clone(),
                    *id == node.id || is_ancestor_of(raster, id, &node.id),
                )
            })
            .collect();
        for (id, visible) in visibility {
            if let Some(l) = raster.layer_mut(&dpaint_core::LayerId::from(id)) {
                l.visible = visible;
            }
        }
        let Ok(pm) = dpaint_render::render_document(&probe, doc_id, assets, &opts.render) else {
            continue;
        };
        let img = encode::to_rgba(&pm);
        node.bbox = opaque_bbox(&img);
        if let Some(bb) = node.bbox {
            let area = bb[2] * bb[3];
            let painted = img.pixels().filter(|p| p.0[3] > 0).count() as f64;
            node.coverage = Some(if area > 0.0 { painted / area } else { 0.0 });
            node.mean_color = Some(mean_color(&img).to_hex());
            node.contrast_vs_backdrop = Some(contrast_in_region(full, &img, bb));
        } else {
            node.coverage = Some(0.0);
        }
    }
    Ok(out)
}

/// Record which face a text layer actually got.
///
/// The engine already decides this — `FontSet::pick` returns the family it resolved and
/// whether that was a fallback — but nothing used to carry the answer out to a caller who
/// cannot look at the pixels. A layout failure is left silent here: the render path
/// reports it as an error, and a digest is not the place to raise it a second time.
fn resolve_font(
    node: &mut NodeDigest,
    doc: &dpaint_core::RasterDoc,
    fonts: &dpaint_raster::text::FontSet,
) {
    use dpaint_core::doc::raster::LayerKind;

    let Some(layer) = doc.layer(&dpaint_core::LayerId::from(node.id.clone())) else {
        return;
    };
    let LayerKind::Text { spec, .. } = &layer.kind else {
        return;
    };
    let Ok(layout) = dpaint_raster::text::layout(spec, fonts) else {
        return;
    };
    node.font = Some(layout.used_family);
    if layout.fallback {
        node.font_fallback = Some(spec.family.clone());
    }
}

fn is_ancestor_of(doc: &dpaint_core::RasterDoc, candidate: &str, target: &str) -> bool {
    fn rec(layers: &[dpaint_core::doc::raster::Layer], candidate: &str, target: &str) -> bool {
        for l in layers {
            if let dpaint_core::doc::raster::LayerKind::Group { layers: inner } = &l.kind {
                let contains = dpaint_core::selector::candidates(&Document::Raster({
                    let mut d = dpaint_core::RasterDoc::new(
                        dpaint_core::DocId::from("probe"),
                        "probe",
                        1,
                        1,
                    );
                    d.layers = inner.clone();
                    d
                }))
                .iter()
                .any(|c| c.id == target);
                if l.id.as_str() == candidate && contains {
                    return true;
                }
                if rec(inner, candidate, target) {
                    return true;
                }
            }
        }
        false
    }
    rec(&doc.layers, candidate, target)
}

/// Contrast of the object's own pixels against the composite behind them.
fn contrast_in_region(full: &RgbaImage, isolated: &RgbaImage, bb: [f64; 4]) -> f32 {
    let (x0, y0) = (bb[0] as u32, bb[1] as u32);
    let (x1, y1) = ((bb[0] + bb[2]) as u32, (bb[1] + bb[3]) as u32);
    let mut fg = [0.0f64; 3];
    let mut bg = [0.0f64; 3];
    let (mut nf, mut nb) = (0.0f64, 0.0f64);
    for y in y0..y1.min(full.height()) {
        for x in x0..x1.min(full.width()) {
            let i = isolated.get_pixel(x, y).0;
            let f = full.get_pixel(x, y).0;
            if i[3] > 128 {
                for c in 0..3 {
                    fg[c] += i[c] as f64;
                }
                nf += 1.0;
            } else {
                for c in 0..3 {
                    bg[c] += f[c] as f64;
                }
                nb += 1.0;
            }
        }
    }
    if nf == 0.0 || nb == 0.0 {
        return 1.0;
    }
    let mk = |v: [f64; 3], n: f64| {
        Color::rgba(
            (v[0] / n / 255.0) as f32,
            (v[1] / n / 255.0) as f32,
            (v[2] / n / 255.0) as f32,
            1.0,
        )
    };
    mk(fg, nf).contrast_ratio(mk(bg, nb))
}

pub fn histogram(img: &RgbaImage, bins: usize) -> Histogram {
    let bins = bins.clamp(2, 256);
    let idx = |v: u8| (v as usize * bins / 256).min(bins - 1);
    let mut h = Histogram {
        r: vec![0; bins],
        g: vec![0; bins],
        b: vec![0; bins],
        l: vec![0; bins],
    };
    for p in img.pixels() {
        if p.0[3] == 0 {
            continue;
        }
        h.r[idx(p.0[0])] += 1;
        h.g[idx(p.0[1])] += 1;
        h.b[idx(p.0[2])] += 1;
        let lum = (0.2126 * p.0[0] as f32 + 0.7152 * p.0[1] as f32 + 0.0722 * p.0[2] as f32) as u8;
        h.l[idx(lum)] += 1;
    }
    h
}

/// Dominant colors by 4-bit-per-channel quantization: enough to answer "is this poster
/// mostly blue" without the nondeterminism of k-means.
pub fn dominant_colors(img: &RgbaImage, n: usize) -> Vec<DominantColor> {
    let mut buckets: std::collections::BTreeMap<[u8; 3], u64> = Default::default();
    let mut total = 0u64;
    for p in img.pixels() {
        if p.0[3] < 8 {
            continue;
        }
        let q = [p.0[0] >> 4, p.0[1] >> 4, p.0[2] >> 4];
        *buckets.entry(q).or_default() += 1;
        total += 1;
    }
    if total == 0 {
        return Vec::new();
    }
    let mut v: Vec<_> = buckets.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    v.into_iter()
        .take(n)
        .map(|(q, count)| DominantColor {
            color: Color::rgb8(q[0] << 4 | 8, q[1] << 4 | 8, q[2] << 4 | 8).to_hex(),
            fraction: count as f64 / total as f64,
        })
        .collect()
}

pub fn alpha_coverage(img: &RgbaImage) -> f64 {
    let n = (img.width() * img.height()) as f64;
    if n == 0.0 {
        return 0.0;
    }
    img.pixels().filter(|p| p.0[3] > 0).count() as f64 / n
}

pub fn mean_color(img: &RgbaImage) -> Color {
    let mut acc = [0.0f64; 3];
    let mut n = 0.0;
    for p in img.pixels() {
        if p.0[3] == 0 {
            continue;
        }
        for (a, v) in acc.iter_mut().zip(p.0.iter()).take(3) {
            *a += *v as f64;
        }
        n += 1.0;
    }
    if n == 0.0 {
        return Color::TRANSPARENT;
    }
    Color::rgba(
        (acc[0] / n / 255.0) as f32,
        (acc[1] / n / 255.0) as f32,
        (acc[2] / n / 255.0) as f32,
        1.0,
    )
}

/// Region helper shared with lint.
pub fn rect_of(bbox: [f64; 4]) -> Rect {
    Rect::new(bbox[0], bbox[1], bbox[2], bbox[3])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn img(w: u32, h: u32, f: impl Fn(u32, u32) -> [u8; 4]) -> RgbaImage {
        let mut i = RgbaImage::new(w, h);
        for y in 0..h {
            for x in 0..w {
                i.put_pixel(x, y, image::Rgba(f(x, y)));
            }
        }
        i
    }

    #[test]
    fn bbox_bounds_exactly_the_painted_region() {
        let i = img(16, 16, |x, y| {
            if (4..8).contains(&x) && (2..5).contains(&y) {
                [255, 0, 0, 255]
            } else {
                [0, 0, 0, 0]
            }
        });
        assert_eq!(opaque_bbox(&i), Some([4.0, 2.0, 4.0, 3.0]));
        assert!(opaque_bbox(&img(4, 4, |_, _| [0, 0, 0, 0])).is_none());
    }

    #[test]
    fn coverage_and_mean_ignore_transparent_pixels() {
        let i = img(10, 10, |x, _| {
            if x < 5 {
                [200, 100, 50, 255]
            } else {
                [0, 0, 0, 0]
            }
        });
        assert!((alpha_coverage(&i) - 0.5).abs() < 1e-9);
        assert_eq!(mean_color(&i).to_hex(), "#c86432");
    }

    #[test]
    fn dominant_colors_rank_by_area_and_sum_to_the_opaque_fraction() {
        let i = img(10, 10, |x, _| {
            if x < 7 {
                [250, 10, 10, 255]
            } else {
                [10, 10, 250, 255]
            }
        });
        let d = dominant_colors(&i, 4);
        assert_eq!(d.len(), 2);
        assert!((d[0].fraction - 0.7).abs() < 1e-9, "{:?}", d);
        assert!(
            d[0].color.starts_with("#f"),
            "the majority color should be the red, got {}",
            d[0].color
        );
    }

    #[test]
    fn the_histogram_counts_only_opaque_pixels() {
        let i = img(4, 4, |x, _| {
            if x == 0 {
                [255, 255, 255, 255]
            } else {
                [0, 0, 0, 0]
            }
        });
        let h = histogram(&i, 4);
        assert_eq!(h.l.iter().sum::<u32>(), 4);
        assert_eq!(h.l[3], 4, "white must land in the top bin");
    }
}
