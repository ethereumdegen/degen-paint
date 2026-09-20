//! The op registry.
//!
//! Every mutation is an op with a JSON Schema. The CLI verbs, the MCP tools, the GUI command
//! surface, undo/redo, the journal and the generated docs are all derived from this one
//! definition — so adding a feature is adding one op, and the surfaces cannot drift.

use crate::asset::AssetStore;
use crate::doc::DocKind;
use crate::error::{Error, Result};
use crate::ids::DocId;
use crate::project::Project;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Arc;

/// What an op did. Returned to the caller and stored in the journal.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OpEffect {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changed: Vec<DocId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub created: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub removed: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<Warning>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    /// Payload for query ops (measurements, digests, provider status).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

impl OpEffect {
    pub fn changed(doc: &DocId) -> Self {
        Self { changed: vec![doc.clone()], ..Default::default() }
    }

    pub fn with_created(mut self, id: impl Into<String>) -> Self {
        self.created.push(id.into());
        self
    }

    pub fn with_removed(mut self, id: impl Into<String>) -> Self {
        self.removed.push(id.into());
        self
    }

    pub fn with_data(mut self, v: serde_json::Value) -> Self {
        self.data = Some(v);
        self
    }

    pub fn warn(mut self, code: &str, target: impl Into<String>, detail: impl Into<String>) -> Self {
        self.warnings.push(Warning {
            code: code.to_string(),
            target: target.into(),
            detail: detail.into(),
        });
        self
    }
}

/// Non-fatal problems ride along with a successful result instead of being printed and lost.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Warning {
    pub code: String,
    pub target: String,
    pub detail: String,
}

/// Execution context handed to an op: asset store, the document it should default to,
/// and the dry-run flag.
pub struct OpCx<'a> {
    pub assets: &'a AssetStore,
    pub doc_hint: Option<String>,
    pub dry_run: bool,
}

impl<'a> OpCx<'a> {
    pub fn new(assets: &'a AssetStore) -> Self {
        Self { assets, doc_hint: None, dry_run: false }
    }

    pub fn with_doc(mut self, doc: Option<String>) -> Self {
        self.doc_hint = doc;
        self
    }

    /// The document this op targets: explicit `--doc`, else the project's active document.
    pub fn target_doc(&self, p: &Project) -> Result<DocId> {
        p.resolve_doc(self.doc_hint.as_deref())
    }
}

pub trait Op: Send + Sync {
    /// Stable dotted identifier, e.g. `raster.filter.gaussian-blur`.
    fn id(&self) -> &'static str;

    /// One-line summary, used by `--help`, MCP tool descriptions and generated docs.
    fn about(&self) -> &'static str;

    /// JSON Schema for the argument object.
    fn schema(&self) -> serde_json::Value;

    /// Document kinds this op applies to. Empty means "any".
    fn modes(&self) -> &'static [DocKind] {
        &[]
    }

    /// Query ops never mutate and are never journaled.
    fn is_query(&self) -> bool {
        false
    }

    /// Ops that reach the network, so `--offline` and budget checks can gate them.
    fn is_network(&self) -> bool {
        false
    }

    fn apply(&self, project: &mut Project, args: serde_json::Value, cx: &mut OpCx) -> Result<OpEffect>;
}

#[derive(Default, Clone)]
pub struct Registry {
    ops: BTreeMap<&'static str, Arc<dyn Op>>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, op: impl Op + 'static) -> &mut Self {
        let op: Arc<dyn Op> = Arc::new(op);
        if self.ops.insert(op.id(), op.clone()).is_some() {
            panic!("duplicate op id '{}'", op.id());
        }
        self
    }

    pub fn extend(&mut self, ops: Vec<Box<dyn Op>>) -> &mut Self {
        for op in ops {
            let op: Arc<dyn Op> = Arc::from(op);
            if self.ops.insert(op.id(), op.clone()).is_some() {
                panic!("duplicate op id '{}'", op.id());
            }
        }
        self
    }

    pub fn get(&self, id: &str) -> Result<Arc<dyn Op>> {
        self.ops
            .get(id)
            .cloned()
            .ok_or_else(|| Error::UnknownOp(id.to_string()))
    }

    pub fn contains(&self, id: &str) -> bool {
        self.ops.contains_key(id)
    }

    pub fn len(&self) -> usize {
        self.ops.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }

    pub fn ids(&self) -> Vec<&'static str> {
        self.ops.keys().copied().collect()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Arc<dyn Op>> {
        self.ops.values()
    }

    /// Ops whose id starts with `prefix`, e.g. all of `raster.filter.`.
    pub fn with_prefix(&self, prefix: &str) -> Vec<&Arc<dyn Op>> {
        self.ops
            .iter()
            .filter(|(k, _)| k.starts_with(prefix))
            .map(|(_, v)| v)
            .collect()
    }

    /// Machine-readable catalog: what `dpaint op --list --json` and MCP tool discovery serve.
    pub fn catalog(&self) -> serde_json::Value {
        serde_json::Value::Array(
            self.ops
                .values()
                .map(|op| {
                    serde_json::json!({
                        "id": op.id(),
                        "about": op.about(),
                        "modes": op.modes().iter().map(|m| m.as_str()).collect::<Vec<_>>(),
                        "query": op.is_query(),
                        "network": op.is_network(),
                        "schema": op.schema(),
                    })
                })
                .collect(),
        )
    }
}

/// Deserialize an op's arguments, turning a serde error into a structured schema violation
/// that names the op — an agent needs to know *which* call was wrong.
pub fn parse_args<T: serde::de::DeserializeOwned>(op: &str, args: serde_json::Value) -> Result<T> {
    serde_json::from_value(args).map_err(|e| Error::SchemaViolation {
        op: op.to_string(),
        detail: e.to_string(),
    })
}

/// Schema for a `#[derive(JsonSchema)]` argument type.
pub fn schema_for<T: schemars::JsonSchema>() -> serde_json::Value {
    schemars::schema_for!(T).to_value()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::{Document, RasterDoc};

    struct SetDpi;

    #[derive(serde::Deserialize, schemars::JsonSchema)]
    struct SetDpiArgs {
        dpi: f32,
    }

    impl Op for SetDpi {
        fn id(&self) -> &'static str {
            "raster.canvas.set-dpi"
        }
        fn about(&self) -> &'static str {
            "Set output resolution"
        }
        fn schema(&self) -> serde_json::Value {
            schema_for::<SetDpiArgs>()
        }
        fn modes(&self) -> &'static [DocKind] {
            &[DocKind::Raster]
        }
        fn apply(&self, p: &mut Project, args: serde_json::Value, cx: &mut OpCx) -> Result<OpEffect> {
            let a: SetDpiArgs = parse_args(self.id(), args)?;
            let doc = cx.target_doc(p)?;
            p.raster_mut(&doc)?.dpi = a.dpi;
            Ok(OpEffect::changed(&doc))
        }
    }

    fn project() -> Project {
        Project::new(
            "t",
            Document::Raster(RasterDoc::new(DocId::from("doc_main"), "main", 8, 8)),
        )
    }

    #[test]
    fn registry_dispatches_and_reports_the_effect() {
        let mut reg = Registry::new();
        reg.register(SetDpi);
        let tmp = tempfile::tempdir().unwrap();
        let assets = AssetStore::new(tmp.path());
        let mut p = project();

        let effect = reg
            .get("raster.canvas.set-dpi")
            .unwrap()
            .apply(&mut p, serde_json::json!({ "dpi": 300 }), &mut OpCx::new(&assets))
            .unwrap();

        assert_eq!(effect.changed, vec![DocId::from("doc_main")]);
        assert_eq!(p.raster(&DocId::from("doc_main")).unwrap().dpi, 300.0);
    }

    #[test]
    fn bad_arguments_name_the_op_and_exit_as_a_usage_error() {
        let mut reg = Registry::new();
        reg.register(SetDpi);
        let tmp = tempfile::tempdir().unwrap();
        let assets = AssetStore::new(tmp.path());
        let mut p = project();
        let err = reg
            .get("raster.canvas.set-dpi")
            .unwrap()
            .apply(&mut p, serde_json::json!({ "dpi": "lots" }), &mut OpCx::new(&assets))
            .unwrap_err();
        assert_eq!(err.code(), "schema_violation");
        assert_eq!(err.exit_code(), 2);
        assert!(err.to_string().contains("raster.canvas.set-dpi"));
    }

    #[test]
    fn unknown_ops_are_rejected_not_ignored() {
        let reg = Registry::new();
        let err = reg.get("nope.at.all").err().expect("unknown op must not resolve");
        assert_eq!(err.code(), "unknown_op");
    }

    #[test]
    fn the_catalog_carries_a_usable_schema_for_every_op() {
        let mut reg = Registry::new();
        reg.register(SetDpi);
        let cat = reg.catalog();
        let entry = &cat[0];
        assert_eq!(entry["id"], "raster.canvas.set-dpi");
        assert_eq!(entry["modes"][0], "raster");
        assert_eq!(entry["schema"]["properties"]["dpi"]["type"], "number");
    }
}
