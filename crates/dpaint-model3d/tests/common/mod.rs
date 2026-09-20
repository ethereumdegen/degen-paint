// Each test binary compiles this module and uses a different subset of it.
#![allow(dead_code)]

//! Shared fixtures: an asset store on a temp dir, and small projects to operate on.

use dpaint_core::doc::common::{FillRule, Paint};
use dpaint_core::doc::model::PathRef;
use dpaint_core::doc::vector::{VKind, VObject};
use dpaint_core::doc::Document;
use dpaint_core::{AssetStore, Color, DocId, ModelDoc, Project, VectorDoc};

pub fn store() -> (tempfile::TempDir, AssetStore) {
    let tmp = tempfile::tempdir().expect("temp dir");
    let store = AssetStore::new(tmp.path());
    (tmp, store)
}

/// A project whose active document is an empty model document.
pub fn model_project() -> (Project, DocId) {
    let doc = DocId::from("doc_scene");
    let project = Project::new(
        "test",
        Document::Model(ModelDoc::new(doc.clone(), "scene")),
    );
    (project, doc)
}

/// Add a vector document holding the given `(object id, path data)` pairs.
pub fn add_vector_doc(project: &mut Project, paths: &[(&str, &str)], rule: FillRule) -> DocId {
    let id = DocId::from("doc_art");
    let mut v = VectorDoc::new(id.clone(), "art", 100.0, 100.0);
    for (obj, d) in paths {
        let mut o = VObject::new(
            (*obj).into(),
            *obj,
            VKind::Path { d: (*d).to_string() },
        );
        o.fill = Paint::Solid { color: Color::WHITE };
        o.fill_rule = rule;
        v.objects.push(o);
    }
    project.add_document(Document::Vector(v));
    id
}

/// A model project plus a vector document with one object, `obj_shape`.
pub fn model_project_with_vector(d: &str) -> (Project, DocId) {
    let (mut project, doc) = model_project();
    add_vector_doc(&mut project, &[("obj_shape", d)], FillRule::Nonzero);
    (project, doc)
}

pub fn path_ref() -> PathRef {
    PathRef {
        document: DocId::from("doc_art"),
        object: "obj_shape".into(),
    }
}

/// An axis-aligned square with its corner at the origin, counter-clockwise in SVG space.
pub fn square_path(side: f64) -> String {
    format!("M 0 0 L {side} 0 L {side} {side} L 0 {side} Z")
}

/// A square ring: an outer square with a concentric square hole, both wound the same way,
/// so only the even-odd fill rule makes the inner one a hole.
pub fn ring_path(outer: f64, inner: f64) -> String {
    let lo = (outer - inner) / 2.0;
    let hi = lo + inner;
    format!(
        "M 0 0 L {outer} 0 L {outer} {outer} L 0 {outer} Z \
         M {lo} {lo} L {hi} {lo} L {hi} {hi} L {lo} {hi} Z"
    )
}

/// 2x2 PNG, so texture bindings have real bytes to embed.
pub const TINY_PNG: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x02, 0x08, 0x06, 0x00, 0x00, 0x00, 0x72, 0xb6, 0x0d,
    0x24, 0x00, 0x00, 0x00, 0x11, 0x49, 0x44, 0x41, 0x54, 0x78, 0xda, 0x63, 0x38, 0x61, 0xa3, 0xf1,
    0x1f, 0x84, 0x19, 0x60, 0x0c, 0x00, 0x4c, 0xfc, 0x08, 0xad, 0xb3, 0xb5, 0x7d, 0xd1, 0x00, 0x00,
    0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];
