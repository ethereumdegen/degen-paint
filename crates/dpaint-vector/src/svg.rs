//! SVG in and out.
//!
//! Export is hand-written rather than generic so the output is tidy and, more importantly,
//! *stable*: fixed attribute order, fixed numeric precision, gradients and clips in
//! `<defs>` with ids derived from the object they belong to, and no redundant transforms
//! (shape transforms are baked into the path data, group transforms are kept because a
//! group is a real coordinate system). Import goes through `usvg`, which resolves units,
//! inheritance, `use` and CSS for us.
//!
//! The consequence of both halves agreeing on those rules is that
//! `to_svg(import_svg(to_svg(doc)))` is byte-identical to `to_svg(doc)`.

use crate::geom;
use crate::text;
use dpaint_core::asset::AssetStore;
use dpaint_core::color::Color;
use dpaint_core::doc::common::{
    BlendMode, FillRule, GradientStop, LineCap, LineJoin, Paint, Rect, Stroke, TextAlign, TextSpec,
    Transform,
};
use dpaint_core::doc::vector::{Artboard, VKind, VObject, VectorDoc};
use dpaint_core::error::{Error, Result};
use dpaint_core::ids::{ArtboardId, DocId, ObjectId};
use dpaint_core::kurbo::{Affine, BezPath, Point, Shape};
use dpaint_core::project::Project;
use std::collections::BTreeSet;
use std::fmt::Write as _;

const NS: &str = "http://www.w3.org/2000/svg";

/// Serialize a vector document to SVG.
pub fn to_svg(project: &Project, doc: &DocId) -> Result<String> {
    write_doc_with(project.vector(doc)?, None)
}

/// Serialize with embedded images inlined as data URIs. Without an asset store an image
/// object can only be written as its frame, because its pixels live in the store.
pub fn to_svg_with_images(project: &Project, doc: &DocId, assets: &AssetStore) -> Result<String> {
    write_doc_with(project.vector(doc)?, Some(assets))
}

/// Serialize a document you already hold.
pub fn write_doc(v: &VectorDoc) -> Result<String> {
    write_doc_with(v, None)
}

fn write_doc_with(v: &VectorDoc, assets: Option<&AssetStore>) -> Result<String> {
    let b = crate::raster::doc_bounds(v);
    let hidden = referenced_shapes(v);
    let mut defs = String::new();
    let mut body = String::new();
    collect_defs(v, &v.objects, &mut defs)?;
    for o in &v.objects {
        if hidden.contains(&o.id) {
            continue;
        }
        write_object(v, o, 1, assets, &mut body)?;
    }
    let mut out = String::new();
    let _ = writeln!(
        out,
        r#"<svg xmlns="{NS}" width="{}" height="{}" viewBox="{} {} {} {}">"#,
        num(b.width()),
        num(b.height()),
        num(b.x0),
        num(b.y0),
        num(b.width()),
        num(b.height())
    );
    if !defs.is_empty() {
        let _ = writeln!(out, "  <defs>");
        out.push_str(&defs);
        let _ = writeln!(out, "  </defs>");
    }
    out.push_str(&body);
    let _ = writeln!(out, "</svg>");
    Ok(out)
}

/// Objects that exist only as a clip or mask source: they belong in `<defs>`, not the body.
fn referenced_shapes(v: &VectorDoc) -> BTreeSet<ObjectId> {
    let mut out = BTreeSet::new();
    for o in v.walk() {
        if let Some(c) = &o.clip {
            out.insert(c.clone());
        }
        if let Some(m) = &o.mask {
            out.insert(m.clone());
        }
    }
    out
}

fn collect_defs(v: &VectorDoc, objects: &[VObject], out: &mut String) -> Result<()> {
    for o in objects {
        if let Paint::Linear { .. } | Paint::Radial { .. } = &o.fill {
            write_gradient(&o.fill, &grad_id(&o.id, "fill"), out);
        }
        if let Some(s) = &o.stroke {
            if let Paint::Linear { .. } | Paint::Radial { .. } = &s.paint {
                write_gradient(&s.paint, &grad_id(&o.id, "stroke"), out);
            }
        }
        if let Some(c) = &o.clip {
            let p = geom::path_in_doc(v, c)?;
            let _ = writeln!(
                out,
                r#"    <clipPath id="cp_{c}"><path d="{}"/></clipPath>"#,
                geom::to_d(&p)
            );
        }
        if let Some(m) = &o.mask {
            let Some((mobj, parent)) = geom::locate(v, m) else {
                return Err(Error::Invalid(format!("mask object '{m}' not found")));
            };
            let p = parent * geom::object_path(v, mobj)?;
            let fill = paint_attr(&mobj.fill, "");
            let _ = writeln!(
                out,
                r#"    <mask id="mk_{m}" maskUnits="userSpaceOnUse"><path d="{}" fill="{}"/></mask>"#,
                geom::to_d(&p),
                if fill.is_empty() {
                    "#ffffff".to_string()
                } else {
                    fill
                }
            );
        }
        if let VKind::Group { objects } = &o.kind {
            collect_defs(v, objects, out)?;
        }
    }
    Ok(())
}

fn grad_id(obj: &ObjectId, role: &str) -> String {
    format!("grad_{obj}_{role}")
}

fn write_gradient(p: &Paint, id: &str, out: &mut String) {
    match p {
        Paint::Linear { stops, from, to } => {
            let _ = writeln!(
                out,
                r#"    <linearGradient id="{id}" gradientUnits="userSpaceOnUse" x1="{}" y1="{}" x2="{}" y2="{}">"#,
                num(from[0]),
                num(from[1]),
                num(to[0]),
                num(to[1])
            );
            write_stops(stops, out);
            let _ = writeln!(out, "    </linearGradient>");
        }
        Paint::Radial {
            stops,
            center,
            radius,
            focal,
        } => {
            let f = focal.unwrap_or(*center);
            let _ = writeln!(
                out,
                r#"    <radialGradient id="{id}" gradientUnits="userSpaceOnUse" cx="{}" cy="{}" r="{}" fx="{}" fy="{}">"#,
                num(center[0]),
                num(center[1]),
                num(*radius),
                num(f[0]),
                num(f[1])
            );
            write_stops(stops, out);
            let _ = writeln!(out, "    </radialGradient>");
        }
        _ => {}
    }
}

fn write_stops(stops: &[GradientStop], out: &mut String) {
    for s in stops {
        let c = s.color;
        let mut line = format!(
            r#"      <stop offset="{}" stop-color="{}""#,
            num(s.offset),
            hex(c)
        );
        if c.a < 1.0 {
            let _ = write!(line, r#" stop-opacity="{}""#, num(c.a as f64));
        }
        let _ = writeln!(out, "{line}/>");
    }
}

fn write_object(
    v: &VectorDoc,
    o: &VObject,
    depth: usize,
    assets: Option<&AssetStore>,
    out: &mut String,
) -> Result<()> {
    let pad = "  ".repeat(depth);
    let mut attrs = format!(r#" id="{}""#, o.id);
    if let VKind::Group { .. } = o.kind {
        if !o.transform.is_identity() {
            let _ = write!(attrs, r#" transform="{}""#, matrix(&o.transform));
        }
    }
    if o.opacity < 1.0 {
        let _ = write!(attrs, r#" opacity="{}""#, num(o.opacity as f64));
    }
    if !o.blend.is_normal() {
        let _ = write!(attrs, r#" style="mix-blend-mode:{}""#, blend_name(o.blend));
    }
    if let Some(c) = &o.clip {
        let _ = write!(attrs, r#" clip-path="url(#cp_{c})""#);
    }
    if let Some(m) = &o.mask {
        let _ = write!(attrs, r#" mask="url(#mk_{m})""#);
    }

    match &o.kind {
        VKind::Group { objects } => {
            let _ = writeln!(out, "{pad}<g{attrs}>");
            for c in objects {
                write_object(v, c, depth + 1, assets, out)?;
            }
            let _ = writeln!(out, "{pad}</g>");
        }
        VKind::Text {
            spec,
            origin,
            on_path,
        } if on_path.is_none() && inline_text(spec) => {
            let m = text::shape(text::fonts(), spec).metrics;
            let anchor = match spec.align {
                TextAlign::Center => Some("middle"),
                TextAlign::Right => Some("end"),
                _ => None,
            };
            let _ = write!(
                out,
                r#"{pad}<text{attrs} x="{}" y="{}" font-family="{}" font-size="{}""#,
                num(origin[0]),
                num(origin[1] + m.ascent),
                esc(&spec.family),
                num(spec.size)
            );
            if spec.weight != 400 {
                let _ = write!(out, r#" font-weight="{}""#, spec.weight);
            }
            if spec.italic {
                let _ = write!(out, r#" font-style="italic""#);
            }
            if spec.tracking != 0.0 {
                let _ = write!(out, r#" letter-spacing="{}""#, num(spec.tracking));
            }
            if let Some(a) = anchor {
                let _ = write!(out, r#" text-anchor="{a}""#);
            }
            out.push_str(&style_attrs(o));
            let _ = writeln!(out, ">{}</text>", esc(&spec.text));
        }
        VKind::Image { asset, rect } if assets.is_some() => {
            let bytes = assets.expect("checked").get(asset)?;
            use base64::Engine;
            let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
            let mime = match asset.ext() {
                "jpg" | "jpeg" => "image/jpeg",
                "webp" => "image/webp",
                "gif" => "image/gif",
                _ => "image/png",
            };
            let r = o.transform.to_kurbo() * rect.to_kurbo().to_path(1e-3);
            let b = r.bounding_box();
            let _ = writeln!(
                out,
                r#"{pad}<image{attrs} x="{}" y="{}" width="{}" height="{}" href="data:{mime};base64,{b64}"/>"#,
                num(b.x0),
                num(b.y0),
                num(b.width()),
                num(b.height())
            );
        }
        _ => {
            let p = geom::object_path(v, o)?;
            let _ = writeln!(
                out,
                r#"{pad}<path{attrs} d="{}"{}/>"#,
                geom::to_d(&p),
                style_attrs(o)
            );
        }
    }
    Ok(())
}

/// Multi-line and box-flowed text needs `tspan` bookkeeping that no two renderers agree
/// on, so it is exported as outlines. Single-line text stays editable `<text>`.
fn inline_text(spec: &TextSpec) -> bool {
    !spec.text.contains('\n') && spec.r#box.is_none()
}

fn style_attrs(o: &VObject) -> String {
    let mut s = String::new();
    let _ = write!(
        s,
        r#" fill="{}""#,
        paint_attr(&o.fill, &grad_id(&o.id, "fill"))
    );
    if let Paint::Solid { color } = &o.fill {
        if color.a < 1.0 {
            let _ = write!(s, r#" fill-opacity="{}""#, num(color.a as f64));
        }
    }
    if o.fill_rule == FillRule::Evenodd {
        let _ = write!(s, r#" fill-rule="evenodd""#);
    }
    if let Some(st) = &o.stroke {
        let _ = write!(
            s,
            r#" stroke="{}" stroke-width="{}""#,
            paint_attr(&st.paint, &grad_id(&o.id, "stroke")),
            num(st.width)
        );
        if let Paint::Solid { color } = &st.paint {
            if color.a < 1.0 {
                let _ = write!(s, r#" stroke-opacity="{}""#, num(color.a as f64));
            }
        }
        if st.cap != LineCap::Butt {
            let _ = write!(s, r#" stroke-linecap="{}""#, cap_name(st.cap));
        }
        if st.join != LineJoin::Miter {
            let _ = write!(s, r#" stroke-linejoin="{}""#, join_name(st.join));
        }
        if st.miter != 4.0 {
            let _ = write!(s, r#" stroke-miterlimit="{}""#, num(st.miter));
        }
        if !st.dash.is_empty() {
            let arr: Vec<String> = st.dash.iter().map(|d| num(*d)).collect();
            let _ = write!(s, r#" stroke-dasharray="{}""#, arr.join(" "));
            if st.dash_offset != 0.0 {
                let _ = write!(s, r#" stroke-dashoffset="{}""#, num(st.dash_offset));
            }
        }
    }
    s
}

fn paint_attr(p: &Paint, grad: &str) -> String {
    match p {
        Paint::None => "none".into(),
        Paint::Solid { color } => hex(*color),
        Paint::Linear { .. } | Paint::Radial { .. } => format!("url(#{grad})"),
        Paint::Document { .. } => "none".into(),
    }
}

fn hex(c: Color) -> String {
    let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!("#{:02x}{:02x}{:02x}", q(c.r), q(c.g), q(c.b))
}

fn matrix(t: &Transform) -> String {
    let c = t.0;
    format!(
        "matrix({} {} {} {} {} {})",
        num(c[0]),
        num(c[1]),
        num(c[2]),
        num(c[3]),
        num(c[4]),
        num(c[5])
    )
}

fn num(v: f64) -> String {
    let r = (v * 1000.0).round() / 1000.0;
    let r = if r == 0.0 { 0.0 } else { r };
    let mut s = format!("{r}");
    if s.ends_with(".0") {
        s.truncate(s.len() - 2);
    }
    s
}

fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out
}

fn cap_name(c: LineCap) -> &'static str {
    match c {
        LineCap::Butt => "butt",
        LineCap::Round => "round",
        LineCap::Square => "square",
    }
}

fn join_name(j: LineJoin) -> &'static str {
    match j {
        LineJoin::Miter => "miter",
        LineJoin::Round => "round",
        LineJoin::Bevel => "bevel",
    }
}

fn blend_name(b: BlendMode) -> &'static str {
    use BlendMode::*;
    match b {
        Normal | Dissolve => "normal",
        Multiply | LinearBurn | DarkerColor => "multiply",
        Screen | LinearDodge | LighterColor => "screen",
        Overlay => "overlay",
        Darken => "darken",
        Lighten => "lighten",
        ColorDodge => "color-dodge",
        ColorBurn => "color-burn",
        HardLight | VividLight | LinearLight | PinLight | HardMix => "hard-light",
        SoftLight => "soft-light",
        Difference | Subtract => "difference",
        Exclusion | Divide => "exclusion",
        Hue => "hue",
        Saturation => "saturation",
        Color => "color",
        Luminosity => "luminosity",
    }
}

// ---------------------------------------------------------------------------------------
// Import
// ---------------------------------------------------------------------------------------

/// Parse an SVG into an editable vector document.
///
/// Embedded raster images are dropped: this entry point has no asset store to put their
/// pixels in, and the document model never carries pixels inline.
pub fn import_svg(svg: &str, doc_id: DocId, name: &str) -> Result<VectorDoc> {
    let mut opt = usvg::Options::default();
    opt.fontdb_mut()
        .load_font_data(text::FALLBACK_FONT.to_vec());
    map_generic_families(&mut opt);
    let tree = usvg::Tree::from_str(svg, &opt)
        .map_err(|e| Error::UnsupportedFormat(format!("not a parseable SVG: {e}")))?;
    from_tree(&tree, doc_id, name)
}

/// Every generic family resolves to the embedded fallback, so an import never silently
/// depends on what happens to be installed on the machine.
fn map_generic_families(opt: &mut usvg::Options) {
    let f = text::FALLBACK_FAMILY;
    opt.font_family = f.to_string();
    let db = opt.fontdb_mut();
    db.set_serif_family(f);
    db.set_sans_serif_family(f);
    db.set_cursive_family(f);
    db.set_fantasy_family(f);
    db.set_monospace_family(f);
}

fn from_tree(tree: &usvg::Tree, doc_id: DocId, name: &str) -> Result<VectorDoc> {
    let size = tree.size();
    let mut doc = VectorDoc {
        id: doc_id,
        name: name.to_string(),
        units: Default::default(),
        artboards: vec![Artboard {
            id: ArtboardId::from("ab_1"),
            name: "artboard".into(),
            rect: Rect::new(0.0, 0.0, size.width() as f64, size.height() as f64),
            background: None,
        }],
        objects: Vec::new(),
    };
    let mut ctx = Import {
        counter: 0,
        defs: Vec::new(),
    };
    let root_tf = affine(tree.root().transform());
    for node in tree.root().children() {
        if let Some(mut o) = ctx.node(node)? {
            if root_tf != Affine::IDENTITY {
                o.transform = Transform::from_kurbo(root_tf * o.transform.to_kurbo());
            }
            doc.objects.push(o);
        }
    }
    doc.objects.extend(std::mem::take(&mut ctx.defs));
    Ok(doc)
}

struct Import {
    counter: u32,
    /// Clip and mask shapes, appended after the body so a clip reference resolves.
    defs: Vec<VObject>,
}

impl Import {
    fn id_for(&mut self, raw: &str) -> ObjectId {
        if raw.is_empty() {
            self.counter += 1;
            ObjectId::from(format!("obj_{}", self.counter))
        } else {
            ObjectId::from(raw)
        }
    }

    fn node(&mut self, n: &usvg::Node) -> Result<Option<VObject>> {
        Ok(match n {
            usvg::Node::Group(g) => self.group(g)?,
            usvg::Node::Path(p) => Some(self.path(p)),
            usvg::Node::Text(t) => Some(self.text(t)?),
            // Pixels cannot live in the document JSON and this entry point has no store.
            usvg::Node::Image(_) => None,
        })
    }

    fn group(&mut self, g: &usvg::Group) -> Result<Option<VObject>> {
        let mut children = Vec::new();
        for c in g.children() {
            if let Some(o) = self.node(c)? {
                children.push(o);
            }
        }
        if children.is_empty() {
            return Ok(None);
        }
        let clip = self.clip_of(g)?;
        let mask = self.mask_of(g)?;
        let blend = blend_from(g.blend_mode());
        let plain = g.id().is_empty() && clip.is_none() && mask.is_none() && blend.is_normal();
        // A bare wrapper group is what `usvg` produces for `opacity` on a shape; folding it
        // back keeps export → import → export stable.
        if plain && children.len() == 1 {
            let mut c = children.pop().unwrap();
            c.opacity *= g.opacity().get();
            c.transform = Transform::from_kurbo(affine(g.transform()) * c.transform.to_kurbo());
            if let VKind::Text { origin, .. } = &mut c.kind {
                let _ = origin;
            }
            return Ok(Some(c));
        }
        let id = self.id_for(g.id());
        let mut o = VObject::new(
            id.clone(),
            display_name(&id),
            VKind::Group { objects: children },
        );
        o.transform = Transform::from_kurbo(affine(g.transform()));
        o.opacity = g.opacity().get();
        o.blend = blend;
        o.clip = clip;
        o.mask = mask;
        Ok(Some(o))
    }

    fn clip_of(&mut self, g: &usvg::Group) -> Result<Option<ObjectId>> {
        let Some(cp) = g.clip_path() else {
            return Ok(None);
        };
        let id = shape_id(cp.id(), "cp_");
        let id = match id {
            Some(i) => i,
            None => {
                self.counter += 1;
                ObjectId::from(format!("obj_clip_{}", self.counter))
            }
        };
        if !self.defs.iter().any(|d| d.id == id) {
            let mut d = BezPath::new();
            collect_group_paths(cp.root(), affine(cp.transform()), &mut d);
            let obj = VObject::new(
                id.clone(),
                display_name(&id),
                VKind::Path { d: geom::to_d(&d) },
            );
            self.defs.push(obj);
        }
        Ok(Some(id))
    }

    fn mask_of(&mut self, g: &usvg::Group) -> Result<Option<ObjectId>> {
        let Some(mk) = g.mask() else {
            return Ok(None);
        };
        let id = match shape_id(mk.id(), "mk_") {
            Some(i) => i,
            None => {
                self.counter += 1;
                ObjectId::from(format!("obj_mask_{}", self.counter))
            }
        };
        if !self.defs.iter().any(|d| d.id == id) {
            let mut sink = BezPath::new();
            collect_group_paths(mk.root(), Affine::IDENTITY, &mut sink);
            let fill = first_fill(mk.root()).unwrap_or(Paint::solid(Color::WHITE));
            let obj = VObject::new(
                id.clone(),
                display_name(&id),
                VKind::Path {
                    d: geom::to_d(&sink),
                },
            )
            .with_fill(fill);
            self.defs.push(obj);
        }
        Ok(Some(id))
    }

    fn path(&mut self, p: &usvg::Path) -> VObject {
        let id = self.id_for(p.id());
        let mut o = VObject::new(
            id.clone(),
            display_name(&id),
            VKind::Path {
                d: geom::to_d(&convert_path(p.data(), Affine::IDENTITY)),
            },
        );
        if let Some(f) = p.fill() {
            o.fill = paint_from(f.paint(), f.opacity().get());
            o.fill_rule = match f.rule() {
                usvg::FillRule::NonZero => FillRule::Nonzero,
                usvg::FillRule::EvenOdd => FillRule::Evenodd,
            };
        }
        o.stroke = p.stroke().map(stroke_from);
        o
    }

    fn text(&mut self, t: &usvg::Text) -> Result<VObject> {
        let id = self.id_for(t.id());
        let chunk = t.chunks().first();
        let flowed = chunk
            .map(|c| !matches!(c.text_flow(), usvg::TextFlow::Linear))
            .unwrap_or(false);
        if chunk.is_none() || flowed {
            // Text on a path: the link to the target object is not recoverable from the
            // resolved tree, so it lands as the outlines it renders to.
            let mut sink = BezPath::new();
            collect_group_paths(t.flattened(), Affine::IDENTITY, &mut sink);
            return Ok(VObject::new(
                id.clone(),
                display_name(&id),
                VKind::Path {
                    d: geom::to_d(&sink),
                },
            )
            .with_fill(Paint::solid(Color::BLACK)));
        }
        let chunk = chunk.unwrap();
        let span = chunk.spans().first();
        let mut spec = TextSpec::new(chunk.text());
        if let Some(s) = span {
            spec.size = s.font_size().get() as f64;
            spec.weight = s.font().weight();
            spec.italic = s.font().style() == usvg::FontStyle::Italic;
            spec.tracking = s.letter_spacing() as f64;
            if let Some(f) = s.font().families().first() {
                spec.family = match f {
                    usvg::FontFamily::Named(n) => n.clone(),
                    usvg::FontFamily::Serif => "serif".into(),
                    usvg::FontFamily::SansSerif => "sans-serif".into(),
                    usvg::FontFamily::Cursive => "cursive".into(),
                    usvg::FontFamily::Fantasy => "fantasy".into(),
                    usvg::FontFamily::Monospace => "monospace".into(),
                };
            }
        }
        spec.align = match chunk.anchor() {
            usvg::TextAnchor::Start => TextAlign::Left,
            usvg::TextAnchor::Middle => TextAlign::Center,
            usvg::TextAnchor::End => TextAlign::Right,
        };
        let baseline = chunk.y().unwrap_or(0.0) as f64;
        let ascent = text::shape(text::fonts(), &spec).metrics.ascent;
        let mut o = VObject::new(
            id.clone(),
            display_name(&id),
            VKind::Text {
                spec,
                origin: [chunk.x().unwrap_or(0.0) as f64, baseline - ascent],
                on_path: None,
            },
        );
        o.fill = span
            .and_then(|s| s.fill())
            .map(|f| paint_from(f.paint(), f.opacity().get()))
            .unwrap_or(Paint::solid(Color::BLACK));
        o.stroke = span.and_then(|s| s.stroke()).map(stroke_from);
        Ok(o)
    }
}

fn display_name(id: &ObjectId) -> String {
    id.as_str().trim_start_matches("obj_").to_string()
}

fn shape_id(raw: &str, prefix: &str) -> Option<ObjectId> {
    raw.strip_prefix(prefix).map(ObjectId::from)
}

fn first_fill(g: &usvg::Group) -> Option<Paint> {
    for c in g.children() {
        match c {
            usvg::Node::Path(p) => {
                if let Some(f) = p.fill() {
                    return Some(paint_from(f.paint(), f.opacity().get()));
                }
            }
            usvg::Node::Group(gg) => {
                if let Some(p) = first_fill(gg) {
                    return Some(p);
                }
            }
            _ => {}
        }
    }
    None
}

fn collect_group_paths(g: &usvg::Group, at: Affine, sink: &mut BezPath) {
    let at = at * affine(g.transform());
    for c in g.children() {
        match c {
            usvg::Node::Path(p) => sink.extend(convert_path(p.data(), at)),
            usvg::Node::Group(gg) => collect_group_paths(gg, at, sink),
            usvg::Node::Text(t) => collect_group_paths(t.flattened(), at, sink),
            usvg::Node::Image(_) => {}
        }
    }
}

fn convert_path(p: &usvg::tiny_skia_path::Path, at: Affine) -> BezPath {
    let mut out = BezPath::new();
    for seg in p.segments() {
        use usvg::tiny_skia_path::PathSegment as S;
        match seg {
            S::MoveTo(p) => out.move_to(pt(p)),
            S::LineTo(p) => out.line_to(pt(p)),
            S::QuadTo(a, b) => out.quad_to(pt(a), pt(b)),
            S::CubicTo(a, b, c) => out.curve_to(pt(a), pt(b), pt(c)),
            S::Close => out.close_path(),
        }
    }
    if at != Affine::IDENTITY {
        out.apply_affine(at);
    }
    out
}

fn pt(p: usvg::tiny_skia_path::Point) -> Point {
    Point::new(p.x as f64, p.y as f64)
}

fn affine(t: usvg::Transform) -> Affine {
    Affine::new([
        t.sx as f64,
        t.ky as f64,
        t.kx as f64,
        t.sy as f64,
        t.tx as f64,
        t.ty as f64,
    ])
}

fn color_from(c: usvg::Color, opacity: f32) -> Color {
    Color::rgba(
        c.red as f32 / 255.0,
        c.green as f32 / 255.0,
        c.blue as f32 / 255.0,
        opacity.clamp(0.0, 1.0),
    )
}

fn paint_from(p: &usvg::Paint, opacity: f32) -> Paint {
    match p {
        usvg::Paint::Color(c) => Paint::Solid {
            color: color_from(*c, opacity),
        },
        usvg::Paint::LinearGradient(g) => {
            let t = affine(g.transform());
            let a = t * Point::new(g.x1() as f64, g.y1() as f64);
            let b = t * Point::new(g.x2() as f64, g.y2() as f64);
            Paint::Linear {
                stops: stops_from(g.stops(), opacity),
                from: [a.x, a.y],
                to: [b.x, b.y],
            }
        }
        usvg::Paint::RadialGradient(g) => {
            let t = affine(g.transform());
            let c = t * Point::new(g.cx() as f64, g.cy() as f64);
            let f = t * Point::new(g.fx() as f64, g.fy() as f64);
            let scale = t.as_coeffs()[0].hypot(t.as_coeffs()[1]).max(1e-9);
            Paint::Radial {
                stops: stops_from(g.stops(), opacity),
                center: [c.x, c.y],
                radius: g.r().get() as f64 * scale,
                focal: if (f.x - c.x).abs() > 1e-9 || (f.y - c.y).abs() > 1e-9 {
                    Some([f.x, f.y])
                } else {
                    None
                },
            }
        }
        // A pattern fill is a rendered document in this model, which needs an asset the
        // importer cannot create; it becomes the pattern's average colour instead.
        usvg::Paint::Pattern(_) => Paint::Solid {
            color: Color::rgba(0.5, 0.5, 0.5, opacity),
        },
    }
}

fn stops_from(stops: &[usvg::Stop], opacity: f32) -> Vec<GradientStop> {
    stops
        .iter()
        .map(|s| GradientStop {
            offset: s.offset().get() as f64,
            color: color_from(s.color(), s.opacity().get() * opacity),
        })
        .collect()
}

fn stroke_from(s: &usvg::Stroke) -> Stroke {
    Stroke {
        paint: paint_from(s.paint(), s.opacity().get()),
        width: s.width().get() as f64,
        cap: match s.linecap() {
            usvg::LineCap::Butt => LineCap::Butt,
            usvg::LineCap::Round => LineCap::Round,
            usvg::LineCap::Square => LineCap::Square,
        },
        join: match s.linejoin() {
            usvg::LineJoin::Miter | usvg::LineJoin::MiterClip => LineJoin::Miter,
            usvg::LineJoin::Round => LineJoin::Round,
            usvg::LineJoin::Bevel => LineJoin::Bevel,
        },
        miter: s.miterlimit().get() as f64,
        dash: s
            .dasharray()
            .map(|d| d.iter().map(|v| *v as f64).collect())
            .unwrap_or_default(),
        dash_offset: s.dashoffset() as f64,
    }
}

fn blend_from(b: usvg::BlendMode) -> BlendMode {
    match b {
        usvg::BlendMode::Normal => BlendMode::Normal,
        usvg::BlendMode::Multiply => BlendMode::Multiply,
        usvg::BlendMode::Screen => BlendMode::Screen,
        usvg::BlendMode::Overlay => BlendMode::Overlay,
        usvg::BlendMode::Darken => BlendMode::Darken,
        usvg::BlendMode::Lighten => BlendMode::Lighten,
        usvg::BlendMode::ColorDodge => BlendMode::ColorDodge,
        usvg::BlendMode::ColorBurn => BlendMode::ColorBurn,
        usvg::BlendMode::HardLight => BlendMode::HardLight,
        usvg::BlendMode::SoftLight => BlendMode::SoftLight,
        usvg::BlendMode::Difference => BlendMode::Difference,
        usvg::BlendMode::Exclusion => BlendMode::Exclusion,
        usvg::BlendMode::Hue => BlendMode::Hue,
        usvg::BlendMode::Saturation => BlendMode::Saturation,
        usvg::BlendMode::Color => BlendMode::Color,
        usvg::BlendMode::Luminosity => BlendMode::Luminosity,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dpaint_core::doc::Document;

    fn round_trip(v: &VectorDoc) -> (String, String) {
        let one = write_doc(v).unwrap();
        let back = import_svg(&one, v.id.clone(), &v.name).unwrap();
        let two = write_doc(&back).unwrap();
        (one, two)
    }

    fn rich_doc() -> VectorDoc {
        let mut v = VectorDoc::new(DocId::from("doc_svg"), "svg", 200.0, 120.0);
        let clip = VObject::new(
            ObjectId::from("obj_clipshape"),
            "clipshape",
            VKind::Path {
                d: "M 0 0 L 100 0 L 100 100 L 0 100 Z".into(),
            },
        );
        let mut grad = VObject::new(
            ObjectId::from("obj_grad"),
            "grad",
            VKind::Path {
                d: "M 10 10 L 90 10 L 90 90 L 10 90 Z".into(),
            },
        );
        grad.fill = Paint::Linear {
            stops: vec![
                GradientStop {
                    offset: 0.0,
                    color: Color::parse("#ff0000").unwrap(),
                },
                GradientStop {
                    offset: 1.0,
                    color: Color::rgba(0.0, 0.0, 1.0, 0.5),
                },
            ],
            from: [10.0, 0.0],
            to: [90.0, 0.0],
        };
        let mut dashed = VObject::new(
            ObjectId::from("obj_dashed"),
            "dashed",
            VKind::Path {
                d: "M 10 100 L 190 100".into(),
            },
        );
        let mut st = Stroke::solid(Color::parse("#00aa33").unwrap(), 3.0);
        st.dash = vec![6.0, 2.0];
        st.dash_offset = 1.5;
        st.cap = LineCap::Round;
        dashed.stroke = Some(st);
        let mut group = VObject::new(
            ObjectId::from("obj_group"),
            "group",
            VKind::Group {
                objects: vec![grad, dashed],
            },
        );
        group.transform = Transform::translate(5.0, 7.0);
        group.opacity = 0.75;
        group.clip = Some(ObjectId::from("obj_clipshape"));
        let mut radial = VObject::new(
            ObjectId::from("obj_dot"),
            "dot",
            VKind::Path {
                d: "M 150 20 L 190 20 L 190 60 L 150 60 Z".into(),
            },
        );
        radial.fill = Paint::Radial {
            stops: vec![
                GradientStop {
                    offset: 0.0,
                    color: Color::WHITE,
                },
                GradientStop {
                    offset: 1.0,
                    color: Color::BLACK,
                },
            ],
            center: [170.0, 40.0],
            radius: 20.0,
            focal: None,
        };
        radial.fill_rule = FillRule::Evenodd;
        v.objects = vec![clip, group, radial];
        v
    }

    #[test]
    fn a_document_with_gradients_dashes_a_clip_and_a_group_round_trips_byte_for_byte() {
        let (one, two) = round_trip(&rich_doc());
        assert_eq!(
            one, two,
            "round trip is not stable\n--- first ---\n{one}\n--- second ---\n{two}"
        );
        assert!(one.contains("<defs>") && one.contains("linearGradient"));
        assert!(one.contains("clipPath") && one.contains("stroke-dasharray"));
    }

    #[test]
    fn import_recovers_the_gradient_stops_and_the_dash_pattern() {
        let v = rich_doc();
        let back = import_svg(&write_doc(&v).unwrap(), v.id.clone(), "svg").unwrap();
        let grad = back.object(&ObjectId::from("obj_grad")).unwrap();
        match &grad.fill {
            Paint::Linear { stops, from, to } => {
                assert_eq!(stops.len(), 2);
                assert!((stops[1].color.a - 0.5).abs() < 0.01, "stop alpha survives");
                assert_eq!(*from, [10.0, 0.0]);
                assert_eq!(*to, [90.0, 0.0]);
            }
            other => panic!("expected a linear gradient, got {other:?}"),
        }
        let dashed = back.object(&ObjectId::from("obj_dashed")).unwrap();
        let st = dashed.stroke.as_ref().unwrap();
        assert_eq!(st.dash, vec![6.0, 2.0]);
        assert!((st.dash_offset - 1.5).abs() < 1e-6);
        assert_eq!(st.cap, LineCap::Round);
    }

    #[test]
    fn group_structure_transform_and_clip_survive_import() {
        let v = rich_doc();
        let back = import_svg(&write_doc(&v).unwrap(), v.id.clone(), "svg").unwrap();
        let g = back.object(&ObjectId::from("obj_group")).unwrap();
        assert!(matches!(&g.kind, VKind::Group { objects } if objects.len() == 2));
        assert_eq!(g.transform.0, [1.0, 0.0, 0.0, 1.0, 5.0, 7.0]);
        assert!((g.opacity - 0.75).abs() < 1e-6);
        assert_eq!(g.clip.as_ref().unwrap().as_str(), "obj_clipshape");
        assert!(back.object(&ObjectId::from("obj_clipshape")).is_some());
    }

    #[test]
    fn a_foreign_svg_without_ids_imports_with_deterministic_ids() {
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10">
            <rect x="1" y="1" width="4" height="4" fill="#ff0000"/>
            <circle cx="7" cy="7" r="2" fill="#00ff00"/></svg>"##;
        let a = import_svg(svg, DocId::from("doc_a"), "a").unwrap();
        let b = import_svg(svg, DocId::from("doc_a"), "a").unwrap();
        assert_eq!(a.objects.len(), 2);
        assert_eq!(
            a.objects
                .iter()
                .map(|o| o.id.to_string())
                .collect::<Vec<_>>(),
            b.objects
                .iter()
                .map(|o| o.id.to_string())
                .collect::<Vec<_>>()
        );
        assert!(a.objects[0].id.as_str().starts_with("obj_"));
    }

    #[test]
    fn imported_geometry_matches_the_original_area() {
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100">
            <rect id="obj_r" x="10" y="10" width="40" height="20" fill="#000"/></svg>"##;
        let d = import_svg(svg, DocId::from("doc_a"), "a").unwrap();
        let p = geom::path_in_doc(&d, &ObjectId::from("obj_r")).unwrap();
        assert!(
            (geom::area(&p) - 800.0).abs() < 1e-6,
            "area {}",
            geom::area(&p)
        );
    }

    #[test]
    fn single_line_text_stays_editable_text_and_round_trips() {
        let mut v = VectorDoc::new(DocId::from("doc_t"), "t", 100.0, 50.0);
        let spec = TextSpec::new("Hello");
        v.objects.push(
            VObject::new(
                ObjectId::from("obj_hello"),
                "hello",
                VKind::Text {
                    spec,
                    origin: [10.0, 10.0],
                    on_path: None,
                },
            )
            .with_fill(Paint::solid(Color::BLACK)),
        );
        let (one, two) = round_trip(&v);
        assert!(one.contains("<text"), "text exports as <text>: {one}");
        assert_eq!(one, two);
        let back = import_svg(&one, v.id.clone(), "t").unwrap();
        match &back.object(&ObjectId::from("obj_hello")).unwrap().kind {
            VKind::Text { spec, origin, .. } => {
                assert_eq!(spec.text, "Hello");
                assert!((origin[1] - 10.0).abs() < 0.05, "origin y {}", origin[1]);
            }
            other => panic!("expected text, got {other:?}"),
        }
    }

    #[test]
    fn export_reads_through_a_project() {
        let v = rich_doc();
        let id = v.id.clone();
        let p = Project::new("p", Document::Vector(v));
        let s = to_svg(&p, &id).unwrap();
        assert!(s.starts_with("<svg xmlns"));
    }
}
