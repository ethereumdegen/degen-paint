//! Inspection: the feedback channel that lets an agent work without seeing.
//!
//! [`digest`] answers "what is actually in this render", [`lint`] answers "what is wrong with
//! it", [`diff`] answers "did I change only what I meant", and [`annotate`] gives a vision
//! model stable handles to point at.

pub mod annotate;
pub mod diff;
pub mod digest;
pub mod lint;

use dpaint_core::{parse_args, schema_for, Op, OpCx, OpEffect, Project, Result};
use serde::Deserialize;

pub use diff::Diff;
pub use digest::{Digest, DigestOptions};
pub use lint::{Finding, Report, Severity};

pub fn ops() -> Vec<Box<dyn Op>> {
    vec![
        Box::new(InspectDigest),
        Box::new(InspectTree),
        Box::new(LintRun),
    ]
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct DigestArgs {
    /// Document to inspect; defaults to the active one.
    #[serde(default)]
    pub document: Option<String>,
    /// Render scale used for the measurement pass.
    #[serde(default = "one")]
    pub scale: f64,
    /// Skip per-object bounds and coverage, which need one render per object.
    #[serde(default)]
    pub fast: bool,
}

fn one() -> f64 {
    1.0
}

pub struct InspectDigest;

impl Op for InspectDigest {
    fn id(&self) -> &'static str {
        "inspect.digest"
    }
    fn about(&self) -> &'static str {
        "Measure a render: tree, bounds, coverage, colors, histogram, contrast"
    }
    fn schema(&self) -> serde_json::Value {
        schema_for::<DigestArgs>()
    }
    fn is_query(&self) -> bool {
        true
    }
    fn apply(&self, p: &mut Project, args: serde_json::Value, cx: &mut OpCx) -> Result<OpEffect> {
        let a: DigestArgs = parse_args(self.id(), args)?;
        let doc = match &a.document {
            Some(d) => p.resolve_doc(Some(d))?,
            None => cx.target_doc(p)?,
        };
        let opts = DigestOptions {
            per_object: !a.fast,
            render: dpaint_render::RenderOptions {
                scale: a.scale,
                ..Default::default()
            },
            ..Default::default()
        };
        let d = digest::digest(p, &doc, cx.assets, &opts)?;
        Ok(OpEffect::default().with_data(serde_json::to_value(d)?))
    }
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct TreeArgs {
    #[serde(default)]
    pub document: Option<String>,
    /// Restrict to objects matching a selector.
    #[serde(default)]
    pub select: Option<String>,
}

pub struct InspectTree;

impl Op for InspectTree {
    fn id(&self) -> &'static str {
        "inspect.tree"
    }
    fn about(&self) -> &'static str {
        "List a document's objects, or exactly what a selector resolves to"
    }
    fn schema(&self) -> serde_json::Value {
        schema_for::<TreeArgs>()
    }
    fn is_query(&self) -> bool {
        true
    }
    fn apply(&self, p: &mut Project, args: serde_json::Value, cx: &mut OpCx) -> Result<OpEffect> {
        let a: TreeArgs = parse_args(self.id(), args)?;
        let doc = match &a.document {
            Some(d) => p.resolve_doc(Some(d))?,
            None => cx.target_doc(p)?,
        };
        let data = match &a.select {
            Some(sel) => serde_json::to_value(dpaint_core::selector::resolve(p, sel, Some(&doc))?)?,
            None => serde_json::Value::Array(
                dpaint_core::selector::candidates(p.doc(&doc)?)
                    .into_iter()
                    .map(|c| {
                        serde_json::json!({
                            "id": c.id,
                            "name": c.name,
                            "type": c.type_name,
                            "depth": c.depth,
                        })
                    })
                    .collect(),
            ),
        };
        Ok(OpEffect::default()
            .with_data(serde_json::json!({ "document": doc.as_str(), "objects": data })))
    }
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct LintArgs {
    /// Limit to one document; omit to lint the whole project.
    #[serde(default)]
    pub document: Option<String>,
    #[serde(default)]
    pub fast: bool,
}

pub struct LintRun;

impl Op for LintRun {
    fn id(&self) -> &'static str {
        "lint.run"
    }
    fn about(&self) -> &'static str {
        "Check for off-canvas content, text overflow, contrast failures and broken geometry"
    }
    fn schema(&self) -> serde_json::Value {
        schema_for::<LintArgs>()
    }
    fn is_query(&self) -> bool {
        true
    }
    fn apply(&self, p: &mut Project, args: serde_json::Value, cx: &mut OpCx) -> Result<OpEffect> {
        let a: LintArgs = parse_args(self.id(), args)?;
        let opts = DigestOptions {
            per_object: !a.fast,
            ..Default::default()
        };
        let report = match &a.document {
            Some(d) => {
                let id = p.resolve_doc(Some(d))?;
                let findings = lint::lint_document(p, &id, cx.assets, &opts)?;
                let errors = findings
                    .iter()
                    .filter(|f| f.severity == Severity::Error)
                    .count();
                let warnings = findings
                    .iter()
                    .filter(|f| f.severity == Severity::Warn)
                    .count();
                Report {
                    findings,
                    errors,
                    warnings,
                }
            }
            None => lint::lint_project(p, cx.assets, &opts)?,
        };
        Ok(OpEffect::default().with_data(serde_json::to_value(report)?))
    }
}
