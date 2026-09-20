//! `vector.text.*` — editing text, binding it to a path, flowing it into a shape and
//! converting it to outlines.

use super::{check_unlocked, doc_of, fresh_id, index_path, insert_at_path, many, one};
use crate::geom;
use crate::text as shaping;
use crate::vop;
use dpaint_core::doc::common::{Rect, TextAlign};
use dpaint_core::doc::vector::{PathSide, TextOnPath, VKind, VObject};
use dpaint_core::error::{Error, Result};
use dpaint_core::ids::ObjectId;
use dpaint_core::project::Project;
use dpaint_core::{Op, OpCx, OpEffect};
use serde::Deserialize;

pub fn ops() -> Vec<Box<dyn Op>> {
    vec![
        Box::new(Set),
        Box::new(SetFont),
        Box::new(OnPath),
        Box::new(ToOutlines),
        Box::new(FlowInShape),
    ]
}

fn text_of<'a>(v: &'a mut dpaint_core::VectorDoc, id: &ObjectId) -> Result<&'a mut VKind> {
    let o = v
        .object_mut(id)
        .ok_or_else(|| Error::Invalid(format!("object '{id}' vanished")))?;
    if !matches!(o.kind, VKind::Text { .. }) {
        return Err(Error::Invalid(format!(
            "'{id}' is a {}, not a text object",
            o.type_name()
        )));
    }
    Ok(&mut o.kind)
}

// --------------------------------------------------------------------------------- set

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SetArgs {
    /// Selector of the text objects to change.
    pub target: String,
    /// New string. `\n` starts a new line.
    #[serde(default)]
    pub text: Option<String>,
    /// Move the text origin (top-left of the first line's em box).
    #[serde(default)]
    pub x: Option<f64>,
    #[serde(default)]
    pub y: Option<f64>,
    /// left | center | right | justify
    #[serde(default)]
    pub align: Option<TextAlign>,
    /// Line height as a multiple of the font size.
    #[serde(default)]
    pub leading: Option<f64>,
    /// Extra advance per glyph, in document units.
    #[serde(default)]
    pub tracking: Option<f64>,
    /// Layout box `[x, y, w, h]`; text wraps to its width.
    #[serde(default)]
    pub r#box: Option<[f64; 4]>,
    /// Remove the layout box, returning the text to unbounded layout.
    #[serde(default)]
    pub clear_box: bool,
}

vop!(
    Set,
    SetArgs,
    "vector.text.set",
    "Change a text object's string, position, alignment, leading, tracking or box"
);

impl Set {
    fn run(project: &mut Project, a: SetArgs, cx: &mut OpCx) -> Result<OpEffect> {
        let doc = doc_of(project, cx)?;
        let ids = many(project, &a.target, &doc)?;
        check_unlocked(project.vector(&doc)?, &ids)?;
        if let Some(l) = a.leading {
            if l <= 0.0 {
                return Err(Error::Invalid("leading must be positive".into()));
            }
        }
        let v = project.vector_mut(&doc)?;
        for id in &ids {
            let VKind::Text { spec, origin, .. } = text_of(v, id)? else {
                unreachable!("checked above")
            };
            if let Some(t) = &a.text {
                spec.text = t.clone();
            }
            if let Some(al) = a.align {
                spec.align = al;
            }
            if let Some(l) = a.leading {
                spec.leading = l;
            }
            if let Some(t) = a.tracking {
                spec.tracking = t;
            }
            if a.clear_box {
                spec.r#box = None;
            }
            if let Some(b) = a.r#box {
                spec.r#box = Some(Rect::new(b[0], b[1], b[2], b[3]));
            }
            if let Some(x) = a.x {
                origin[0] = x;
            }
            if let Some(y) = a.y {
                origin[1] = y;
            }
        }
        Ok(OpEffect::changed(&doc))
    }
}

// ---------------------------------------------------------------------------- set-font

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SetFontArgs {
    /// Selector of the text objects to restyle.
    pub target: String,
    /// Family name. Unavailable families fall back and report `font-fallback`.
    #[serde(default)]
    pub family: Option<String>,
    /// Font size in document units.
    #[serde(default)]
    pub size: Option<f64>,
    /// Weight, 100..=900.
    #[serde(default)]
    pub weight: Option<u16>,
    #[serde(default)]
    pub italic: Option<bool>,
}

vop!(
    SetFont,
    SetFontArgs,
    "vector.text.set-font",
    "Change family, size, weight or slant, reporting any font substitution"
);

impl SetFont {
    fn run(project: &mut Project, a: SetFontArgs, cx: &mut OpCx) -> Result<OpEffect> {
        let doc = doc_of(project, cx)?;
        let ids = many(project, &a.target, &doc)?;
        check_unlocked(project.vector(&doc)?, &ids)?;
        if let Some(s) = a.size {
            if s <= 0.0 {
                return Err(Error::Invalid(format!("font size {s} must be positive")));
            }
        }
        if a.family.is_none() && a.size.is_none() && a.weight.is_none() && a.italic.is_none() {
            return Err(Error::Invalid(
                "nothing to change: pass family, size, weight or italic".into(),
            ));
        }
        let fonts = shaping::Fonts::for_project(project, cx.assets);
        let v = project.vector_mut(&doc)?;
        let mut eff = OpEffect::changed(&doc);
        for id in &ids {
            let VKind::Text { spec, .. } = text_of(v, id)? else {
                unreachable!("checked above")
            };
            if let Some(f) = &a.family {
                spec.family = f.clone();
            }
            if let Some(s) = a.size {
                spec.size = s;
            }
            if let Some(w) = a.weight {
                spec.weight = w.clamp(1, 1000);
            }
            if let Some(i) = a.italic {
                spec.italic = i;
            }
            if let Some(actual) = fonts
                .select(&spec.family, spec.weight, spec.italic)
                .substituted
            {
                eff = eff.warn(
                    "font-fallback",
                    id.to_string(),
                    format!("'{}' is unavailable; shaped with '{actual}'", spec.family),
                );
            }
        }
        Ok(eff)
    }
}

// ----------------------------------------------------------------------------- on-path

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct OnPathArgs {
    /// Selector of the text object.
    pub target: String,
    /// Selector of the path the text should follow. Omit with `release` to detach.
    #[serde(default)]
    pub path: Option<String>,
    /// Arc-length offset of the first glyph along the path.
    #[serde(default)]
    pub offset: f64,
    /// Place glyphs on the other side of the curve.
    #[serde(default)]
    pub flip: bool,
    /// Detach the text from its path, leaving it at its origin.
    #[serde(default)]
    pub release: bool,
}

vop!(
    OnPath,
    OnPathArgs,
    "vector.text.on-path",
    "Bind text to a path so each glyph follows its tangent, or release it"
);

impl OnPath {
    fn run(project: &mut Project, a: OnPathArgs, cx: &mut OpCx) -> Result<OpEffect> {
        let doc = doc_of(project, cx)?;
        let id = one(project, &a.target, &doc)?;
        check_unlocked(project.vector(&doc)?, &[id.clone()])?;
        if a.release {
            let v = project.vector_mut(&doc)?;
            let VKind::Text { on_path, .. } = text_of(v, &id)? else {
                unreachable!("checked above")
            };
            if on_path.take().is_none() {
                return Err(Error::Invalid(format!("'{id}' is not on a path")));
            }
            return Ok(OpEffect::changed(&doc));
        }
        let sel = a
            .path
            .as_deref()
            .ok_or_else(|| Error::Invalid("pass --path, or --release to detach".into()))?;
        let target = one(project, sel, &doc)?;
        if target == id {
            return Err(Error::Invalid("text cannot follow itself".into()));
        }
        let v = project.vector(&doc)?;
        // A path that contains the text (or is contained by it) would make resolving the
        // text's outline depend on itself.
        if geom::ancestors(v, &id).contains(&target) || geom::ancestors(v, &target).contains(&id) {
            return Err(Error::CyclicLink {
                from: id.to_string(),
                to: target.to_string(),
            });
        }
        let p = geom::path_in_doc(v, &target)?;
        if geom::length(&p) <= 0.0 {
            return Err(Error::DegenerateGeometry(format!(
                "'{target}' has no length for text to follow"
            )));
        }
        let v = project.vector_mut(&doc)?;
        let VKind::Text { on_path, .. } = text_of(v, &id)? else {
            unreachable!("checked above")
        };
        *on_path = Some(TextOnPath {
            target,
            offset: a.offset,
            side: if a.flip {
                PathSide::Right
            } else {
                PathSide::Left
            },
        });
        Ok(OpEffect::changed(&doc))
    }
}

// ------------------------------------------------------------------------- to-outlines

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ToOutlinesArgs {
    /// Selector of the text objects to convert.
    pub target: String,
}

vop!(
    ToOutlines,
    ToOutlinesArgs,
    "vector.text.to-outlines",
    "Replace text objects with real paths of their glyph outlines"
);

impl ToOutlines {
    fn run(project: &mut Project, a: ToOutlinesArgs, cx: &mut OpCx) -> Result<OpEffect> {
        let doc = doc_of(project, cx)?;
        let ids = many(project, &a.target, &doc)?;
        check_unlocked(project.vector(&doc)?, &ids)?;
        let fonts = shaping::Fonts::for_project(project, cx.assets);
        let v = project.vector(&doc)?;
        let mut plan: Vec<(
            ObjectId,
            VObject,
            Vec<dpaint_core::kurbo::BezPath>,
            Option<String>,
        )> = Vec::new();
        for id in &ids {
            let o = v
                .object(id)
                .ok_or_else(|| Error::Invalid(format!("object '{id}' vanished")))?;
            let VKind::Text {
                spec,
                origin,
                on_path,
            } = &o.kind
            else {
                return Err(Error::Invalid(format!(
                    "'{id}' is a {}, not a text object",
                    o.type_name()
                )));
            };
            let (lines, sub) = match on_path {
                Some(tp) => {
                    let target = geom::path_in_doc(v, &tp.target)?;
                    let flat = geom::flatten(&target, geom::DEFAULT_TOLERANCE);
                    let (p, sub, _) = shaping::outline_on_path(
                        &fonts,
                        spec,
                        &flat,
                        tp.offset,
                        tp.side == PathSide::Right,
                    );
                    (vec![p], sub)
                }
                None => shaping::outline_block_lines(&fonts, spec, (origin[0], origin[1])),
            };
            let lines: Vec<_> = lines
                .into_iter()
                .filter(|l| !l.elements().is_empty())
                .collect();
            if lines.is_empty() {
                return Err(Error::DegenerateGeometry(format!(
                    "'{id}' has no glyphs to outline"
                )));
            }
            plan.push((id.clone(), o.clone(), lines, sub));
        }
        let v = project.vector_mut(&doc)?;
        let mut eff = OpEffect::changed(&doc);
        for (id, src, lines, sub) in plan {
            let at = index_path(v, &id).unwrap_or_default();
            let mut built: Vec<VObject> = Vec::new();
            for (i, line) in lines.iter().enumerate() {
                let name = if lines.len() == 1 {
                    src.name.clone()
                } else {
                    format!("{}-line-{}", src.name, i + 1)
                };
                let nid = fresh_id(v, &name);
                let mut o = src.clone();
                o.id = nid.clone();
                o.name = name;
                o.transform = dpaint_core::doc::common::Transform::IDENTITY;
                o.kind = VKind::Path {
                    d: geom::to_d(&(src.transform.to_kurbo() * line.clone())),
                };
                built.push(o);
            }
            v.remove_object(&id);
            eff = eff.with_removed(id.to_string());
            let ids: Vec<String> = built.iter().map(|o| o.id.to_string()).collect();
            if built.len() == 1 {
                let o = built.pop().unwrap();
                insert_at_path(v, &at, o);
            } else {
                let gname = format!("{}-outlines", src.name);
                let gid = fresh_id(v, &gname);
                insert_at_path(
                    v,
                    &at,
                    VObject::new(gid.clone(), gname, VKind::Group { objects: built }),
                );
                eff = eff.with_created(gid.to_string());
            }
            for nid in ids {
                eff = eff.with_created(nid);
            }
            if let Some(actual) = sub {
                eff = eff.warn(
                    "font-fallback",
                    id.to_string(),
                    format!("outlined with the substituted family '{actual}'"),
                );
            }
        }
        Ok(eff)
    }
}

// ---------------------------------------------------------------------- flow-in-shape

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct FlowArgs {
    /// Selector of the text object to flow.
    pub target: String,
    /// Selector of the shape to flow it into.
    pub shape: String,
    /// Replace the text object with the flowed outlines instead of keeping it editable.
    #[serde(default = "yes")]
    pub to_outlines: bool,
}

fn yes() -> bool {
    true
}

vop!(
    FlowInShape,
    FlowArgs,
    "vector.text.flow-in-shape",
    "Lay text out inside an arbitrary shape, line by line"
);

impl FlowInShape {
    fn run(project: &mut Project, a: FlowArgs, cx: &mut OpCx) -> Result<OpEffect> {
        let doc = doc_of(project, cx)?;
        let id = one(project, &a.target, &doc)?;
        let shape_id = one(project, &a.shape, &doc)?;
        if shape_id == id {
            return Err(Error::Invalid("text cannot flow into itself".into()));
        }
        {
            let v = project.vector(&doc)?;
            if geom::ancestors(v, &id).contains(&shape_id) {
                return Err(Error::CyclicLink {
                    from: id.to_string(),
                    to: shape_id.to_string(),
                });
            }
        }
        check_unlocked(project.vector(&doc)?, &[id.clone()])?;
        let fonts = shaping::Fonts::for_project(project, cx.assets);
        let v = project.vector(&doc)?;
        let src = v
            .object(&id)
            .cloned()
            .ok_or_else(|| Error::Invalid(format!("object '{id}' vanished")))?;
        let VKind::Text { spec, .. } = &src.kind else {
            return Err(Error::Invalid(format!(
                "'{id}' is a {}, not a text object",
                src.type_name()
            )));
        };
        let shape = geom::path_in_doc(v, &shape_id)?;
        let rule = v.object(&shape_id).map(|o| o.fill_rule).unwrap_or_default();
        let (outline, sub, leftover) = shaping::flow_in_shape(
            &fonts,
            spec,
            &shape,
            rule == dpaint_core::doc::common::FillRule::Evenodd,
        )?;
        if outline.elements().is_empty() {
            return Err(Error::DegenerateGeometry(format!(
                "no line of '{id}' fits inside '{shape_id}'"
            )));
        }
        let v = project.vector_mut(&doc)?;
        let mut eff = OpEffect::changed(&doc);
        if a.to_outlines {
            let at = index_path(v, &id).unwrap_or_default();
            let name = format!("{}-flowed", src.name);
            let nid = fresh_id(v, &name);
            let mut o = src.clone();
            o.id = nid.clone();
            o.name = name;
            o.transform = dpaint_core::doc::common::Transform::IDENTITY;
            o.kind = VKind::Path {
                d: geom::to_d(&outline),
            };
            v.remove_object(&id);
            insert_at_path(v, &at, o);
            eff = eff
                .with_removed(id.to_string())
                .with_created(nid.to_string());
        } else {
            // Keep it editable: adopt the shape's bounding box as the layout box.
            let b = geom::bbox(&shape)
                .ok_or_else(|| Error::DegenerateGeometry(format!("'{shape_id}' has no extent")))?;
            let VKind::Text { spec, origin, .. } = text_of(v, &id)? else {
                unreachable!("checked above")
            };
            spec.r#box = Some(Rect::new(b.x0, b.y0, b.width(), b.height()));
            *origin = [b.x0, b.y0];
        }
        if !leftover.is_empty() {
            eff = eff.warn(
                "text-overflow",
                id.to_string(),
                format!(
                    "{} word(s) did not fit: {}",
                    leftover.len(),
                    leftover.join(" ")
                ),
            );
        }
        if let Some(actual) = sub {
            eff = eff.warn(
                "font-fallback",
                id.to_string(),
                format!("shaped with the substituted family '{actual}'"),
            );
        }
        Ok(eff)
    }
}
