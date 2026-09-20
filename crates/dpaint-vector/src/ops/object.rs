//! `vector.object.*` — creating, removing, duplicating, reordering and grouping objects.

use super::{doc_of, fresh_id, index_in_owner, many, one, owner_list, parse_color};
use crate::vop;
use dpaint_core::doc::common::{Paint, Rect, Stroke, TextSpec, Transform};
use dpaint_core::doc::vector::{VKind, VObject, VectorDoc};
use dpaint_core::error::{Error, Result};
use dpaint_core::ids::ObjectId;
use dpaint_core::project::Project;
use dpaint_core::{Op, OpCx, OpEffect};
use serde::Deserialize;

pub fn ops() -> Vec<Box<dyn Op>> {
    vec![
        Box::new(AddPath),
        Box::new(AddRect),
        Box::new(AddEllipse),
        Box::new(AddPolygon),
        Box::new(AddStar),
        Box::new(AddLine),
        Box::new(AddText),
        Box::new(AddImage),
        Box::new(Remove),
        Box::new(Duplicate),
        Box::new(Reorder),
        Box::new(Rename),
        Box::new(Group),
        Box::new(Ungroup),
    ]
}

/// Style and placement shared by every `add-*` op.
#[derive(Debug, Clone, Default, Deserialize, schemars::JsonSchema)]
pub struct Appearance {
    /// Name for the new object; the id is derived from it.
    #[serde(default)]
    pub name: Option<String>,
    /// Fill colour as `#rrggbb[aa]`, a palette name, or `none`.
    #[serde(default)]
    pub fill: Option<String>,
    /// Stroke colour as `#rrggbb[aa]`, a palette name, or `none`.
    #[serde(default)]
    pub stroke: Option<String>,
    /// Stroke width in document units. Defaults to 1 when a stroke colour is given.
    #[serde(default)]
    pub stroke_width: Option<f64>,
    /// Selector of the group to insert into. Omitted means the document root.
    #[serde(default)]
    pub parent: Option<String>,
}

fn create(
    project: &mut Project,
    cx: &OpCx,
    kind: VKind,
    ap: &Appearance,
    default_name: &str,
) -> Result<(dpaint_core::DocId, ObjectId)> {
    let doc = doc_of(project, cx)?;
    let fill = match &ap.fill {
        Some(s) => parse_color(project, s)?
            .map(Paint::solid)
            .unwrap_or(Paint::None),
        None => Paint::None,
    };
    let stroke_color = match &ap.stroke {
        Some(s) => parse_color(project, s)?,
        None => None,
    };
    if let Some(w) = ap.stroke_width {
        if w < 0.0 {
            return Err(Error::Invalid(format!("stroke width {w} is negative")));
        }
    }
    let parent = match &ap.parent {
        Some(sel) => {
            let id = one(project, sel, &doc)?;
            let v = project.vector(&doc)?;
            match v.object(&id).map(|o| &o.kind) {
                Some(VKind::Group { .. }) => Some(id),
                _ => {
                    return Err(Error::Invalid(format!(
                        "parent '{sel}' resolves to '{id}', which is not a group"
                    )))
                }
            }
        }
        None => None,
    };
    let name = ap.name.clone().unwrap_or_else(|| default_name.to_string());
    let v = project.vector_mut(&doc)?;
    let id = fresh_id(v, &name);
    let mut obj = VObject::new(id.clone(), name, kind);
    obj.fill = fill;
    if let Some(c) = stroke_color {
        obj.stroke = Some(Stroke::solid(c, ap.stroke_width.unwrap_or(1.0)));
    }
    match parent {
        Some(p) => match v.object_mut(&p) {
            Some(VObject {
                kind: VKind::Group { objects },
                ..
            }) => objects.push(obj),
            _ => return Err(Error::Invalid(format!("parent '{p}' vanished"))),
        },
        None => v.objects.push(obj),
    }
    Ok((doc, id))
}

fn created(doc: dpaint_core::DocId, id: ObjectId) -> OpEffect {
    OpEffect::changed(&doc).with_created(id.to_string())
}

// ---------------------------------------------------------------------------- add-path

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct AddPathArgs {
    /// SVG path data, e.g. `M 0 0 L 10 0 L 10 10 Z`.
    pub d: String,
    #[serde(flatten)]
    pub style: Appearance,
}

vop!(
    AddPath,
    AddPathArgs,
    "vector.object.add-path",
    "Add a Bézier path from SVG path data"
);

impl AddPath {
    fn run(project: &mut Project, a: AddPathArgs, cx: &mut OpCx) -> Result<OpEffect> {
        crate::geom::parse_d(&a.d)?;
        let (doc, id) = create(project, cx, VKind::Path { d: a.d }, &a.style, "path")?;
        Ok(created(doc, id))
    }
}

// ---------------------------------------------------------------------------- add-rect

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct AddRectArgs {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    /// Corner radius; clamped to half the shorter side.
    #[serde(default)]
    pub radius: f64,
    #[serde(flatten)]
    pub style: Appearance,
}

vop!(
    AddRect,
    AddRectArgs,
    "vector.object.add-rect",
    "Add a rectangle, optionally with rounded corners"
);

impl AddRect {
    fn run(project: &mut Project, a: AddRectArgs, cx: &mut OpCx) -> Result<OpEffect> {
        if a.width <= 0.0 || a.height <= 0.0 {
            return Err(Error::DegenerateGeometry(format!(
                "a rectangle needs a positive size, got {}x{}",
                a.width, a.height
            )));
        }
        let kind = VKind::Rect {
            rect: Rect::new(a.x, a.y, a.width, a.height),
            radius: a.radius.max(0.0),
        };
        let (doc, id) = create(project, cx, kind, &a.style, "rect")?;
        Ok(created(doc, id))
    }
}

// ------------------------------------------------------------------------- add-ellipse

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct AddEllipseArgs {
    /// Centre x.
    pub cx: f64,
    /// Centre y.
    pub cy: f64,
    /// Horizontal radius.
    pub rx: f64,
    /// Vertical radius; defaults to `rx`, giving a circle.
    #[serde(default)]
    pub ry: Option<f64>,
    #[serde(flatten)]
    pub style: Appearance,
}

vop!(
    AddEllipse,
    AddEllipseArgs,
    "vector.object.add-ellipse",
    "Add an ellipse or circle"
);

impl AddEllipse {
    fn run(project: &mut Project, a: AddEllipseArgs, cx: &mut OpCx) -> Result<OpEffect> {
        let ry = a.ry.unwrap_or(a.rx);
        if a.rx <= 0.0 || ry <= 0.0 {
            return Err(Error::DegenerateGeometry(format!(
                "an ellipse needs positive radii, got {} and {ry}",
                a.rx
            )));
        }
        let kind = VKind::Ellipse {
            center: [a.cx, a.cy],
            radius: [a.rx, ry],
        };
        let (doc, id) = create(project, cx, kind, &a.style, "ellipse")?;
        Ok(created(doc, id))
    }
}

// ------------------------------------------------------------------------- add-polygon

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct AddPolygonArgs {
    pub cx: f64,
    pub cy: f64,
    /// Circumradius.
    pub radius: f64,
    /// Number of sides, 3 or more.
    pub sides: u32,
    /// Rotation in degrees; 0 puts the first vertex straight up.
    #[serde(default)]
    pub rotation: f64,
    #[serde(flatten)]
    pub style: Appearance,
}

vop!(
    AddPolygon,
    AddPolygonArgs,
    "vector.object.add-polygon",
    "Add a regular polygon"
);

impl AddPolygon {
    fn run(project: &mut Project, a: AddPolygonArgs, cx: &mut OpCx) -> Result<OpEffect> {
        if a.sides < 3 {
            return Err(Error::DegenerateGeometry(format!(
                "a polygon needs at least 3 sides, got {}",
                a.sides
            )));
        }
        if a.radius <= 0.0 {
            return Err(Error::DegenerateGeometry("radius must be positive".into()));
        }
        let kind = VKind::Polygon {
            center: [a.cx, a.cy],
            radius: a.radius,
            sides: a.sides,
            rotation: a.rotation,
        };
        let (doc, id) = create(project, cx, kind, &a.style, "polygon")?;
        Ok(created(doc, id))
    }
}

// ---------------------------------------------------------------------------- add-star

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct AddStarArgs {
    pub cx: f64,
    pub cy: f64,
    /// Radius of the outer points.
    pub outer: f64,
    /// Radius of the inner points.
    pub inner: f64,
    /// Number of points, 3 or more.
    pub points: u32,
    /// Rotation in degrees.
    #[serde(default)]
    pub rotation: f64,
    #[serde(flatten)]
    pub style: Appearance,
}

vop!(AddStar, AddStarArgs, "vector.object.add-star", "Add a star");

impl AddStar {
    fn run(project: &mut Project, a: AddStarArgs, cx: &mut OpCx) -> Result<OpEffect> {
        if a.points < 3 {
            return Err(Error::DegenerateGeometry(format!(
                "a star needs at least 3 points, got {}",
                a.points
            )));
        }
        if a.outer <= 0.0 || a.inner <= 0.0 {
            return Err(Error::DegenerateGeometry("radii must be positive".into()));
        }
        let kind = VKind::Star {
            center: [a.cx, a.cy],
            outer: a.outer,
            inner: a.inner,
            points: a.points,
            rotation: a.rotation,
        };
        let (doc, id) = create(project, cx, kind, &a.style, "star")?;
        Ok(created(doc, id))
    }
}

// ---------------------------------------------------------------------------- add-line

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct AddLineArgs {
    pub x1: f64,
    pub y1: f64,
    pub x2: f64,
    pub y2: f64,
    #[serde(flatten)]
    pub style: Appearance,
}

vop!(
    AddLine,
    AddLineArgs,
    "vector.object.add-line",
    "Add a straight line segment"
);

impl AddLine {
    fn run(project: &mut Project, a: AddLineArgs, cx: &mut OpCx) -> Result<OpEffect> {
        if (a.x1 - a.x2).abs() < f64::EPSILON && (a.y1 - a.y2).abs() < f64::EPSILON {
            return Err(Error::DegenerateGeometry(
                "a line needs two different endpoints".into(),
            ));
        }
        let mut style = a.style;
        if style.stroke.is_none() {
            style.stroke = Some("#000000".into());
        }
        let kind = VKind::Line {
            from: [a.x1, a.y1],
            to: [a.x2, a.y2],
        };
        let (doc, id) = create(project, cx, kind, &style, "line")?;
        Ok(created(doc, id))
    }
}

// ---------------------------------------------------------------------------- add-text

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct AddTextArgs {
    /// The string to set. `\n` starts a new line.
    pub text: String,
    /// Top-left of the first line's em box.
    #[serde(default)]
    pub x: f64,
    #[serde(default)]
    pub y: f64,
    /// Font family. Unavailable families fall back and report `font-fallback`.
    #[serde(default)]
    pub family: Option<String>,
    /// Font size in document units.
    #[serde(default)]
    pub size: Option<f64>,
    /// Weight, 100..=900.
    #[serde(default)]
    pub weight: Option<u16>,
    #[serde(default)]
    pub italic: bool,
    /// Layout box `[x, y, w, h]`; text wraps to its width.
    #[serde(default)]
    pub r#box: Option<[f64; 4]>,
    #[serde(flatten)]
    pub style: Appearance,
}

vop!(
    AddText,
    AddTextArgs,
    "vector.object.add-text",
    "Add a text object"
);

impl AddText {
    fn run(project: &mut Project, a: AddTextArgs, cx: &mut OpCx) -> Result<OpEffect> {
        let mut spec = TextSpec::new(a.text);
        if let Some(f) = a.family {
            spec.family = f;
        }
        if let Some(s) = a.size {
            if s <= 0.0 {
                return Err(Error::Invalid(format!("font size {s} must be positive")));
            }
            spec.size = s;
        }
        if let Some(w) = a.weight {
            spec.weight = w.clamp(1, 1000);
        }
        spec.italic = a.italic;
        spec.r#box = a.r#box.map(|b| Rect::new(b[0], b[1], b[2], b[3]));
        let sub = crate::text::fonts()
            .select(&spec.family, spec.weight, spec.italic)
            .substituted;
        let mut style = a.style;
        if style.fill.is_none() && style.stroke.is_none() {
            style.fill = Some("#000000".into());
        }
        let kind = VKind::Text {
            spec,
            origin: [a.x, a.y],
            on_path: None,
        };
        let (doc, id) = create(project, cx, kind, &style, "text")?;
        let mut eff = created(doc, id.clone());
        if let Some(actual) = sub {
            eff = eff.warn(
                "font-fallback",
                id.to_string(),
                format!("requested family is unavailable; shaped with '{actual}'"),
            );
        }
        Ok(eff)
    }
}

// --------------------------------------------------------------------------- add-image

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct AddImageArgs {
    /// Path to an image file on disk; its bytes go into the asset store.
    pub path: String,
    /// Placement x.
    #[serde(default)]
    pub x: f64,
    #[serde(default)]
    pub y: f64,
    /// Placement width; defaults to the image's pixel width.
    #[serde(default)]
    pub width: Option<f64>,
    #[serde(default)]
    pub height: Option<f64>,
    #[serde(flatten)]
    pub style: Appearance,
}

vop!(
    AddImage,
    AddImageArgs,
    "vector.object.add-image",
    "Place a raster image, stored in the asset store"
);

impl AddImage {
    fn run(project: &mut Project, a: AddImageArgs, cx: &mut OpCx) -> Result<OpEffect> {
        let bytes = std::fs::read(&a.path)?;
        let img = image::load_from_memory(&bytes)
            .map_err(|e| Error::AssetDecode(format!("{}: {e}", a.path)))?;
        let (iw, ih) = (img.width() as f64, img.height() as f64);
        let ext = std::path::Path::new(&a.path)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("png")
            .to_ascii_lowercase();
        let asset = cx.assets.put(&bytes, &ext)?;
        let kind = VKind::Image {
            asset,
            rect: Rect::new(
                a.x,
                a.y,
                a.width.unwrap_or(iw).max(1e-6),
                a.height.unwrap_or(ih).max(1e-6),
            ),
        };
        let (doc, id) = create(project, cx, kind, &a.style, "image")?;
        Ok(created(doc, id))
    }
}

// ------------------------------------------------------------------------------ remove

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct TargetArgs {
    /// Selector of the objects to act on.
    pub target: String,
}

vop!(
    Remove,
    TargetArgs,
    "vector.object.remove",
    "Delete the selected objects"
);

impl Remove {
    fn run(project: &mut Project, a: TargetArgs, cx: &mut OpCx) -> Result<OpEffect> {
        let doc = doc_of(project, cx)?;
        let ids = many(project, &a.target, &doc)?;
        super::check_unlocked(project.vector(&doc)?, &ids)?;
        let v = project.vector_mut(&doc)?;
        let mut eff = OpEffect::changed(&doc);
        for id in &ids {
            if v.remove_object(id).is_some() {
                eff = eff.with_removed(id.to_string());
            }
        }
        // References to a deleted object would dangle.
        let removed: Vec<ObjectId> = ids.clone();
        for_each_mut(&mut v.objects, &mut |o| {
            if o.clip.as_ref().is_some_and(|c| removed.contains(c)) {
                o.clip = None;
            }
            if o.mask.as_ref().is_some_and(|m| removed.contains(m)) {
                o.mask = None;
            }
        });
        Ok(eff)
    }
}

/// Visit every object in the tree, depth first.
pub(crate) fn for_each_mut(list: &mut Vec<VObject>, f: &mut impl FnMut(&mut VObject)) {
    for o in list.iter_mut() {
        f(o);
        if let VKind::Group { objects } = &mut o.kind {
            for_each_mut(objects, f);
        }
    }
}

// --------------------------------------------------------------------------- duplicate

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct DuplicateArgs {
    /// Selector of the objects to copy.
    pub target: String,
    /// Offset applied to each copy.
    #[serde(default)]
    pub dx: f64,
    #[serde(default)]
    pub dy: f64,
}

vop!(
    Duplicate,
    DuplicateArgs,
    "vector.object.duplicate",
    "Copy the selected objects, optionally offset"
);

impl Duplicate {
    fn run(project: &mut Project, a: DuplicateArgs, cx: &mut OpCx) -> Result<OpEffect> {
        let doc = doc_of(project, cx)?;
        let ids = many(project, &a.target, &doc)?;
        let v = project.vector_mut(&doc)?;
        let mut eff = OpEffect::changed(&doc);
        for id in &ids {
            let Some(src) = v.object(id).cloned() else {
                continue;
            };
            let new_id = fresh_id(v, &format!("{}-copy", src.name));
            let mut copy = renumber(v, src, new_id.clone());
            copy.transform = Transform::from_kurbo(
                dpaint_core::kurbo::Affine::translate((a.dx, a.dy)) * copy.transform.to_kurbo(),
            );
            let at = index_in_owner(v, id).map(|i| i + 1);
            let list = owner_list(v, id).ok_or_else(|| Error::Invalid("object vanished".into()))?;
            match at {
                Some(i) if i <= list.len() => list.insert(i, copy),
                _ => list.push(copy),
            }
            eff = eff.with_created(new_id.to_string());
        }
        Ok(eff)
    }
}

/// Give a cloned subtree fresh ids so the document keeps unique identifiers.
fn renumber(v: &VectorDoc, mut o: VObject, id: ObjectId) -> VObject {
    o.id = id;
    if let VKind::Group { objects } = &mut o.kind {
        let children: Vec<VObject> = std::mem::take(objects);
        *objects = children
            .into_iter()
            .map(|c| {
                let cid = fresh_id(v, &format!("{}-copy", c.name));
                renumber(v, c, cid)
            })
            .collect();
    }
    o
}

// ----------------------------------------------------------------------------- reorder

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum ReorderTo {
    Front,
    Back,
    Forward,
    Backward,
    /// Move to an explicit index inside the current parent.
    Index,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ReorderArgs {
    /// Selector of the object to move.
    pub target: String,
    /// Where to move it in its parent's stacking order.
    pub to: ReorderTo,
    /// Destination index, required when `to` is `index`.
    #[serde(default)]
    pub index: Option<usize>,
}

vop!(
    Reorder,
    ReorderArgs,
    "vector.object.reorder",
    "Change an object's stacking order within its parent"
);

impl Reorder {
    fn run(project: &mut Project, a: ReorderArgs, cx: &mut OpCx) -> Result<OpEffect> {
        let doc = doc_of(project, cx)?;
        let id = one(project, &a.target, &doc)?;
        if a.to == ReorderTo::Index && a.index.is_none() {
            return Err(Error::Invalid("'index' is required when to=index".into()));
        }
        let v = project.vector_mut(&doc)?;
        let cur = index_in_owner(v, &id).ok_or_else(|| Error::Invalid("object vanished".into()))?;
        let list = owner_list(v, &id).ok_or_else(|| Error::Invalid("object vanished".into()))?;
        let last = list.len() - 1;
        let dest = match a.to {
            ReorderTo::Front => last,
            ReorderTo::Back => 0,
            ReorderTo::Forward => (cur + 1).min(last),
            ReorderTo::Backward => cur.saturating_sub(1),
            ReorderTo::Index => {
                let i = a.index.unwrap();
                if i > last {
                    return Err(Error::Invalid(format!(
                        "index {i} is out of range; the parent holds {} objects",
                        last + 1
                    )));
                }
                i
            }
        };
        let o = list.remove(cur);
        list.insert(dest, o);
        Ok(OpEffect::changed(&doc))
    }
}

// ------------------------------------------------------------------------------ rename

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct RenameArgs {
    /// Selector of the object to rename.
    pub target: String,
    /// The new name. Ids never change, so selectors by id keep working.
    pub name: String,
}

vop!(
    Rename,
    RenameArgs,
    "vector.object.rename",
    "Rename an object without changing its id"
);

impl Rename {
    fn run(project: &mut Project, a: RenameArgs, cx: &mut OpCx) -> Result<OpEffect> {
        let doc = doc_of(project, cx)?;
        let id = one(project, &a.target, &doc)?;
        let v = project.vector_mut(&doc)?;
        v.object_mut(&id)
            .ok_or_else(|| Error::Invalid("object vanished".into()))?
            .name = a.name;
        Ok(OpEffect::changed(&doc))
    }
}

// ------------------------------------------------------------------------------- group

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct GroupArgs {
    /// Selector of the objects to group; they must share a parent.
    pub target: String,
    /// Name for the new group.
    #[serde(default)]
    pub name: Option<String>,
}

vop!(
    Group,
    GroupArgs,
    "vector.object.group",
    "Wrap the selected objects in a group"
);

impl Group {
    fn run(project: &mut Project, a: GroupArgs, cx: &mut OpCx) -> Result<OpEffect> {
        let doc = doc_of(project, cx)?;
        let ids = many(project, &a.target, &doc)?;
        if ids.len() < 2 {
            return Err(Error::Invalid(format!(
                "grouping needs at least two objects, '{}' matched {}",
                a.target,
                ids.len()
            )));
        }
        let v = project.vector(&doc)?;
        let parents: Vec<Vec<usize>> = ids
            .iter()
            .map(|i| {
                let mut p = super::index_path(v, i).unwrap_or_default();
                p.pop();
                p
            })
            .collect();
        if parents.windows(2).any(|w| w[0] != w[1]) {
            return Err(Error::Invalid(
                "every object in a group must come from the same parent".into(),
            ));
        }
        let name = a.name.unwrap_or_else(|| "group".to_string());
        let v = project.vector_mut(&doc)?;
        let gid = fresh_id(v, &name);
        let anchor = index_in_owner(v, &ids[0]).unwrap_or(0);
        let list =
            owner_list(v, &ids[0]).ok_or_else(|| Error::Invalid("object vanished".into()))?;
        let mut taken = Vec::new();
        let mut i = 0;
        while i < list.len() {
            if ids.contains(&list[i].id) {
                taken.push(list.remove(i));
            } else {
                i += 1;
            }
        }
        let at = anchor.min(list.len());
        list.insert(
            at,
            VObject::new(gid.clone(), name, VKind::Group { objects: taken }),
        );
        Ok(OpEffect::changed(&doc).with_created(gid.to_string()))
    }
}

// ----------------------------------------------------------------------------- ungroup

vop!(
    Ungroup,
    TargetArgs,
    "vector.object.ungroup",
    "Dissolve a group, baking its transform and opacity into its children"
);

impl Ungroup {
    fn run(project: &mut Project, a: TargetArgs, cx: &mut OpCx) -> Result<OpEffect> {
        let doc = doc_of(project, cx)?;
        let ids = many(project, &a.target, &doc)?;
        let v = project.vector(&doc)?;
        let groups: Vec<ObjectId> = ids
            .into_iter()
            .filter(|i| matches!(v.object(i).map(|o| &o.kind), Some(VKind::Group { .. })))
            .collect();
        if groups.is_empty() {
            return Err(Error::Invalid(format!(
                "'{}' matched no groups to ungroup",
                a.target
            )));
        }
        let v = project.vector_mut(&doc)?;
        let mut eff = OpEffect::changed(&doc);
        for gid in &groups {
            let at = index_in_owner(v, gid).unwrap_or(0);
            let list = owner_list(v, gid).ok_or_else(|| Error::Invalid("group vanished".into()))?;
            let g = list.remove(at);
            let VKind::Group { objects } = g.kind else {
                continue;
            };
            for (n, mut child) in objects.into_iter().enumerate() {
                child.transform =
                    Transform::from_kurbo(g.transform.to_kurbo() * child.transform.to_kurbo());
                child.opacity *= g.opacity;
                child.visible = child.visible && g.visible;
                if child.clip.is_none() {
                    child.clip = g.clip.clone();
                }
                if child.mask.is_none() {
                    child.mask = g.mask.clone();
                }
                list.insert(at + n, child);
            }
            eff = eff.with_removed(gid.to_string());
        }
        Ok(eff)
    }
}
