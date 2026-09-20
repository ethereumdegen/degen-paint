//! The tool handler table.
//!
//! Op tools are executed by this crate against the project's engine. The five loop tools are
//! closures: `dpaint_render` and `dpaint_lint` come from the renderer and the inspector,
//! which this crate must not depend on — so the caller injects them and there is no cycle.

use crate::tools::loop_tool;
use dpaint_core::{Engine, Error, Project, Result};
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::Arc;

/// A loop-tool implementation: JSON in, JSON out.
pub type Handler = Box<dyn Fn(Value) -> Result<Value> + Send + Sync>;

/// Executes one registry op: `(op id, arguments, target document, dry run)`.
pub type OpExec = Box<dyn Fn(&str, Value, Option<String>, bool) -> Result<Value> + Send + Sync>;

#[derive(Default)]
pub struct Handlers {
    tools: BTreeMap<String, (Value, Handler)>,
    ops: Option<OpExec>,
}

impl Handlers {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register one of the documented loop tools, e.g. `dpaint_render`.
    pub fn with(mut self, name: &str, handler: Handler) -> Self {
        let spec = loop_tool(name).unwrap_or_else(|| {
            json!({
                "name": name,
                "description": format!("Host-provided tool '{name}'."),
                "inputSchema": { "type": "object" }
            })
        });
        self.tools.insert(name.to_string(), (spec, handler));
        self
    }

    /// Register a tool with a schema of your own.
    pub fn with_tool(mut self, spec: Value, handler: Handler) -> Self {
        let name = spec["name"].as_str().unwrap_or_default().to_string();
        self.tools.insert(name, (spec, handler));
        self
    }

    /// Install the op executor: how `tools/call` runs a registry op.
    pub fn with_ops(mut self, exec: OpExec) -> Self {
        self.ops = Some(exec);
        self
    }

    pub fn has(&self, name: &str) -> bool {
        self.tools.contains_key(name)
    }

    pub fn specs(&self) -> Vec<Value> {
        self.tools.values().map(|(spec, _)| spec.clone()).collect()
    }

    pub fn call(&self, name: &str, args: Value) -> Option<Result<Value>> {
        self.tools.get(name).map(|(_, h)| h(args))
    }

    pub fn can_run_ops(&self) -> bool {
        self.ops.is_some()
    }

    pub fn run_op(&self, id: &str, args: Value, doc: Option<String>, dry: bool) -> Result<Value> {
        match &self.ops {
            Some(exec) => exec(id, args, doc, dry),
            None => Err(Error::Invalid(
                "this server has no project open, so ops cannot be executed".into(),
            )),
        }
    }

    /// Back the op executor and any unfilled loop tool with a live workspace. Handlers the
    /// caller already supplied are left alone, so an injected `dpaint_render` wins.
    pub fn backed_by(mut self, engine: Arc<Mutex<Engine>>) -> Self {
        if self.ops.is_none() {
            let e = engine.clone();
            self.ops = Some(Box::new(move |id, args, doc, dry| {
                let applied = e.lock().apply(id, args, doc, dry)?;
                Ok(serde_json::to_value(applied)?)
            }));
        }
        if !self.has("dpaint_overview") {
            let e = engine.clone();
            self = self.with("dpaint_overview", Box::new(move |args| overview(&e, args)));
        }
        if !self.has("dpaint_apply") {
            let e = engine.clone();
            self = self.with("dpaint_apply", Box::new(move |args| apply_batch(&e, args)));
        }
        if !self.has("dpaint_history") {
            let e = engine.clone();
            self = self.with("dpaint_history", Box::new(move |args| history(&e, args)));
        }
        self
    }
}

fn overview(engine: &Arc<Mutex<Engine>>, args: Value) -> Result<Value> {
    let mut engine = engine.lock();
    let only = args.get("doc").and_then(Value::as_str).map(str::to_string);
    let recent: Vec<Value> = {
        let entries = engine.workspace.journal.load()?;
        entries
            .iter()
            .rev()
            .take(5)
            .map(|e| json!({ "seq": e.seq, "op": e.op, "at": e.ts, "actor": e.actor, "undone": e.undone }))
            .collect()
    };
    let project: &Project = &engine.workspace.project;
    let documents: Vec<Value> = project
        .documents
        .values()
        .filter(|d| match &only {
            Some(want) => d.id().as_str() == want || d.name() == want,
            None => true,
        })
        .map(describe_document)
        .collect();

    Ok(json!({
        "project": {
            "id": project.id,
            "name": project.name,
            "active": project.active,
            "modified": project.modified,
        },
        "documents": documents,
        "assets": {
            "referenced": project.referenced_assets().len(),
            "onDisk": engine.workspace.assets.list().map(|a| a.len()).unwrap_or(0),
        },
        "history": { "entries": engine.workspace.journal.entries().len(), "recent": recent },
    }))
}

fn describe_document(doc: &dpaint_core::Document) -> Value {
    let mut v = json!({
        "id": doc.id(),
        "name": doc.name(),
        "kind": doc.kind().as_str(),
    });
    if let Some((w, h)) = doc.size() {
        v["size"] = json!([w, h]);
    }
    match doc {
        dpaint_core::Document::Raster(d) => {
            v["layers"] = json!(d.walk().len());
            v["hasSelection"] = json!(d.selection.is_some());
        }
        dpaint_core::Document::Vector(d) => {
            v["objects"] = json!(d.walk().len());
            v["artboards"] = json!(d.artboards.len());
        }
        dpaint_core::Document::Model(d) => {
            v["nodes"] = json!(d.nodes.len());
            v["meshes"] = json!(d.meshes.len());
            v["materials"] = json!(d.materials.len());
        }
    }
    v
}

fn apply_batch(engine: &Arc<Mutex<Engine>>, args: Value) -> Result<Value> {
    let list = args
        .get("ops")
        .and_then(Value::as_array)
        .ok_or_else(|| Error::SchemaViolation {
            op: "dpaint_apply".into(),
            detail: "expected an 'ops' array".into(),
        })?;
    if list.is_empty() {
        return Err(Error::SchemaViolation {
            op: "dpaint_apply".into(),
            detail: "'ops' is empty".into(),
        });
    }
    let mut batch = Vec::with_capacity(list.len());
    for (i, entry) in list.iter().enumerate() {
        let id = entry
            .get("op")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::SchemaViolation {
                op: "dpaint_apply".into(),
                detail: format!("ops[{i}] has no 'op' id"),
            })?
            .to_string();
        let op_args = entry.get("args").cloned().unwrap_or_else(|| json!({}));
        let doc = entry.get("doc").and_then(Value::as_str).map(str::to_string);
        batch.push((id, op_args, doc));
    }
    let dry = args.get("dryRun").and_then(Value::as_bool).unwrap_or(false);
    let applied = engine.lock().apply_batch(batch, dry)?;
    Ok(json!({ "ok": true, "dryRun": dry, "applied": applied }))
}

fn history(engine: &Arc<Mutex<Engine>>, args: Value) -> Result<Value> {
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(20)
        .clamp(1, 500) as usize;
    let mut engine = engine.lock();
    let entries = engine.workspace.journal.load()?;
    let start = entries.len().saturating_sub(limit);
    let recent: Vec<Value> = entries[start..]
        .iter()
        .map(|e| {
            json!({
                "seq": e.seq,
                "at": e.ts,
                "actor": e.actor,
                "op": e.op,
                "args": e.args,
                "effect": e.effect,
                "undone": e.undone,
            })
        })
        .collect();
    Ok(json!({ "total": entries.len(), "entries": recent }))
}
