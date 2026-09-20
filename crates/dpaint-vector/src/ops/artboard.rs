//! `vector.artboard.*` — the page rectangles a vector document renders through.

use super::{doc_of, parse_color};
use crate::vop;
use dpaint_core::doc::common::Rect;
use dpaint_core::doc::vector::Artboard;
use dpaint_core::error::{Error, Result};
use dpaint_core::ids::ArtboardId;
use dpaint_core::project::Project;
use dpaint_core::{Op, OpCx, OpEffect};
use serde::Deserialize;

pub fn ops() -> Vec<Box<dyn Op>> {
    vec![
        Box::new(Add),
        Box::new(Remove),
        Box::new(Resize),
        Box::new(FitContent),
    ]
}

fn find(project: &Project, doc: &dpaint_core::DocId, name: Option<&str>) -> Result<usize> {
    let v = project.vector(doc)?;
    match name {
        None => {
            if v.artboards.is_empty() {
                return Err(Error::Invalid("this document has no artboards".into()));
            }
            Ok(0)
        }
        Some(n) => v
            .artboards
            .iter()
            .position(|a| a.id.as_str() == n || a.name == n)
            .ok_or_else(|| {
                Error::Invalid(format!(
                    "no artboard '{n}'; this document has {}",
                    v.artboards
                        .iter()
                        .map(|a| a.id.to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                ))
            }),
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct AddArgs {
    /// Name for the artboard; its id is derived from it.
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub x: f64,
    #[serde(default)]
    pub y: f64,
    pub width: f64,
    pub height: f64,
    /// Background colour, or `none` for transparent.
    #[serde(default)]
    pub background: Option<String>,
}

vop!(Add, AddArgs, "vector.artboard.add", "Add an artboard to the document");

impl Add {
    fn run(project: &mut Project, a: AddArgs, cx: &mut OpCx) -> Result<OpEffect> {
        let doc = doc_of(project, cx)?;
        if a.width <= 0.0 || a.height <= 0.0 {
            return Err(Error::Invalid(format!(
                "an artboard needs a positive size, got {}x{}",
                a.width, a.height
            )));
        }
        let bg = match &a.background {
            Some(s) => parse_color(project, s)?,
            None => None,
        };
        let v = project.vector_mut(&doc)?;
        let name = a.name.unwrap_or_else(|| format!("artboard-{}", v.artboards.len() + 1));
        let mut id = ArtboardId::from_name(&name);
        if v.artboards.iter().any(|b| b.id == id) {
            id = ArtboardId::generate();
        }
        v.artboards.push(Artboard {
            id: id.clone(),
            name,
            rect: Rect::new(a.x, a.y, a.width, a.height),
            background: bg,
        });
        Ok(OpEffect::changed(&doc).with_created(id.to_string()))
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct RemoveArgs {
    /// Artboard id or name.
    pub artboard: String,
}

vop!(Remove, RemoveArgs, "vector.artboard.remove", "Remove an artboard; the last one cannot be removed");

impl Remove {
    fn run(project: &mut Project, a: RemoveArgs, cx: &mut OpCx) -> Result<OpEffect> {
        let doc = doc_of(project, cx)?;
        let i = find(project, &doc, Some(&a.artboard))?;
        let v = project.vector_mut(&doc)?;
        if v.artboards.len() == 1 {
            return Err(Error::Invalid(
                "a vector document needs at least one artboard".into(),
            ));
        }
        let gone = v.artboards.remove(i);
        Ok(OpEffect::changed(&doc).with_removed(gone.id.to_string()))
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ResizeArgs {
    /// Artboard id or name; omitted means the first artboard.
    #[serde(default)]
    pub artboard: Option<String>,
    #[serde(default)]
    pub x: Option<f64>,
    #[serde(default)]
    pub y: Option<f64>,
    #[serde(default)]
    pub width: Option<f64>,
    #[serde(default)]
    pub height: Option<f64>,
    /// Background colour, or `none` for transparent.
    #[serde(default)]
    pub background: Option<String>,
}

vop!(Resize, ResizeArgs, "vector.artboard.resize", "Move, resize or recolour an artboard");

impl Resize {
    fn run(project: &mut Project, a: ResizeArgs, cx: &mut OpCx) -> Result<OpEffect> {
        let doc = doc_of(project, cx)?;
        let i = find(project, &doc, a.artboard.as_deref())?;
        for d in [a.width, a.height].into_iter().flatten() {
            if d <= 0.0 {
                return Err(Error::Invalid(format!("size {d} must be positive")));
            }
        }
        let bg = match &a.background {
            Some(s) => Some(parse_color(project, s)?),
            None => None,
        };
        let v = project.vector_mut(&doc)?;
        let ab = &mut v.artboards[i];
        let r = ab.rect;
        ab.rect = Rect::new(
            a.x.unwrap_or(r.x()),
            a.y.unwrap_or(r.y()),
            a.width.unwrap_or(r.w()),
            a.height.unwrap_or(r.h()),
        );
        if let Some(c) = bg {
            ab.background = c;
        }
        Ok(OpEffect::changed(&doc))
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct FitArgs {
    /// Artboard id or name; omitted means the first artboard.
    #[serde(default)]
    pub artboard: Option<String>,
    /// Padding added on every side.
    #[serde(default)]
    pub padding: f64,
}

vop!(FitContent, FitArgs, "vector.artboard.fit-content", "Shrink or grow an artboard to the bounds of the document's objects");

impl FitContent {
    fn run(project: &mut Project, a: FitArgs, cx: &mut OpCx) -> Result<OpEffect> {
        let doc = doc_of(project, cx)?;
        let i = find(project, &doc, a.artboard.as_deref())?;
        let v = project.vector(&doc)?;
        let b = super::transform::content_bounds(v).ok_or_else(|| {
            Error::DegenerateGeometry("the document has no objects to fit to".into())
        })?;
        let p = a.padding.max(0.0);
        let v = project.vector_mut(&doc)?;
        v.artboards[i].rect = Rect::new(
            b.x0 - p,
            b.y0 - p,
            (b.width() + 2.0 * p).max(1.0),
            (b.height() + 2.0 * p).max(1.0),
        );
        Ok(OpEffect::changed(&doc))
    }
}
