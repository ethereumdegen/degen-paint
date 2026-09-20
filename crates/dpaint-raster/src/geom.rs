//! Geometry bridge: `kurbo` is the interchange type, `tiny-skia` is the scanline rasterizer.

use dpaint_core::doc::common::{FillRule, LineCap, LineJoin, Stroke, Transform};
use dpaint_core::kurbo::{self, Affine, BezPath, PathEl, Point, Shape};
use dpaint_core::{Error, Result};

pub fn parse_d(d: &str) -> Result<BezPath> {
    BezPath::from_svg(d).map_err(|e| Error::DegenerateGeometry(format!("bad path data: {e}")))
}

pub fn rect_path(x: f64, y: f64, w: f64, h: f64) -> BezPath {
    kurbo::Rect::new(x, y, x + w, y + h).to_path(0.1)
}

pub fn ellipse_path(cx: f64, cy: f64, rx: f64, ry: f64) -> BezPath {
    kurbo::Ellipse::new(Point::new(cx, cy), (rx, ry), 0.0).to_path(0.05)
}

/// Convert to a tiny-skia path, applying `at` (document -> device) on the way.
pub fn to_sk(path: &BezPath, at: Affine) -> Option<tiny_skia::Path> {
    let mut b = tiny_skia::PathBuilder::new();
    let mut open = false;
    for el in path.elements() {
        match *el {
            PathEl::MoveTo(p) => {
                let p = at * p;
                b.move_to(p.x as f32, p.y as f32);
                open = true;
            }
            PathEl::LineTo(p) => {
                if !open {
                    continue;
                }
                let p = at * p;
                b.line_to(p.x as f32, p.y as f32);
            }
            PathEl::QuadTo(c, p) => {
                if !open {
                    continue;
                }
                let (c, p) = (at * c, at * p);
                b.quad_to(c.x as f32, c.y as f32, p.x as f32, p.y as f32);
            }
            PathEl::CurveTo(c1, c2, p) => {
                if !open {
                    continue;
                }
                let (c1, c2, p) = (at * c1, at * c2, at * p);
                b.cubic_to(
                    c1.x as f32,
                    c1.y as f32,
                    c2.x as f32,
                    c2.y as f32,
                    p.x as f32,
                    p.y as f32,
                );
            }
            PathEl::ClosePath => {
                if open {
                    b.close();
                }
            }
        }
    }
    b.finish()
}

pub fn sk_fill_rule(r: FillRule) -> tiny_skia::FillRule {
    match r {
        FillRule::Nonzero => tiny_skia::FillRule::Winding,
        FillRule::Evenodd => tiny_skia::FillRule::EvenOdd,
    }
}

/// Anti-aliased coverage of a filled path, 0..1 per pixel, row-major `w x h`.
pub fn fill_coverage(path: &tiny_skia::Path, w: u32, h: u32, rule: FillRule) -> Vec<f32> {
    let mut mask = match tiny_skia::Mask::new(w, h) {
        Some(m) => m,
        None => return vec![0.0; (w as usize) * (h as usize)],
    };
    mask.fill_path(path, sk_fill_rule(rule), true, tiny_skia::Transform::identity());
    mask.data().iter().map(|v| *v as f32 / 255.0).collect()
}

/// Coverage of a path's stroke outline, for shape layers and the `stroke` layer effect.
pub fn stroke_coverage(path: &tiny_skia::Path, stroke: &Stroke, scale: f64, w: u32, h: u32) -> Vec<f32> {
    let Some(outline) = stroke_outline(path, stroke, scale) else {
        return vec![0.0; (w as usize) * (h as usize)];
    };
    fill_coverage(&outline, w, h, FillRule::Nonzero)
}

pub fn stroke_outline(path: &tiny_skia::Path, stroke: &Stroke, scale: f64) -> Option<tiny_skia::Path> {
    let mut props = tiny_skia::Stroke {
        width: (stroke.width * scale).max(1e-3) as f32,
        miter_limit: stroke.miter as f32,
        line_cap: match stroke.cap {
            LineCap::Butt => tiny_skia::LineCap::Butt,
            LineCap::Round => tiny_skia::LineCap::Round,
            LineCap::Square => tiny_skia::LineCap::Square,
        },
        line_join: match stroke.join {
            LineJoin::Miter => tiny_skia::LineJoin::Miter,
            LineJoin::Round => tiny_skia::LineJoin::Round,
            LineJoin::Bevel => tiny_skia::LineJoin::Bevel,
        },
        dash: None,
    };
    if !stroke.dash.is_empty() {
        let dashes: Vec<f32> = stroke.dash.iter().map(|d| (*d * scale).max(0.01) as f32).collect();
        props.dash = tiny_skia::StrokeDash::new(dashes, (stroke.dash_offset * scale) as f32);
    }
    path.stroke(&props, 1.0)
}

/// Document -> device matrix for a layer: its own transform, then the render scale.
pub fn device_matrix(layer: Transform, scale: f64) -> Affine {
    Affine::scale(scale) * layer.to_kurbo()
}

/// Bounding box of a path in document space.
pub fn bbox(path: &BezPath) -> kurbo::Rect {
    path.bounding_box()
}
