//! `vector.measure.*` — read-only geometry queries. These never mutate and never journal.

use super::{doc_of, many, one};
use crate::geom;
use crate::vop;
use dpaint_core::error::{Error, Result};
use dpaint_core::project::Project;
use dpaint_core::{Op, OpCx, OpEffect};
use serde::Deserialize;
use serde_json::json;

pub fn ops() -> Vec<Box<dyn Op>> {
    vec![
        Box::new(Bbox),
        Box::new(Length),
        Box::new(Area),
        Box::new(Sample),
        Box::new(Tangent),
        Box::new(Intersections),
    ]
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct TargetArgs {
    /// Selector of the objects to measure.
    pub target: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct AtArgs {
    /// Selector of the path to sample.
    pub target: String,
    /// Normalized position along the path by arc length, 0..1.
    pub t: f64,
}

// -------------------------------------------------------------------------------- bbox

vop!(query Bbox, TargetArgs, "vector.measure.bbox", "Bounding box of each match and of the whole selection, in document space");

impl Bbox {
    fn run(project: &mut Project, a: TargetArgs, cx: &mut OpCx) -> Result<OpEffect> {
        let doc = doc_of(project, cx)?;
        let ids = many(project, &a.target, &doc)?;
        let v = project.vector(&doc)?;
        let mut per = Vec::new();
        let mut union: Option<dpaint_core::kurbo::Rect> = None;
        for id in &ids {
            let Some(b) = geom::bbox(&geom::path_in_doc(v, id)?) else {
                continue;
            };
            union = Some(match union {
                Some(u) => u.union(b),
                None => b,
            });
            per.push(json!({
                "id": id.to_string(),
                "x": b.x0, "y": b.y0, "width": b.width(), "height": b.height(),
            }));
        }
        let u = union.ok_or_else(|| {
            Error::DegenerateGeometry(format!("'{}' matched nothing with an extent", a.target))
        })?;
        Ok(OpEffect::default().with_data(json!({
            "objects": per,
            "bbox": { "x": u.x0, "y": u.y0, "width": u.width(), "height": u.height() },
        })))
    }
}

// ------------------------------------------------------------------------------ length

vop!(query Length, TargetArgs, "vector.measure.length", "Total outline length of each match");

impl Length {
    fn run(project: &mut Project, a: TargetArgs, cx: &mut OpCx) -> Result<OpEffect> {
        let doc = doc_of(project, cx)?;
        let ids = many(project, &a.target, &doc)?;
        let v = project.vector(&doc)?;
        let mut per = Vec::new();
        let mut total = 0.0;
        for id in &ids {
            let l = geom::length(&geom::path_in_doc(v, id)?);
            total += l;
            per.push(json!({ "id": id.to_string(), "length": l }));
        }
        Ok(OpEffect::default().with_data(json!({ "objects": per, "total": total })))
    }
}

// -------------------------------------------------------------------------------- area

vop!(query Area, TargetArgs, "vector.measure.area", "Enclosed area of each match, computed from the Béziers");

impl Area {
    fn run(project: &mut Project, a: TargetArgs, cx: &mut OpCx) -> Result<OpEffect> {
        let doc = doc_of(project, cx)?;
        let ids = many(project, &a.target, &doc)?;
        let v = project.vector(&doc)?;
        let mut per = Vec::new();
        let mut total = 0.0;
        for id in &ids {
            let area = geom::area(&geom::path_in_doc(v, id)?);
            total += area;
            per.push(json!({ "id": id.to_string(), "area": area }));
        }
        Ok(OpEffect::default().with_data(json!({ "objects": per, "total": total })))
    }
}

// ------------------------------------------------------------------------------ sample

vop!(query Sample, AtArgs, "vector.measure.sample", "Point at a normalized arc-length position along a path");

impl Sample {
    fn run(project: &mut Project, a: AtArgs, cx: &mut OpCx) -> Result<OpEffect> {
        let (pt, tan, len) = sample_at(project, cx, &a.target, a.t)?;
        Ok(OpEffect::default().with_data(json!({
            "t": a.t.clamp(0.0, 1.0),
            "point": [pt.x, pt.y],
            "tangent": [tan.x, tan.y],
            "length": len,
        })))
    }
}

// ----------------------------------------------------------------------------- tangent

vop!(query Tangent, AtArgs, "vector.measure.tangent", "Unit tangent and normal at a normalized position along a path");

impl Tangent {
    fn run(project: &mut Project, a: AtArgs, cx: &mut OpCx) -> Result<OpEffect> {
        let (pt, tan, _) = sample_at(project, cx, &a.target, a.t)?;
        Ok(OpEffect::default().with_data(json!({
            "t": a.t.clamp(0.0, 1.0),
            "point": [pt.x, pt.y],
            "tangent": [tan.x, tan.y],
            "normal": [-tan.y, tan.x],
            "angle_degrees": tan.y.atan2(tan.x).to_degrees(),
        })))
    }
}

fn sample_at(
    project: &Project,
    cx: &OpCx,
    target: &str,
    t: f64,
) -> Result<(dpaint_core::kurbo::Point, dpaint_core::kurbo::Vec2, f64)> {
    let doc = doc_of(project, cx)?;
    let id = one(project, target, &doc)?;
    let v = project.vector(&doc)?;
    let p = geom::path_in_doc(v, &id)?;
    let (pt, tan) = geom::sample(&p, t).ok_or_else(|| {
        Error::DegenerateGeometry(format!("'{id}' has no segments to sample"))
    })?;
    Ok((pt, tan, geom::length(&p)))
}

// ----------------------------------------------------------------------- intersections

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct IntersectArgs {
    /// Selector of the first path.
    pub target: String,
    /// Selector of the path to intersect it with.
    pub with: String,
}

vop!(query Intersections, IntersectArgs, "vector.measure.intersections", "Every crossing point between two paths");

impl Intersections {
    fn run(project: &mut Project, a: IntersectArgs, cx: &mut OpCx) -> Result<OpEffect> {
        let doc = doc_of(project, cx)?;
        let first = one(project, &a.target, &doc)?;
        let second = one(project, &a.with, &doc)?;
        if first == second {
            return Err(Error::Invalid(
                "intersecting an object with itself needs two different selectors".into(),
            ));
        }
        let v = project.vector(&doc)?;
        let pts = geom::intersections(
            &geom::path_in_doc(v, &first)?,
            &geom::path_in_doc(v, &second)?,
        );
        Ok(OpEffect::default().with_data(json!({
            "count": pts.len(),
            "points": pts.iter().map(|p| json!([p.x, p.y])).collect::<Vec<_>>(),
        })))
    }
}
