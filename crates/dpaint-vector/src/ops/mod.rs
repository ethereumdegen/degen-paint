//! The `vector.*` op catalog.
//!
//! One struct per op, ids exactly as in `docs/op-registry.md`. Every op resolves its
//! selectors and validates its arguments before it touches the project, so a failed op
//! leaves the document untouched.

pub mod artboard;
pub mod mask;
pub mod measure;
pub mod object;
pub mod path;
pub mod style;
pub mod textops;
pub mod trace;
pub mod transform;

use dpaint_core::color::Color;
use dpaint_core::doc::vector::{VKind, VObject, VectorDoc};
use dpaint_core::error::{Error, Result};
use dpaint_core::ids::{DocId, ObjectId};
use dpaint_core::kurbo::BezPath;
use dpaint_core::project::Project;
use dpaint_core::Op;

/// Every op this crate contributes.
pub fn all() -> Vec<Box<dyn Op>> {
    let mut v: Vec<Box<dyn Op>> = Vec::new();
    v.extend(artboard::ops());
    v.extend(object::ops());
    v.extend(path::ops());
    v.extend(style::ops());
    v.extend(transform::ops());
    v.extend(textops::ops());
    v.extend(mask::ops());
    v.extend(trace::ops());
    v.extend(measure::ops());
    v
}

/// Boilerplate for an op: id, about, schema from the args type, vector-only mode, and the
/// parse-then-run entry point. `query` marks the read-only measure ops.
#[macro_export]
macro_rules! vop {
    ($name:ident, $args:ty, $id:literal, $about:literal) => {
        $crate::vop!(@build $name, $args, $id, $about, false);
    };
    (query $name:ident, $args:ty, $id:literal, $about:literal) => {
        $crate::vop!(@build $name, $args, $id, $about, true);
    };
    (@build $name:ident, $args:ty, $id:literal, $about:literal, $q:expr) => {
        pub struct $name;

        impl dpaint_core::Op for $name {
            fn id(&self) -> &'static str {
                $id
            }
            fn about(&self) -> &'static str {
                $about
            }
            fn schema(&self) -> serde_json::Value {
                dpaint_core::schema_for::<$args>()
            }
            fn modes(&self) -> &'static [dpaint_core::DocKind] {
                &[dpaint_core::DocKind::Vector]
            }
            fn is_query(&self) -> bool {
                $q
            }
            fn apply(
                &self,
                project: &mut dpaint_core::Project,
                args: serde_json::Value,
                cx: &mut dpaint_core::OpCx,
            ) -> dpaint_core::Result<dpaint_core::OpEffect> {
                let a: $args = dpaint_core::parse_args($id, args)?;
                $name::run(project, a, cx)
            }
        }
    };
}

/// Document this op targets, checked to be a vector document.
pub(crate) fn doc_of(project: &Project, cx: &dpaint_core::OpCx) -> Result<DocId> {
    let id = cx.target_doc(project)?;
    project.vector(&id)?;
    Ok(id)
}

/// Resolve a selector to one or more object ids, in document order.
pub(crate) fn many(project: &Project, sel: &str, doc: &DocId) -> Result<Vec<ObjectId>> {
    Ok(dpaint_core::resolve(project, sel, Some(doc))?
        .into_iter()
        .map(|m| ObjectId::from(m.id))
        .collect())
}

/// Resolve a selector that must name exactly one object.
pub(crate) fn one(project: &Project, sel: &str, doc: &DocId) -> Result<ObjectId> {
    Ok(ObjectId::from(
        dpaint_core::resolve_one(project, sel, Some(doc))?.id,
    ))
}

/// Index path from the document root down to an object.
pub(crate) fn index_path(v: &VectorDoc, id: &ObjectId) -> Option<Vec<usize>> {
    fn rec(list: &[VObject], id: &ObjectId, trail: &mut Vec<usize>) -> bool {
        for (i, o) in list.iter().enumerate() {
            trail.push(i);
            if &o.id == id {
                return true;
            }
            if let VKind::Group { objects } = &o.kind {
                if rec(objects, id, trail) {
                    return true;
                }
            }
            trail.pop();
        }
        false
    }
    let mut trail = Vec::new();
    rec(&v.objects, id, &mut trail).then_some(trail)
}

/// The list an object lives in, so it can be removed, reordered or inserted next to.
pub(crate) fn owner_list<'a>(v: &'a mut VectorDoc, id: &ObjectId) -> Option<&'a mut Vec<VObject>> {
    let mut path = index_path(v, id)?;
    path.pop();
    let mut list = &mut v.objects;
    for i in path {
        match &mut list[i].kind {
            VKind::Group { objects } => list = objects,
            _ => return None,
        }
    }
    Some(list)
}

/// Insert an object at an index path captured before a removal, so a replacement lands
/// in the same group and at the same depth in the stacking order.
pub(crate) fn insert_at_path(v: &mut VectorDoc, path: &[usize], o: VObject) {
    let Some((last, parents)) = path.split_last() else {
        v.objects.push(o);
        return;
    };
    let mut list = &mut v.objects;
    for i in parents {
        let ok = matches!(list.get(*i).map(|x| &x.kind), Some(VKind::Group { .. }));
        if !ok {
            list.push(o);
            return;
        }
        match &mut list[*i].kind {
            VKind::Group { objects } => list = objects,
            _ => unreachable!("checked above"),
        }
    }
    let at = (*last).min(list.len());
    list.insert(at, o);
}

/// Position of an object inside its own list.
pub(crate) fn index_in_owner(v: &VectorDoc, id: &ObjectId) -> Option<usize> {
    index_path(v, id)?.last().copied()
}

/// A fresh object id derived from a name, made unique inside the document.
pub(crate) fn fresh_id(v: &VectorDoc, name: &str) -> ObjectId {
    let base = ObjectId::from_name(name);
    if v.object(&base).is_none() {
        return base;
    }
    for n in 2..10_000u32 {
        let c = ObjectId::from(format!("{base}-{n}"));
        if v.object(&c).is_none() {
            return c;
        }
    }
    ObjectId::generate()
}

/// Parse a colour argument: a hex literal, a project palette name, or `none`.
pub(crate) fn parse_color(project: &Project, s: &str) -> Result<Option<Color>> {
    let t = s.trim();
    if t.eq_ignore_ascii_case("none") || t.is_empty() {
        return Ok(None);
    }
    if let Some(c) = project.palette.get(t) {
        return Ok(Some(*c));
    }
    Color::parse(t)
        .map(Some)
        .ok_or_else(|| Error::Invalid(format!("'{s}' is not a colour, a palette name, or 'none'")))
}

/// Replace an object's geometry with a path given in **document space**, keeping its
/// style and identity. The ancestor transform is divided out so the stored data still
/// draws in the same place, and the object's own transform is spent.
pub(crate) fn set_geometry(v: &mut VectorDoc, id: &ObjectId, doc_path: &BezPath) -> Result<()> {
    let (_, parent) = crate::geom::locate(v, id)
        .ok_or_else(|| Error::Invalid(format!("object '{id}' disappeared")))?;
    let local = parent.inverse() * doc_path.clone();
    let o = v
        .object_mut(id)
        .ok_or_else(|| Error::Invalid(format!("object '{id}' disappeared")))?;
    if matches!(o.kind, VKind::Group { .. }) {
        return Err(Error::Invalid(format!(
            "'{id}' is a group; ungroup it or target its children"
        )));
    }
    o.kind = VKind::Path {
        d: crate::geom::to_d(&local),
    };
    o.transform = dpaint_core::doc::common::Transform::IDENTITY;
    Ok(())
}

/// Refuse to edit a locked object rather than silently skipping it.
pub(crate) fn check_unlocked(v: &VectorDoc, ids: &[ObjectId]) -> Result<()> {
    for id in ids {
        if v.object(id).map(|o| o.locked).unwrap_or(false) {
            return Err(Error::Invalid(format!(
                "object '{id}' is locked; unlock it with vector.style.opacity --locked false"
            )));
        }
    }
    Ok(())
}
