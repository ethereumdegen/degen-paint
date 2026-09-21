//! Lint on vector documents.
//!
//! These checks existed for raster only, because the digest measured raster only: vector
//! objects arrived with no bbox and no contrast, so there was nothing to judge. A logo is
//! a vector document, which made the one place this feedback matters most the one place it
//! was absent.

use dpaint_core::doc::{Document, VectorDoc};
use dpaint_core::{AssetStore, DocId, OpCx, Project, Registry};
use dpaint_inspect::lint::lint_document;
use dpaint_inspect::{DigestOptions, Finding};
use serde_json::json;

struct Fixture {
    project: Project,
    registry: Registry,
    assets: AssetStore,
    _tmp: tempfile::TempDir,
    doc: DocId,
}

impl Fixture {
    fn new(w: f64, h: f64) -> Self {
        let v = VectorDoc::new(DocId::from("doc_v"), "v", w, h);
        let doc = v.id.clone();
        let project = Project::new("t", Document::Vector(v));
        let mut registry = Registry::new();
        registry.extend(dpaint_vector::ops());
        let tmp = tempfile::tempdir().expect("tempdir");
        let assets = AssetStore::new(tmp.path());
        Self {
            project,
            registry,
            assets,
            _tmp: tmp,
            doc,
        }
    }

    fn op(&mut self, id: &str, args: serde_json::Value) {
        let op = self.registry.get(id).expect("op exists");
        let mut cx = OpCx::new(&self.assets);
        op.apply(&mut self.project, args, &mut cx)
            .unwrap_or_else(|e| panic!("{id} failed: {e}"));
    }

    fn findings(&self) -> Vec<Finding> {
        lint_document(
            &self.project,
            &self.doc,
            &self.assets,
            &DigestOptions::default(),
        )
        .expect("lint")
    }
}

fn rules(findings: &[Finding]) -> Vec<&str> {
    let mut r: Vec<&str> = findings.iter().map(|f| f.rule).collect();
    r.sort_unstable();
    r
}

/// Dark type on a dark plate is the mistake that ships, because it looks deliberate in the
/// editor and illegible everywhere else. The digest can measure it now, so lint can say so.
#[test]
fn unreadable_text_on_a_vector_mark_is_reported() {
    let mut f = Fixture::new(600.0, 200.0);
    f.op(
        "vector.object.add-rect",
        json!({"x": 0, "y": 0, "width": 600, "height": 200, "name": "plate", "fill": "#101319"}),
    );
    f.op(
        "vector.object.add-text",
        json!({"x": 40, "y": 60, "text": "BARELY", "size": 64, "name": "faint", "fill": "#161b24"}),
    );

    let found = f.findings();
    let low: Vec<_> = found.iter().filter(|x| x.rule == "low-contrast").collect();
    assert_eq!(low.len(), 1, "expected one low-contrast finding: {found:?}");
    assert_eq!(low[0].target, "#obj_faint");
    assert!(
        low[0].value.unwrap() < 4.5,
        "the finding carries the measured ratio: {:?}",
        low[0].value
    );

    // The same mark in a legible colour has nothing to say.
    f.op(
        "vector.style.fill",
        json!({"target": "#obj_faint", "color": "#f4f7fb"}),
    );
    assert!(
        !rules(&f.findings()).contains(&"low-contrast"),
        "white on near-black must pass: {:?}",
        f.findings()
    );
}

/// A mark that runs off the page is the other thing you cannot see from the op stream.
#[test]
fn geometry_outside_the_canvas_is_reported() {
    let mut f = Fixture::new(200.0, 200.0);
    f.op(
        "vector.object.add-rect",
        json!({"x": 900, "y": 900, "width": 50, "height": 50, "name": "gone", "fill": "#ffffff"}),
    );
    f.op(
        "vector.object.add-rect",
        json!({"x": 150, "y": 60, "width": 120, "height": 40, "name": "half", "fill": "#ffffff"}),
    );

    let found = f.findings();
    let by = |rule: &str| -> Vec<String> {
        found
            .iter()
            .filter(|x| x.rule == rule)
            .map(|x| x.target.clone())
            .collect()
    };
    assert_eq!(by("off-canvas"), vec!["#obj_gone"], "{found:?}");
    assert_eq!(by("clipped"), vec!["#obj_half"], "{found:?}");
}

/// A wordmark silently set in the fallback is a logo that is wrong everywhere it is used,
/// and nothing about the document looks broken. The digest knows; lint now says it.
#[test]
fn a_substituted_font_is_reported_on_the_object_that_asked_for_it() {
    let mut f = Fixture::new(600.0, 200.0);
    f.op(
        "vector.object.add-rect",
        json!({"x": 0, "y": 0, "width": 600, "height": 200, "name": "plate", "fill": "#101319"}),
    );
    f.op(
        "vector.object.add-text",
        json!({"x": 40, "y": 60, "text": "ORBIT", "size": 64, "name": "word",
               "family": "Definitely Not Installed", "fill": "#f4f7fb"}),
    );

    let found = f.findings();
    let sub: Vec<_> = found.iter().filter(|x| x.rule == "font-fallback").collect();
    assert_eq!(sub.len(), 1, "{found:?}");
    assert_eq!(sub[0].target, "#obj_word");
    assert!(
        sub[0].detail.contains("Definitely Not Installed"),
        "the finding names the family that was asked for: {}",
        sub[0].detail
    );
}
