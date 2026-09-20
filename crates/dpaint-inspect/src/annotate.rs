//! Annotated previews: numbered bounding boxes with stable ids, so a vision model can say
//! "#3 overlaps #7" and mean something an op can act on.

use crate::digest::Digest;
use dpaint_core::{Color, Result};
use tiny_skia::{Paint, PathBuilder, Pixmap, Stroke, Transform};

/// Distinct, high-contrast marker colors, cycled by index.
const PALETTE: [&str; 6] = [
    "#ff3b30", "#34c759", "#0a84ff", "#ff9f0a", "#bf5af2", "#00c7be",
];

/// Draw each measured object's bbox with its index badge. Returns the legend an agent reads
/// alongside the image: index -> selector.
pub fn annotate(base: &Pixmap, digest: &Digest) -> Result<(Pixmap, Vec<Legend>)> {
    let mut pm = base.clone();
    let mut legend = Vec::new();

    for (i, node) in digest.tree.iter().filter(|n| n.bbox.is_some()).enumerate() {
        let bb = node.bbox.expect("filtered to Some above");
        let color = Color::parse(PALETTE[i % PALETTE.len()]).expect("palette entries are valid");
        let rgba = color.to_rgba8();

        let mut paint = Paint::default();
        paint.set_color_rgba8(rgba[0], rgba[1], rgba[2], 255);
        paint.anti_alias = true;

        let mut pb = PathBuilder::new();
        pb.push_rect(
            tiny_skia::Rect::from_xywh(
                bb[0] as f32,
                bb[1] as f32,
                bb[2].max(1.0) as f32,
                bb[3].max(1.0) as f32,
            )
            .unwrap_or_else(|| tiny_skia::Rect::from_xywh(0.0, 0.0, 1.0, 1.0).expect("unit rect")),
        );
        if let Some(path) = pb.finish() {
            pm.stroke_path(
                &path,
                &paint,
                &Stroke {
                    width: 2.0,
                    ..Default::default()
                },
                Transform::identity(),
                None,
            );
        }

        // Badge: a filled square whose size encodes nothing, only its color and position
        // matter; the legend carries the mapping.
        let badge =
            tiny_skia::Rect::from_xywh(bb[0] as f32, (bb[1] - 14.0).max(0.0) as f32, 14.0, 14.0);
        if let Some(r) = badge {
            pm.fill_rect(r, &paint, Transform::identity(), None);
        }

        legend.push(Legend {
            index: i + 1,
            selector: format!("#{}", node.id),
            name: node.name.clone(),
            type_name: node.type_name.clone(),
            color: color.to_hex(),
            bbox: bb,
        });
    }

    Ok((pm, legend))
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Legend {
    pub index: usize,
    /// Feed this straight back into an op.
    pub selector: String,
    pub name: String,
    #[serde(rename = "type")]
    pub type_name: String,
    pub color: String,
    pub bbox: [f64; 4],
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::digest::{Digest, Histogram, NodeDigest};

    fn digest_with(boxes: &[(&str, [f64; 4])]) -> Digest {
        Digest {
            document: "doc_main".into(),
            kind: "raster".into(),
            size: [64, 64],
            dpi: Some(72.0),
            render_ms: 0,
            tree: boxes
                .iter()
                .map(|(id, bb)| NodeDigest {
                    id: (*id).into(),
                    name: (*id).into(),
                    type_name: "pixel".into(),
                    depth: 0,
                    bbox: Some(*bb),
                    visible: true,
                    opacity: 1.0,
                    blend: None,
                    coverage: Some(1.0),
                    mean_color: None,
                    text: None,
                    contrast_vs_backdrop: None,
                })
                .collect(),
            histogram: Histogram {
                r: vec![],
                g: vec![],
                b: vec![],
                l: vec![],
            },
            dominant_colors: vec![],
            alpha_coverage: 1.0,
            mean_color: "#ffffff".into(),
        }
    }

    #[test]
    fn annotation_draws_on_the_image_and_returns_actionable_selectors() {
        let base = Pixmap::new(64, 64).unwrap();
        let d = digest_with(&[
            ("lyr_a", [8.0, 20.0, 20.0, 20.0]),
            ("lyr_b", [40.0, 40.0, 10.0, 10.0]),
        ]);
        let (out, legend) = annotate(&base, &d).unwrap();

        assert_eq!(legend.len(), 2);
        assert_eq!(legend[0].selector, "#lyr_a");
        assert_eq!(legend[0].index, 1);
        assert_ne!(
            legend[0].color, legend[1].color,
            "adjacent boxes must be distinguishable"
        );

        let painted = out.pixels().iter().filter(|p| p.alpha() > 0).count();
        assert!(painted > 0, "the annotation must actually mark the image");
        assert_eq!(
            base.pixels().iter().filter(|p| p.alpha() > 0).count(),
            0,
            "the base must not be mutated"
        );
    }

    #[test]
    fn objects_without_bounds_are_skipped_rather_than_drawn_at_the_origin() {
        let base = Pixmap::new(32, 32).unwrap();
        let mut d = digest_with(&[("lyr_a", [1.0, 1.0, 4.0, 4.0])]);
        d.tree[0].bbox = None;
        let (_, legend) = annotate(&base, &d).unwrap();
        assert!(legend.is_empty());
    }
}
