//! Lint: the mistakes a blind operator makes, encoded as checks with actionable selectors.

use crate::digest::{digest, Digest, DigestOptions};
use dpaint_core::doc::{raster::LayerKind, Document};
use dpaint_core::{AssetStore, DocId, Project, Result};
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Info,
    Warn,
    Error,
}

impl std::str::FromStr for Severity {
    type Err = dpaint_core::Error;
    fn from_str(s: &str) -> Result<Self> {
        match s {
            "info" => Ok(Severity::Info),
            "warn" | "warning" => Ok(Severity::Warn),
            "error" => Ok(Severity::Error),
            other => Err(dpaint_core::Error::Invalid(format!(
                "unknown severity '{other}' (expected info, warn or error)"
            ))),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Finding {
    pub rule: &'static str,
    pub severity: Severity,
    pub document: String,
    /// Selector an agent can feed straight back into an op.
    pub target: String,
    pub detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub findings: Vec<Finding>,
    pub errors: usize,
    pub warnings: usize,
}

impl Report {
    pub fn worst(&self) -> Option<Severity> {
        self.findings.iter().map(|f| f.severity).max()
    }
}

/// WCAG AA for large text.
const MIN_CONTRAST: f32 = 4.5;
/// Below this, print is unreadable.
const MIN_POINT_SIZE: f64 = 6.0;

pub fn lint_document(
    project: &Project,
    doc_id: &DocId,
    assets: &AssetStore,
    digest_opts: &DigestOptions,
) -> Result<Vec<Finding>> {
    let mut out = Vec::new();
    let document = project.doc(doc_id)?;
    let d = digest(project, doc_id, assets, digest_opts)?;

    match document {
        Document::Raster(raster) => {
            let canvas = [d.size[0] as f64, d.size[1] as f64];
            let bleed = raster.guides.bleed.max(raster.guides.safe);

            for node in &d.tree {
                let sel = format!("#{}", node.id);

                if node.visible && node.opacity > 0.0 && node.coverage == Some(0.0) {
                    out.push(Finding {
                        rule: "invisible-layer",
                        severity: Severity::Warn,
                        document: doc_id.to_string(),
                        target: sel.clone(),
                        detail: "layer is enabled but paints nothing".into(),
                        value: None,
                        required: None,
                    });
                }

                let Some(bb) = node.bbox else { continue };
                let (x0, y0, x1, y1) = (bb[0], bb[1], bb[0] + bb[2], bb[1] + bb[3]);

                if x1 <= 0.0 || y1 <= 0.0 || x0 >= canvas[0] || y0 >= canvas[1] {
                    out.push(Finding {
                        rule: "off-canvas",
                        severity: Severity::Error,
                        document: doc_id.to_string(),
                        target: sel.clone(),
                        detail: format!(
                            "bbox {bb:?} lies outside the {}x{} canvas",
                            canvas[0], canvas[1]
                        ),
                        value: None,
                        required: None,
                    });
                } else if x0 < 0.0 || y0 < 0.0 || x1 > canvas[0] || y1 > canvas[1] {
                    out.push(Finding {
                        rule: "clipped",
                        severity: Severity::Warn,
                        document: doc_id.to_string(),
                        target: sel.clone(),
                        detail: "content extends past the canvas and will be cut".into(),
                        value: None,
                        required: None,
                    });
                } else if bleed > 0.0 {
                    let margin = x0.min(y0).min(canvas[0] - x1).min(canvas[1] - y1);
                    if margin < bleed {
                        out.push(Finding {
                            rule: "near-edge",
                            severity: Severity::Warn,
                            document: doc_id.to_string(),
                            target: sel.clone(),
                            detail: format!("{margin:.0}px from the trim edge"),
                            value: Some(margin),
                            required: Some(bleed),
                        });
                    }
                }

                if node.type_name == "text" {
                    if let Some(c) = node.contrast_vs_backdrop {
                        if c < MIN_CONTRAST {
                            out.push(Finding {
                                rule: "low-contrast",
                                severity: Severity::Error,
                                document: doc_id.to_string(),
                                target: sel.clone(),
                                detail: "text fails WCAG AA against what is behind it".into(),
                                value: Some(c as f64),
                                required: Some(MIN_CONTRAST as f64),
                            });
                        }
                    }
                    if let Some(layer) = raster.layer(&dpaint_core::LayerId::from(node.id.clone()))
                    {
                        if let LayerKind::Text { spec, .. } = &layer.kind {
                            let points = spec.size * 72.0 / raster.dpi.max(1.0) as f64;
                            if points < MIN_POINT_SIZE {
                                out.push(Finding {
                                    rule: "tiny-type",
                                    severity: Severity::Warn,
                                    document: doc_id.to_string(),
                                    target: sel.clone(),
                                    detail: format!(
                                        "{points:.1}pt at {}dpi is below the readable floor",
                                        raster.dpi
                                    ),
                                    value: Some(points),
                                    required: Some(MIN_POINT_SIZE),
                                });
                            }
                            if let Some(b) = spec.r#box {
                                if bb[2] > b.w() + 1.0 || bb[3] > b.h() + 1.0 {
                                    out.push(Finding {
                                        rule: "text-overflow",
                                        severity: Severity::Error,
                                        document: doc_id.to_string(),
                                        target: sel.clone(),
                                        detail: format!(
                                            "rendered {:.0}x{:.0} overflows its {:.0}x{:.0} box",
                                            bb[2],
                                            bb[3],
                                            b.w(),
                                            b.h()
                                        ),
                                        value: None,
                                        required: None,
                                    });
                                }
                            }
                        }
                        if let LayerKind::Pixel { asset, .. } = &layer.kind {
                            check_upscale(assets, asset, bb, doc_id, &sel, &mut out);
                        }
                    }
                }

                if let Some(layer) = raster.layer(&dpaint_core::LayerId::from(node.id.clone())) {
                    if let LayerKind::Pixel { asset, .. } = &layer.kind {
                        check_upscale(assets, asset, bb, doc_id, &sel, &mut out);
                    }
                }
            }

            // Text on text: two text objects whose painted regions intersect.
            let texts: Vec<_> = d
                .tree
                .iter()
                .filter(|n| n.type_name == "text" && n.bbox.is_some())
                .collect();
            for (i, a) in texts.iter().enumerate() {
                for b in texts.iter().skip(i + 1) {
                    let (ra, rb) = (
                        crate::digest::rect_of(a.bbox.unwrap()),
                        crate::digest::rect_of(b.bbox.unwrap()),
                    );
                    if ra.intersects(rb) {
                        out.push(Finding {
                            rule: "text-collision",
                            severity: Severity::Error,
                            document: doc_id.to_string(),
                            target: format!("#{}", a.id),
                            detail: format!("overlaps text #{}", b.id),
                            value: None,
                            required: None,
                        });
                    }
                }
            }

            if raster.layers.is_empty() {
                out.push(Finding {
                    rule: "empty-document",
                    severity: Severity::Warn,
                    document: doc_id.to_string(),
                    target: doc_id.to_string(),
                    detail: "document has no layers".into(),
                    value: None,
                    required: None,
                });
            }
        }

        Document::Vector(vector) => {
            if vector.objects.is_empty() {
                out.push(Finding {
                    rule: "empty-document",
                    severity: Severity::Warn,
                    document: doc_id.to_string(),
                    target: doc_id.to_string(),
                    detail: "document has no objects".into(),
                    value: None,
                    required: None,
                });
            }
            for o in vector.walk() {
                let paints = !matches!(o.fill, dpaint_core::doc::Paint::None) || o.stroke.is_some();
                if o.visible
                    && !paints
                    && !matches!(o.kind, dpaint_core::doc::vector::VKind::Group { .. })
                {
                    out.push(Finding {
                        rule: "invisible-layer",
                        severity: Severity::Warn,
                        document: doc_id.to_string(),
                        target: format!("#{}", o.id),
                        detail: "object has neither fill nor stroke".into(),
                        value: None,
                        required: None,
                    });
                }
            }
        }

        Document::Model(model) => {
            for mesh in &model.meshes {
                let Ok(data) = dpaint_model3d::build_mesh(project, doc_id, &mesh.id, assets) else {
                    out.push(Finding {
                        rule: "mesh-build-failed",
                        severity: Severity::Error,
                        document: doc_id.to_string(),
                        target: format!("#{}", mesh.id),
                        detail: "mesh recipe could not be evaluated".into(),
                        value: None,
                        required: None,
                    });
                    continue;
                };
                let sel = format!("#{}", mesh.id);

                if data.indices.is_empty() || data.positions.is_empty() {
                    out.push(Finding {
                        rule: "empty-mesh",
                        severity: Severity::Error,
                        document: doc_id.to_string(),
                        target: sel.clone(),
                        detail: "mesh has no geometry".into(),
                        value: None,
                        required: None,
                    });
                    continue;
                }

                let open = boundary_edges(&data.indices);
                if open > 0 {
                    out.push(Finding {
                        rule: "non-manifold",
                        severity: Severity::Warn,
                        document: doc_id.to_string(),
                        target: sel.clone(),
                        detail: format!("{open} edges are not shared by exactly two triangles"),
                        value: Some(open as f64),
                        required: Some(0.0),
                    });
                }

                let textured = model.materials.iter().any(|m| !m.textures.is_empty());
                if textured && data.uvs.is_empty() {
                    out.push(Finding {
                        rule: "missing-uv",
                        severity: Severity::Error,
                        document: doc_id.to_string(),
                        target: sel,
                        detail: "a textured material is in play but this mesh has no UVs".into(),
                        value: None,
                        required: None,
                    });
                }
            }
            if model.nodes.is_empty() {
                out.push(Finding {
                    rule: "empty-document",
                    severity: Severity::Warn,
                    document: doc_id.to_string(),
                    target: doc_id.to_string(),
                    detail: "scene has no nodes".into(),
                    value: None,
                    required: None,
                });
            }
        }
    }

    Ok(out)
}

fn check_upscale(
    assets: &AssetStore,
    asset: &dpaint_core::AssetRef,
    bb: [f64; 4],
    doc_id: &DocId,
    sel: &str,
    out: &mut Vec<Finding>,
) {
    let Ok(bytes) = assets.get(asset) else { return };
    let Ok(img) = image::load_from_memory(&bytes) else {
        return;
    };
    let native = img.width() as f64;
    if native > 0.0 && bb[2] > native * 1.25 {
        out.push(Finding {
            rule: "upscaled-asset",
            severity: Severity::Warn,
            document: doc_id.to_string(),
            target: sel.to_string(),
            detail: format!("{}px source drawn at {:.0}px wide", native, bb[2]),
            value: Some(bb[2] / native),
            required: Some(1.0),
        });
    }
}

/// Edges used by exactly one triangle. A closed solid has none.
fn boundary_edges(indices: &[u32]) -> usize {
    let mut counts: std::collections::BTreeMap<(u32, u32), usize> = Default::default();
    for t in indices.chunks_exact(3) {
        for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
            let key = if a < b { (a, b) } else { (b, a) };
            *counts.entry(key).or_default() += 1;
        }
    }
    counts.values().filter(|c| **c != 2).count()
}

/// Lint every document in the project.
pub fn lint_project(
    project: &Project,
    assets: &AssetStore,
    digest_opts: &DigestOptions,
) -> Result<Report> {
    let mut findings = Vec::new();
    for id in project.documents.keys() {
        findings.extend(lint_document(project, id, assets, digest_opts)?);
    }

    // Blobs nothing points at: cheap to check, annoying to discover later.
    let referenced = project.referenced_assets();
    if let Ok(on_disk) = assets.list() {
        for a in on_disk {
            if !referenced.contains(&a) {
                findings.push(Finding {
                    rule: "unreferenced-asset",
                    severity: Severity::Info,
                    document: String::new(),
                    target: a.to_string(),
                    detail:
                        "blob is not referenced by any document; `dpaint op asset.gc` reclaims it"
                            .into(),
                    value: None,
                    required: None,
                });
            }
        }
    }

    let errors = findings
        .iter()
        .filter(|f| f.severity == Severity::Error)
        .count();
    let warnings = findings
        .iter()
        .filter(|f| f.severity == Severity::Warn)
        .count();
    Ok(Report {
        findings,
        errors,
        warnings,
    })
}

/// A digest plus its lint findings — what the MCP `dpaint_render` tool returns.
pub fn digest_and_lint(
    project: &Project,
    doc: &DocId,
    assets: &AssetStore,
    opts: &DigestOptions,
) -> Result<(Digest, Vec<Finding>)> {
    Ok((
        digest(project, doc, assets, opts)?,
        lint_document(project, doc, assets, opts)?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_closed_cube_has_no_boundary_edges_and_an_open_strip_does() {
        // Two triangles forming a quad: its four outer edges are boundaries.
        let quad = [0u32, 1, 2, 0, 2, 3];
        assert_eq!(boundary_edges(&quad), 4);

        // A tetrahedron is closed.
        let tet = [0u32, 1, 2, 0, 2, 3, 0, 3, 1, 1, 3, 2];
        assert_eq!(boundary_edges(&tet), 0);
    }

    #[test]
    fn severity_ordering_lets_callers_threshold_on_it() {
        assert!(Severity::Error > Severity::Warn);
        assert!(Severity::Warn > Severity::Info);
        assert_eq!("warning".parse::<Severity>().unwrap(), Severity::Warn);
        assert!("loud".parse::<Severity>().is_err());
    }
}
