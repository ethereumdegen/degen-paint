//! The GUI's view of the engine.
//!
//! One dispatch function, two shells: the Tauri app calls it in-process and the local server
//! calls it over HTTP, so the desktop app and a browser tab cannot drift. Crucially it goes
//! through the same [`Engine`] an agent uses, which is what puts human edits and agent edits
//! on one journal with one undo stack.

use dpaint_core::journal::Actor;
use dpaint_core::{AssetStore, DocId, Engine, Error, Project, Registry, Result, Workspace};
use dpaint_inspect::DigestOptions;
use dpaint_render::RenderOptions;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// Every op the engine knows, in the order the CLI and MCP see them.
pub fn registry() -> Registry {
    let mut r = Registry::new();
    r.extend(dpaint_core::ops::ops());
    r.extend(dpaint_raster::ops());
    r.extend(dpaint_vector::ops());
    r.extend(dpaint_model3d::ops());
    r.extend(dpaint_render::ops());
    r.extend(dpaint_inspect::ops());
    r
}

pub struct Studio {
    root: PathBuf,
    registry: Registry,
}

impl Studio {
    pub fn open(root: impl AsRef<Path>) -> Result<Self> {
        let ws = Workspace::open(root.as_ref())?;
        Ok(Self { root: ws.root().to_path_buf(), registry: registry() })
    }

    pub fn discover(start: impl AsRef<Path>) -> Result<Self> {
        let ws = Workspace::discover(start.as_ref())?;
        Ok(Self { root: ws.root().to_path_buf(), registry: registry() })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Reload from disk on every call: an agent may have written since the last one, and the
    /// GUI must never show a stale document.
    fn engine(&self) -> Result<Engine> {
        Ok(Engine::new(self.registry.clone(), Workspace::open(&self.root)?).as_human())
    }

    /// `method` + `params` in, JSON out. The whole GUI surface.
    pub fn dispatch(&self, method: &str, params: &Value) -> Result<Value> {
        match method {
            "state" => self.state(),
            "catalog" => Ok(self.registry.catalog()),
            "schema" => {
                let id = str_param(params, "op")?;
                let op = self.registry.get(&id)?;
                Ok(json!({ "id": op.id(), "about": op.about(), "schema": op.schema() }))
            }
            "op" => self.op(params),
            "undo" => {
                let mut e = self.engine()?;
                Ok(json!({ "op": e.undo()? }))
            }
            "redo" => {
                let mut e = self.engine()?;
                Ok(json!({ "op": e.redo()? }))
            }
            "digest" => self.digest(params),
            "lint" => self.lint(params),
            "history" => self.history(params),
            "select" => self.select(params),
            other => Err(Error::Invalid(format!("unknown studio method '{other}'"))),
        }
    }

    fn state(&self) -> Result<Value> {
        let mut ws = Workspace::open(&self.root)?;
        let p = &ws.project;
        let documents: Vec<Value> = p
            .documents
            .values()
            .map(|d| {
                json!({
                    "id": d.id().as_str(),
                    "name": d.name(),
                    "kind": d.kind().as_str(),
                    "size": d.size().map(|(w, h)| vec![w, h]),
                    "dependsOn": d.dependencies().iter().map(|x| x.to_string()).collect::<Vec<_>>(),
                    "objects": tree_of(p, d.id()),
                })
            })
            .collect();
        let entries = ws.journal.load()?;
        let seq = entries.last().map(|e| e.seq).unwrap_or(0);
        Ok(json!({
            "project": { "name": p.name, "root": self.root.display().to_string(),
                         "active": p.active.as_str(), "modified": p.modified },
            "documents": documents,
            "palette": p.palette.iter().map(|(k, v)| (k.clone(), v.to_hex()))
                        .collect::<std::collections::BTreeMap<_, _>>(),
            "canUndo": entries.iter().any(|e| !e.undone),
            "canRedo": entries.iter().any(|e| e.undone),
            // A revision the UI can poll cheaply to notice an agent's writes.
            "revision": seq,
        }))
    }

    fn op(&self, params: &Value) -> Result<Value> {
        let id = str_param(params, "op")?;
        let args = params.get("args").cloned().unwrap_or(json!({}));
        let doc = params.get("doc").and_then(|d| d.as_str()).map(String::from);
        let dry = params.get("dryRun").and_then(|d| d.as_bool()).unwrap_or(false);
        let mut e = self.engine()?;
        let applied = e.apply(&id, args, doc, dry)?;
        Ok(serde_json::to_value(applied)?)
    }

    fn digest(&self, params: &Value) -> Result<Value> {
        let ws = Workspace::open(&self.root)?;
        let doc = ws.project.resolve_doc(params.get("doc").and_then(|d| d.as_str()))?;
        let opts = DigestOptions {
            per_object: !params.get("fast").and_then(|f| f.as_bool()).unwrap_or(false),
            render: RenderOptions {
                scale: params.get("scale").and_then(|s| s.as_f64()).unwrap_or(1.0),
                ..Default::default()
            },
            ..Default::default()
        };
        let d = dpaint_inspect::digest::digest(&ws.project, &doc, &AssetStore::new(&self.root), &opts)?;
        Ok(serde_json::to_value(d)?)
    }

    fn lint(&self, params: &Value) -> Result<Value> {
        let ws = Workspace::open(&self.root)?;
        let assets = AssetStore::new(&self.root);
        let opts = DigestOptions::default();
        let report = match params.get("doc").and_then(|d| d.as_str()) {
            Some(d) => {
                let id = ws.project.resolve_doc(Some(d))?;
                let findings = dpaint_inspect::lint::lint_document(&ws.project, &id, &assets, &opts)?;
                let errors = findings.iter().filter(|f| f.severity == dpaint_inspect::Severity::Error).count();
                let warnings = findings.iter().filter(|f| f.severity == dpaint_inspect::Severity::Warn).count();
                dpaint_inspect::Report { findings, errors, warnings }
            }
            None => dpaint_inspect::lint::lint_project(&ws.project, &assets, &opts)?,
        };
        Ok(serde_json::to_value(report)?)
    }

    /// Journal entries newest first, with the actor, so a human can see what an agent did
    /// and an agent can see what the human did.
    fn history(&self, params: &Value) -> Result<Value> {
        let mut ws = Workspace::open(&self.root)?;
        let limit = params.get("limit").and_then(|l| l.as_u64()).unwrap_or(50) as usize;
        let entries = ws.journal.load()?;
        let rows: Vec<Value> = entries
            .iter()
            .rev()
            .take(limit)
            .map(|e| {
                json!({
                    "seq": e.seq, "ts": e.ts, "op": e.op, "undone": e.undone,
                    "actor": match e.actor { Actor::Human => "human", Actor::Agent => "agent", Actor::Replay => "replay" },
                    "changed": e.effect.as_ref().map(|f| f.changed.iter().map(|d| d.to_string()).collect::<Vec<_>>()).unwrap_or_default(),
                })
            })
            .collect();
        Ok(json!({ "entries": rows }))
    }

    fn select(&self, params: &Value) -> Result<Value> {
        let ws = Workspace::open(&self.root)?;
        let doc = ws.project.resolve_doc(params.get("doc").and_then(|d| d.as_str()))?;
        let sel = str_param(params, "selector")?;
        Ok(serde_json::to_value(dpaint_core::selector::resolve(&ws.project, &sel, Some(&doc))?)?)
    }

    /// Render a document to PNG bytes for the viewport.
    pub fn render_png(&self, doc: Option<&str>, scale: f64, max_side: u32) -> Result<(Vec<u8>, [u32; 2])> {
        let ws = Workspace::open(&self.root)?;
        let id = ws.project.resolve_doc(doc)?;
        // Fit the viewport request to a sane pixel budget so a 300 DPI poster does not
        // push 40 MB through the bridge on every keystroke.
        let opts = match ws.project.doc(&id)?.size() {
            Some((w, h)) if w.max(h) * scale > max_side as f64 => {
                let k = max_side as f64 / w.max(h);
                RenderOptions {
                    size: Some(((w * k).round().max(1.0) as u32, (h * k).round().max(1.0) as u32)),
                    ..Default::default()
                }
            }
            _ => RenderOptions { scale, ..Default::default() },
        };
        let pm = dpaint_render::render_document(&ws.project, &id, &AssetStore::new(&self.root), &opts)?;
        let size = [pm.width(), pm.height()];
        let png = dpaint_render::encode::encode(
            &dpaint_render::encode::to_rgba(&pm),
            dpaint_render::ImageFormat::Png,
            100,
        )?;
        Ok((png, size))
    }
}

fn tree_of(p: &Project, doc: &DocId) -> Vec<Value> {
    let Ok(d) = p.doc(doc) else { return Vec::new() };
    dpaint_core::selector::candidates(d)
        .into_iter()
        .map(|c| {
            json!({
                "id": c.id,
                "name": c.name,
                "type": c.type_name,
                "category": c.category,
                "depth": c.depth,
                "visible": c.attrs.get("visible").and_then(|v| v.as_bool()).unwrap_or(true),
                "opacity": c.attrs.get("opacity").and_then(|v| v.as_f64()).unwrap_or(1.0),
                "blend": c.attrs.get("blend").and_then(|v| v.as_str()).unwrap_or("normal"),
            })
        })
        .collect()
}

fn str_param(params: &Value, key: &str) -> Result<String> {
    params
        .get(key)
        .and_then(|v| v.as_str())
        .map(String::from)
        .ok_or_else(|| Error::Invalid(format!("missing '{key}'")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use dpaint_core::doc::{Document, RasterDoc};

    fn studio() -> (tempfile::TempDir, Studio) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("p.dpaint");
        let project = Project::new(
            "demo",
            Document::Raster(RasterDoc::new(DocId::from("doc_main"), "main", 64, 48)),
        );
        Workspace::create(&root, project).unwrap();
        let s = Studio::open(&root).unwrap();
        (tmp, s)
    }

    #[test]
    fn state_reports_documents_their_trees_and_undo_availability() {
        let (_t, s) = studio();
        let st = s.dispatch("state", &json!({})).unwrap();
        assert_eq!(st["documents"][0]["kind"], "raster");
        assert_eq!(st["documents"][0]["size"], json!([64.0, 48.0]));
        assert_eq!(st["canUndo"], false);
        assert_eq!(st["revision"], 0);

        s.dispatch("op", &json!({ "op": "raster.layer.add", "args": { "type": "fill", "color": "#ff0000", "name": "bg" } }))
            .unwrap();

        let st = s.dispatch("state", &json!({})).unwrap();
        assert_eq!(st["documents"][0]["objects"][0]["name"], "bg");
        assert_eq!(st["canUndo"], true);
        assert_eq!(st["revision"], 1, "the revision must advance so the UI notices writes");
    }

    #[test]
    fn gui_edits_are_journaled_as_human_and_are_undoable_by_anyone() {
        let (_t, s) = studio();
        s.dispatch("op", &json!({ "op": "raster.layer.add", "args": { "type": "fill", "color": "#00ff00", "name": "bg" } }))
            .unwrap();

        let h = s.dispatch("history", &json!({})).unwrap();
        assert_eq!(h["entries"][0]["actor"], "human", "GUI edits must be attributable");
        assert_eq!(h["entries"][0]["op"], "raster.layer.add");

        assert_eq!(s.dispatch("undo", &json!({})).unwrap()["op"], "raster.layer.add");
        let st = s.dispatch("state", &json!({})).unwrap();
        assert_eq!(st["documents"][0]["objects"].as_array().unwrap().len(), 0);
        assert_eq!(st["canRedo"], true);
    }

    #[test]
    fn the_viewport_render_is_a_real_png_of_the_document() {
        let (_t, s) = studio();
        s.dispatch("op", &json!({ "op": "raster.layer.add", "args": { "type": "fill", "color": "#0000ff", "name": "bg" } }))
            .unwrap();
        let (png, size) = s.render_png(None, 1.0, 4096).unwrap();
        assert_eq!(size, [64, 48]);
        assert_eq!(&png[1..4], b"PNG");
    }

    #[test]
    fn oversized_documents_are_fitted_to_the_viewport_budget() {
        let (_t, s) = studio();
        s.dispatch("op", &json!({ "op": "doc.resize", "args": { "width": 4000, "height": 2000 } }))
            .unwrap();
        let (_png, size) = s.render_png(None, 1.0, 800).unwrap();
        assert_eq!(size, [800, 400], "aspect must be preserved while fitting the budget");
    }

    #[test]
    fn a_failed_op_returns_a_structured_error_and_changes_nothing() {
        let (_t, s) = studio();
        let err = s
            .dispatch("op", &json!({ "op": "raster.layer.set", "args": { "target": "#nope", "opacity": 0.5 } }))
            .unwrap_err();
        assert_eq!(err.code(), "selector_no_match");
        assert_eq!(s.dispatch("state", &json!({})).unwrap()["revision"], 0);
    }

    #[test]
    fn the_catalog_and_per_op_schemas_are_available_to_build_forms_from() {
        let (_t, s) = studio();
        let cat = s.dispatch("catalog", &json!({})).unwrap();
        assert!(cat.as_array().unwrap().len() > 150);
        let sc = s.dispatch("schema", &json!({ "op": "raster.filter.gaussian-blur" })).unwrap();
        assert_eq!(sc["schema"]["properties"]["sigma"]["type"], "number");
    }

    #[test]
    fn an_agents_write_is_visible_to_the_next_gui_call() {
        let (_t, s) = studio();
        // Simulate an agent writing through its own Engine against the same directory.
        let mut agent = Engine::new(registry(), Workspace::open(s.root()).unwrap());
        agent
            .apply("raster.layer.add", json!({ "type": "fill", "color": "#123456", "name": "agent-bg" }), None, false)
            .unwrap();

        let st = s.dispatch("state", &json!({})).unwrap();
        assert_eq!(st["documents"][0]["objects"][0]["name"], "agent-bg");
        assert_eq!(st["revision"], 1);
        let h = s.dispatch("history", &json!({})).unwrap();
        assert_eq!(h["entries"][0]["actor"], "agent");
    }
}
