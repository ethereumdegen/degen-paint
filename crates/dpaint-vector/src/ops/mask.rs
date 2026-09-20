//! `vector.clip.*` and `vector.mask.*` — clipping to a shape and masking by luminance.

use super::{check_unlocked, doc_of, many, one};
use crate::geom;
use crate::vop;
use dpaint_core::error::{Error, Result};
use dpaint_core::ids::ObjectId;
use dpaint_core::project::Project;
use dpaint_core::{Op, OpCx, OpEffect};
use serde::Deserialize;

pub fn ops() -> Vec<Box<dyn Op>> {
    vec![
        Box::new(ClipSet),
        Box::new(ClipRelease),
        Box::new(MaskSet),
        Box::new(MaskRelease),
    ]
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SetArgs {
    /// Selector of the objects to clip or mask.
    pub target: String,
    /// Selector of the object used as the clip shape or the mask.
    pub source: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ReleaseArgs {
    /// Selector of the objects to release.
    pub target: String,
}

/// A clip or mask source may not be inside the thing it clips, or be the thing itself.
fn validate(
    project: &Project,
    doc: &dpaint_core::DocId,
    targets: &[ObjectId],
    source: &ObjectId,
) -> Result<()> {
    let v = project.vector(doc)?;
    if v.object(source).is_none() {
        return Err(Error::Invalid(format!("no object '{source}'")));
    }
    for t in targets {
        if t == source {
            return Err(Error::Invalid(format!(
                "'{source}' cannot clip or mask itself"
            )));
        }
        if geom::ancestors(v, source).contains(t) {
            return Err(Error::CyclicLink {
                from: t.to_string(),
                to: source.to_string(),
            });
        }
    }
    Ok(())
}

vop!(ClipSet, SetArgs, "vector.clip.set", "Clip objects to another object's filled region");

impl ClipSet {
    fn run(project: &mut Project, a: SetArgs, cx: &mut OpCx) -> Result<OpEffect> {
        let doc = doc_of(project, cx)?;
        let targets = many(project, &a.target, &doc)?;
        let source = one(project, &a.source, &doc)?;
        check_unlocked(project.vector(&doc)?, &targets)?;
        validate(project, &doc, &targets, &source)?;
        let v = project.vector(&doc)?;
        if geom::path_in_doc(v, &source)?.elements().is_empty() {
            return Err(Error::DegenerateGeometry(format!(
                "'{source}' has no geometry to clip with"
            )));
        }
        let v = project.vector_mut(&doc)?;
        for t in &targets {
            if let Some(o) = v.object_mut(t) {
                o.clip = Some(source.clone());
            }
        }
        Ok(OpEffect::changed(&doc))
    }
}

vop!(ClipRelease, ReleaseArgs, "vector.clip.release", "Remove a clip, leaving both objects in place");

impl ClipRelease {
    fn run(project: &mut Project, a: ReleaseArgs, cx: &mut OpCx) -> Result<OpEffect> {
        let doc = doc_of(project, cx)?;
        let targets = many(project, &a.target, &doc)?;
        check_unlocked(project.vector(&doc)?, &targets)?;
        let v = project.vector_mut(&doc)?;
        let mut hit = false;
        for t in &targets {
            if let Some(o) = v.object_mut(t) {
                hit |= o.clip.take().is_some();
            }
        }
        if !hit {
            return Err(Error::Invalid(format!(
                "'{}' matched nothing that is clipped",
                a.target
            )));
        }
        Ok(OpEffect::changed(&doc))
    }
}

vop!(MaskSet, SetArgs, "vector.mask.set", "Mask objects by another object's luminance");

impl MaskSet {
    fn run(project: &mut Project, a: SetArgs, cx: &mut OpCx) -> Result<OpEffect> {
        let doc = doc_of(project, cx)?;
        let targets = many(project, &a.target, &doc)?;
        let source = one(project, &a.source, &doc)?;
        check_unlocked(project.vector(&doc)?, &targets)?;
        validate(project, &doc, &targets, &source)?;
        let v = project.vector(&doc)?;
        if v.object(&source).map(|o| o.fill.is_none()).unwrap_or(true) {
            return Err(Error::Invalid(format!(
                "'{source}' has no fill, so it would mask everything away; give it a fill first"
            )));
        }
        let v = project.vector_mut(&doc)?;
        for t in &targets {
            if let Some(o) = v.object_mut(t) {
                o.mask = Some(source.clone());
            }
        }
        Ok(OpEffect::changed(&doc))
    }
}

vop!(MaskRelease, ReleaseArgs, "vector.mask.release", "Remove a luminance mask");

impl MaskRelease {
    fn run(project: &mut Project, a: ReleaseArgs, cx: &mut OpCx) -> Result<OpEffect> {
        let doc = doc_of(project, cx)?;
        let targets = many(project, &a.target, &doc)?;
        check_unlocked(project.vector(&doc)?, &targets)?;
        let v = project.vector_mut(&doc)?;
        let mut hit = false;
        for t in &targets {
            if let Some(o) = v.object_mut(t) {
                hit |= o.mask.take().is_some();
            }
        }
        if !hit {
            return Err(Error::Invalid(format!(
                "'{}' matched nothing that is masked",
                a.target
            )));
        }
        Ok(OpEffect::changed(&doc))
    }
}
