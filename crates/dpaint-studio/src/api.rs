//! The GUI's view of the engine.
//!
//! One dispatch function, two shells: the Tauri app calls it in-process and the local server
//! calls it over HTTP, so the desktop app and a browser tab cannot drift. Crucially it goes
//! through the same [`Engine`] an agent uses, which is what puts human edits and agent edits
//! on one journal with one undo stack.

use crate::{io, recent};
use dpaint_core::journal::Actor;
use dpaint_core::{
    AssetStore, DocId, DocKind, Document, Engine, Error, ModelDoc, Project, RasterDoc, Registry,
    Result, VectorDoc, Workspace,
};
use dpaint_inspect::DigestOptions;
use dpaint_render::RenderOptions;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

/// Every op a desktop shell knows: the mode crates plus the host-file ops of [`crate::io`].
pub fn registry() -> Registry {
    let mut r = base_registry();
    r.extend(io::ops());
    r
}

/// The catalog the [`crate::io`] ops delegate to, and the one a build with no host
/// filesystem — the wasm engine — can serve in full.
pub fn base_registry() -> Registry {
    let mut r = Registry::new();
    r.extend(dpaint_core::ops::ops());
    r.extend(dpaint_raster::ops());
    r.extend(dpaint_vector::ops());
    r.extend(dpaint_model3d::ops());
    r.extend(dpaint_render::ops());
    r.extend(dpaint_inspect::ops());
    r
}

/// The project `dpaint new` creates. One place, so the CLI verb and the Studio's New
/// Project dialog cannot produce different projects from the same answers.
pub fn create_project(
    dir: &Path,
    name: &str,
    kind: DocKind,
    w: f64,
    h: f64,
    dpi: f32,
) -> Result<Workspace> {
    if dir.join("project.json").exists() {
        return Err(Error::Exists(format!(
            "{} already holds a project; open it instead",
            dir.display()
        )));
    }
    if w <= 0.0 || h <= 0.0 {
        return Err(Error::Invalid(format!(
            "size must be positive, got {w}x{h}"
        )));
    }
    let id = DocId::from_name(name);
    let first = match kind {
        DocKind::Raster => {
            let mut d = RasterDoc::new(id, name, w as u32, h as u32);
            d.dpi = dpi;
            Document::Raster(d)
        }
        DocKind::Vector => Document::Vector(VectorDoc::new(id, name, w, h)),
        DocKind::Model => Document::Model(ModelDoc::new(id, name)),
    };
    Workspace::create(dir, Project::new(name, first))
}

pub struct Studio {
    /// A shell can run with no project at all — the Welcome screen — and swap projects
    /// without being rebuilt, so the root is state rather than a constructor argument.
    /// The HTTP bridge shares one `Studio` across threads, hence the lock.
    root: Mutex<Option<PathBuf>>,
    registry: Registry,
}

impl Studio {
    pub fn open(root: impl AsRef<Path>) -> Result<Self> {
        let studio = Self::empty();
        studio.adopt(Workspace::open(root.as_ref())?);
        Ok(studio)
    }

    pub fn discover(start: impl AsRef<Path>) -> Result<Self> {
        let studio = Self::empty();
        studio.adopt(Workspace::discover(start.as_ref())?);
        Ok(studio)
    }

    /// A shell launched with nothing open. Every method that needs a project says so.
    pub fn empty() -> Self {
        Self {
            root: Mutex::new(None),
            registry: registry(),
        }
    }

    pub fn root(&self) -> Option<PathBuf> {
        self.slot().clone()
    }

    pub fn is_open(&self) -> bool {
        self.slot().is_some()
    }

    fn slot(&self) -> MutexGuard<'_, Option<PathBuf>> {
        // Nothing but a clone happens under this lock, so a poisoned one still holds a
        // valid path and is better recovered than propagated to every later call.
        self.root.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn adopt(&self, ws: Workspace) {
        recent::record(ws.root(), &ws.project.name);
        *self.slot() = Some(ws.root().to_path_buf());
    }

    pub(crate) fn root_required(&self) -> Result<PathBuf> {
        self.root()
            .ok_or_else(|| Error::Invalid("no project is open".into()))
    }

    /// Reload from disk on every call: an agent may have written since the last one, and the
    /// GUI must never show a stale document.
    fn engine(&self) -> Result<Engine> {
        let ws = Workspace::open(self.root_required()?)?;
        Ok(Engine::new(self.registry.clone(), ws).as_human())
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
            "project.new" => self.project_new(params),
            "project.open" => self.project_open(params),
            "project.close" => self.project_close(),
            "project.recent" => Ok(json!({ "entries": recent::list() })),
            "io.import" => self.io_import(params),
            "io.export" => io::export(&mut self.engine()?, params),
            "io.sendToEditor" => io::send_to_editor(&mut self.workspace()?, params),
            "io.exportPreview" => io::export_preview(&mut self.workspace()?, params),
            // Jobs, quotes, providers and the UI contract keep to their own modules; this is
            // the only hook they need here.
            m if crate::jobs::handles(m) || crate::providers::handles(m) => {
                crate::jobs::dispatch_ext(self, m, params)
                    .or_else(|| crate::providers::dispatch_ext(self, m, params))
                    .unwrap_or_else(|| Err(Error::Invalid(format!("unknown studio method '{m}'"))))
            }
            other => Err(Error::Invalid(format!("unknown studio method '{other}'"))),
        }
    }

    /// Open the project at `path`, replacing whatever was open.
    fn project_open(&self, params: &Value) -> Result<Value> {
        self.adopt(Workspace::open(str_param(params, "path")?)?);
        self.state()
    }

    fn project_new(&self, params: &Value) -> Result<Value> {
        let dir = PathBuf::from(str_param(params, "path")?);
        let name = str_param(params, "name")?;
        let kind: DocKind = params
            .get("kind")
            .and_then(|k| k.as_str())
            .unwrap_or("raster")
            .parse()?;
        let (w, h) = size_param(params)?;
        let dpi = params.get("dpi").and_then(|d| d.as_f64()).unwrap_or(72.0) as f32;
        self.adopt(create_project(&dir, &name, kind, w, h, dpi)?);
        self.state()
    }

    /// Back to the Welcome screen. The project on disk is untouched.
    fn project_close(&self) -> Result<Value> {
        *self.slot() = None;
        self.state()
    }

    fn state(&self) -> Result<Value> {
        let Some(root) = self.root() else {
            // The Welcome screen's state: a shell with nothing open is not an error.
            return Ok(json!({
                "project": null,
                "documents": [],
                "palette": {},
                "canUndo": false,
                "canRedo": false,
                "revision": 0,
                "busy": crate::jobs::busy(),
            }));
        };
        let mut ws = Workspace::open(&root)?;
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
            "project": { "name": p.name, "root": root.display().to_string(),
                         "active": p.active.as_str(), "modified": p.modified },
            "documents": documents,
            "palette": p.palette.iter().map(|(k, v)| (k.clone(), v.to_hex()))
                        .collect::<std::collections::BTreeMap<_, _>>(),
            "canUndo": entries.iter().any(|e| !e.undone),
            "canRedo": entries.iter().any(|e| e.undone),
            // A revision the UI can poll cheaply to notice an agent's writes.
            "revision": seq,
            "busy": crate::jobs::busy(),
        }))
    }

    /// Import a file as an op, so it lands in history and one undo takes it back.
    fn io_import(&self, params: &Value) -> Result<Value> {
        let mode = params
            .get("mode")
            .and_then(|m| m.as_str())
            .unwrap_or("layer");
        let mut args = json!({ "path": str_param(params, "path")?, "mode": mode });
        if let Some(name) = params.get("name").and_then(|n| n.as_str()) {
            args["name"] = json!(name);
        }
        let doc = params.get("doc").and_then(|d| d.as_str()).map(String::from);
        let applied = self.engine()?.apply("io.import", args, doc, false)?;
        let data = applied.effect.data.unwrap_or(json!({}));
        Ok(json!({
            "created": applied.effect.created,
            "sidecar": data.get("sidecar").cloned().unwrap_or(Value::Null),
            "doc": data.get("doc").cloned().unwrap_or(Value::Null),
        }))
    }

    /// The open project, reloaded. Renders and sidecars read it; only ops write.
    fn workspace(&self) -> Result<Workspace> {
        Workspace::open(self.root_required()?)
    }

    fn op(&self, params: &Value) -> Result<Value> {
        let id = str_param(params, "op")?;
        let args = params.get("args").cloned().unwrap_or(json!({}));
        let doc = params.get("doc").and_then(|d| d.as_str()).map(String::from);
        let dry = params
            .get("dryRun")
            .and_then(|d| d.as_bool())
            .unwrap_or(false);
        let mut e = self.engine()?;
        let applied = e.apply(&id, args, doc, dry)?;
        Ok(serde_json::to_value(applied)?)
    }

    fn digest(&self, params: &Value) -> Result<Value> {
        let root = self.root_required()?;
        let ws = Workspace::open(&root)?;
        let doc = ws
            .project
            .resolve_doc(params.get("doc").and_then(|d| d.as_str()))?;
        let opts = DigestOptions {
            per_object: !params
                .get("fast")
                .and_then(|f| f.as_bool())
                .unwrap_or(false),
            render: RenderOptions {
                scale: params.get("scale").and_then(|s| s.as_f64()).unwrap_or(1.0),
                ..Default::default()
            },
            ..Default::default()
        };
        let d = dpaint_inspect::digest::digest(&ws.project, &doc, &AssetStore::new(&root), &opts)?;
        Ok(serde_json::to_value(d)?)
    }

    fn lint(&self, params: &Value) -> Result<Value> {
        let root = self.root_required()?;
        let ws = Workspace::open(&root)?;
        let assets = AssetStore::new(&root);
        let opts = DigestOptions::default();
        let report = match params.get("doc").and_then(|d| d.as_str()) {
            Some(d) => {
                let id = ws.project.resolve_doc(Some(d))?;
                let findings =
                    dpaint_inspect::lint::lint_document(&ws.project, &id, &assets, &opts)?;
                let errors = findings
                    .iter()
                    .filter(|f| f.severity == dpaint_inspect::Severity::Error)
                    .count();
                let warnings = findings
                    .iter()
                    .filter(|f| f.severity == dpaint_inspect::Severity::Warn)
                    .count();
                dpaint_inspect::Report {
                    findings,
                    errors,
                    warnings,
                }
            }
            None => dpaint_inspect::lint::lint_project(&ws.project, &assets, &opts)?,
        };
        Ok(serde_json::to_value(report)?)
    }

    /// Journal entries newest first, with the actor, so a human can see what an agent did
    /// and an agent can see what the human did.
    fn history(&self, params: &Value) -> Result<Value> {
        let mut ws = self.workspace()?;
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
        let ws = self.workspace()?;
        let doc = ws
            .project
            .resolve_doc(params.get("doc").and_then(|d| d.as_str()))?;
        let sel = str_param(params, "selector")?;
        Ok(serde_json::to_value(dpaint_core::selector::resolve(
            &ws.project,
            &sel,
            Some(&doc),
        )?)?)
    }

    /// Render a document to PNG bytes for the viewport.
    pub fn render_png(
        &self,
        doc: Option<&str>,
        scale: f64,
        max_side: u32,
    ) -> Result<(Vec<u8>, [u32; 2])> {
        let root = self.root_required()?;
        let ws = Workspace::open(&root)?;
        let id = ws.project.resolve_doc(doc)?;
        // Fit the viewport request to a sane pixel budget so a 300 DPI poster does not
        // push 40 MB through the bridge on every keystroke.
        let opts = match ws.project.doc(&id)?.size() {
            Some((w, h)) if w.max(h) * scale > max_side as f64 => {
                let k = max_side as f64 / w.max(h);
                RenderOptions {
                    size: Some((
                        (w * k).round().max(1.0) as u32,
                        (h * k).round().max(1.0) as u32,
                    )),
                    ..Default::default()
                }
            }
            _ => RenderOptions {
                scale,
                ..Default::default()
            },
        };
        let pm = dpaint_render::render_document(&ws.project, &id, &AssetStore::new(&root), &opts)?;
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

pub(crate) fn str_param(params: &Value, key: &str) -> Result<String> {
    params
        .get(key)
        .and_then(|v| v.as_str())
        .map(String::from)
        .ok_or_else(|| Error::Invalid(format!("missing '{key}'")))
}

/// `size` as the dialog's two numbers or as the CLI's `WxH`, defaulting to 1024².
fn size_param(params: &Value) -> Result<(f64, f64)> {
    match params.get("size") {
        None | Some(Value::Null) => Ok((1024.0, 1024.0)),
        Some(Value::Array(wh)) if wh.len() == 2 => {
            let n = |v: &Value| {
                v.as_f64()
                    .ok_or_else(|| Error::Invalid("size must be numbers".into()))
            };
            Ok((n(&wh[0])?, n(&wh[1])?))
        }
        Some(Value::String(s)) => {
            let (w, h) = s
                .split_once(['x', 'X', ','])
                .ok_or_else(|| Error::Invalid(format!("bad size '{s}', expected WxH")))?;
            let n = |v: &str| {
                v.trim()
                    .parse::<f64>()
                    .map_err(|_| Error::Invalid(format!("bad size '{s}', expected WxH")))
            };
            Ok((n(w)?, n(h)?))
        }
        Some(other) => Err(Error::Invalid(format!(
            "size must be [w, h] or \"WxH\", got {other}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dpaint_core::doc::{Document, RasterDoc};
    use dpaint_core::LayerId;
    use std::sync::LazyLock;

    /// Recents, `~/Movies` and `~/Pictures` are real user directories in production, so
    /// the whole suite runs against a throwaway HOME rather than the machine's.
    fn sandbox() -> &'static Path {
        static HOME: LazyLock<PathBuf> = LazyLock::new(|| {
            let dir = std::env::temp_dir().join(format!("dpaint-studio-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            std::env::set_var("HOME", &dir);
            std::env::set_var("XDG_CONFIG_HOME", dir.join("config"));
            std::env::remove_var("XDG_VIDEOS_DIR");
            std::env::remove_var("XDG_PICTURES_DIR");
            dir
        });
        &HOME
    }

    fn studio() -> (tempfile::TempDir, Studio) {
        sandbox();
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

    /// A pixel layer to render and export.
    fn fill(s: &Studio) {
        s.dispatch("op", &json!({ "op": "raster.layer.add", "args": { "type": "fill", "color": "#3366ff", "name": "bg" } }))
            .unwrap();
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
        assert_eq!(
            st["revision"], 1,
            "the revision must advance so the UI notices writes"
        );
    }

    #[test]
    fn gui_edits_are_journaled_as_human_and_are_undoable_by_anyone() {
        let (_t, s) = studio();
        s.dispatch("op", &json!({ "op": "raster.layer.add", "args": { "type": "fill", "color": "#00ff00", "name": "bg" } }))
            .unwrap();

        let h = s.dispatch("history", &json!({})).unwrap();
        assert_eq!(
            h["entries"][0]["actor"], "human",
            "GUI edits must be attributable"
        );
        assert_eq!(h["entries"][0]["op"], "raster.layer.add");

        assert_eq!(
            s.dispatch("undo", &json!({})).unwrap()["op"],
            "raster.layer.add"
        );
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
        s.dispatch(
            "op",
            &json!({ "op": "doc.resize", "args": { "width": 4000, "height": 2000 } }),
        )
        .unwrap();
        let (_png, size) = s.render_png(None, 1.0, 800).unwrap();
        assert_eq!(
            size,
            [800, 400],
            "aspect must be preserved while fitting the budget"
        );
    }

    #[test]
    fn a_failed_op_returns_a_structured_error_and_changes_nothing() {
        let (_t, s) = studio();
        let err = s
            .dispatch(
                "op",
                &json!({ "op": "raster.layer.set", "args": { "target": "#nope", "opacity": 0.5 } }),
            )
            .unwrap_err();
        assert_eq!(err.code(), "selector_no_match");
        assert_eq!(s.dispatch("state", &json!({})).unwrap()["revision"], 0);
    }

    #[test]
    fn the_catalog_and_per_op_schemas_are_available_to_build_forms_from() {
        let (_t, s) = studio();
        let cat = s.dispatch("catalog", &json!({})).unwrap();
        assert!(cat.as_array().unwrap().len() > 150);
        let sc = s
            .dispatch("schema", &json!({ "op": "raster.filter.gaussian-blur" }))
            .unwrap();
        assert_eq!(sc["schema"]["properties"]["sigma"]["type"], "number");
    }

    #[test]
    fn an_agents_write_is_visible_to_the_next_gui_call() {
        let (_t, s) = studio();
        // Simulate an agent writing through its own Engine against the same directory.
        let mut agent = Engine::new(registry(), Workspace::open(s.root().unwrap()).unwrap());
        agent
            .apply(
                "raster.layer.add",
                json!({ "type": "fill", "color": "#123456", "name": "agent-bg" }),
                None,
                false,
            )
            .unwrap();

        let st = s.dispatch("state", &json!({})).unwrap();
        assert_eq!(st["documents"][0]["objects"][0]["name"], "agent-bg");
        assert_eq!(st["revision"], 1);
        let h = s.dispatch("history", &json!({})).unwrap();
        assert_eq!(h["entries"][0]["actor"], "agent");
    }

    #[test]
    fn a_shell_with_no_project_open_reports_one_rather_than_failing() {
        sandbox();
        let s = Studio::empty();
        let st = s.dispatch("state", &json!({})).unwrap();
        assert_eq!(st["project"], Value::Null, "the Welcome screen reads this");
        assert_eq!(st["documents"].as_array().unwrap().len(), 0);
        assert_eq!(st["canUndo"], false);
        assert_eq!(st["revision"], 0);
        assert!(!s.is_open());

        // The catalog is still there — the palette works before a project exists — but
        // anything that needs documents says exactly what is missing.
        assert!(
            s.dispatch("catalog", &json!({}))
                .unwrap()
                .as_array()
                .unwrap()
                .len()
                > 150
        );
        let err = s.dispatch("history", &json!({})).unwrap_err();
        assert_eq!(err.to_string(), "no project is open");
    }

    #[test]
    fn a_project_can_be_created_closed_and_reopened_in_one_shell() {
        sandbox();
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("acme-promo.dpaint");
        let s = Studio::empty();

        let st = s
            .dispatch(
                "project.new",
                &json!({ "path": dir, "name": "acme-promo", "kind": "raster", "size": [1080, 1350] }),
            )
            .unwrap();
        assert_eq!(st["project"]["name"], "acme-promo");
        assert_eq!(st["documents"][0]["name"], "acme-promo");
        assert_eq!(st["documents"][0]["size"], json!([1080.0, 1350.0]));
        assert!(dir.join("project.json").is_file());

        let again = s
            .dispatch(
                "project.new",
                &json!({ "path": dir, "name": "acme-promo", "kind": "raster" }),
            )
            .unwrap_err();
        assert_eq!(
            again.code(),
            "exists",
            "a project is never silently replaced"
        );

        assert_eq!(
            s.dispatch("project.close", &json!({})).unwrap()["project"],
            Value::Null
        );
        assert!(dir.join("project.json").is_file(), "closing writes nothing");
        assert_eq!(
            s.dispatch("project.open", &json!({ "path": dir })).unwrap()["project"]["name"],
            "acme-promo"
        );
    }

    #[test]
    fn an_imported_take_brings_its_sidecar_prompt_in_as_provenance() {
        let (_t, s) = studio();
        let hand_off = tempfile::tempdir().unwrap();
        let png = hand_off.path().join("microphone.png");
        image::RgbaImage::from_pixel(8, 6, image::Rgba([10, 200, 30, 255]))
            .save(&png)
            .unwrap();
        std::fs::write(
            hand_off.path().join("microphone.json"),
            r#"{"prompt":"a loud microphone","model":"flux/dev","parents":["take_7"],
                "cost":0.031,"somethingDmsAddedLater":true}"#,
        )
        .unwrap();

        let out = s
            .dispatch("io.import", &json!({ "path": png, "mode": "layer" }))
            .unwrap();
        assert_eq!(out["sidecar"]["prompt"], "a loud microphone");
        assert_eq!(out["doc"], "doc_main");
        let layer = out["created"][0].as_str().unwrap().to_string();

        let saved: Project =
            serde_json::from_slice(&std::fs::read(s.root().unwrap().join("project.json")).unwrap())
                .unwrap();
        let prov = saved
            .raster(&DocId::from("doc_main"))
            .unwrap()
            .layer(&LayerId::from(layer.as_str()))
            .unwrap()
            .provenance
            .clone()
            .expect("the sidecar is recorded on the layer it describes");
        assert_eq!(prov.prompt.as_deref(), Some("a loud microphone"));
        assert_eq!(prov.model, "flux/dev");
        assert_eq!(prov.parents, vec!["take_7"]);
        assert_eq!(prov.cost_usd, Some(0.031));

        let h = s.dispatch("history", &json!({})).unwrap();
        assert_eq!(h["entries"][0]["op"], "io.import");
        assert_eq!(h["entries"][0]["actor"], "human");

        // One dialog, one journal entry: undo takes the whole import back.
        s.dispatch("undo", &json!({})).unwrap();
        let st = s.dispatch("state", &json!({})).unwrap();
        assert_eq!(st["documents"][0]["objects"].as_array().unwrap().len(), 0);

        // The same file as a document of its own is sized to the image.
        let out = s
            .dispatch("io.import", &json!({ "path": png, "mode": "document" }))
            .unwrap();
        let doc = out["doc"].as_str().unwrap().to_string();
        let st = s.dispatch("state", &json!({})).unwrap();
        let made = st["documents"]
            .as_array()
            .unwrap()
            .iter()
            .find(|d| d["id"] == json!(doc))
            .unwrap();
        assert_eq!(made["kind"], "raster");
        assert_eq!(made["size"], json!([8.0, 6.0]));
        assert_eq!(made["objects"][0]["type"], "pixel");
    }

    #[test]
    fn export_writes_the_render_and_never_clobbers_unasked() {
        let (_t, s) = studio();
        fill(&s);
        let out = tempfile::tempdir().unwrap();
        let poster = out.path().join("poster.png");

        let first = s
            .dispatch("io.export", &json!({ "path": poster, "overwrite": false }))
            .unwrap();
        assert_eq!(first["size"], json!([64, 48]));
        assert!(first["bytes"].as_u64().unwrap() > 0);
        let written = std::fs::metadata(&poster).unwrap().len();

        let err = s
            .dispatch("io.export", &json!({ "path": poster, "overwrite": false }))
            .unwrap_err();
        assert_eq!(err.code(), "exists");
        assert!(
            err.to_string().contains("overwrite"),
            "the status region has to say what unblocks it, got: {err}"
        );
        assert_eq!(std::fs::metadata(&poster).unwrap().len(), written);

        // Scale is honoured, and with permission the file is replaced.
        let second = s
            .dispatch(
                "io.export",
                &json!({ "path": poster, "scale": 2.0, "overwrite": true }),
            )
            .unwrap();
        assert_eq!(second["size"], json!([128, 96]));
    }

    #[test]
    fn send_to_editor_writes_the_file_and_a_sidecar_naming_the_revision() {
        let (_t, s) = studio();
        fill(&s);
        let editor = tempfile::tempdir().unwrap();

        let sent = s
            .dispatch(
                "io.sendToEditor",
                &json!({ "docs": ["doc_main"], "dir": editor.path(), "overwrite": false }),
            )
            .unwrap();
        let file = sent["files"][0]["path"].as_str().unwrap();
        let side = sent["files"][0]["sidecar"].as_str().unwrap();
        assert!(Path::new(file).is_file(), "{file} was not written");

        let manifest: Value = serde_json::from_slice(&std::fs::read(side).unwrap()).unwrap();
        assert_eq!(manifest["project"], "demo");
        assert_eq!(manifest["document"], "main");
        assert_eq!(manifest["kind"], "raster");
        assert_eq!(
            manifest["revision"], 1,
            "the receiving app has to know which revision it got"
        );
        assert_eq!(manifest["size"], json!([64, 48]));
        assert_eq!(manifest["digestSummary"]["objects"], 1);

        let again = s
            .dispatch(
                "io.sendToEditor",
                &json!({ "docs": ["doc_main"], "dir": editor.path(), "overwrite": false }),
            )
            .unwrap_err();
        assert_eq!(again.code(), "exists");

        // Without a directory it goes to the folder the other three apps import from.
        let sent = s
            .dispatch(
                "io.sendToEditor",
                &json!({ "docs": ["doc_main"], "overwrite": true }),
            )
            .unwrap();
        assert_eq!(
            PathBuf::from(sent["files"][0]["path"].as_str().unwrap()),
            sandbox().join("Movies/degen-paint/demo/main.png")
        );
    }

    #[test]
    fn the_export_preview_lands_where_sol_looks_and_names_the_revision() {
        let (_t, s) = studio();
        fill(&s);
        let out = s.dispatch("io.exportPreview", &json!({})).unwrap();
        let png = PathBuf::from(out["png"].as_str().unwrap());
        let annotated = PathBuf::from(out["annotated"].as_str().unwrap());

        assert_eq!(
            png.parent().unwrap(),
            sandbox().join("Pictures/degen-paint/previews"),
            "the native path's look target is fixed, not chosen per run"
        );
        assert_eq!(png.file_name().unwrap(), "demo-main-r1.png");
        assert_eq!(annotated.file_name().unwrap(), "demo-main-r1-annotated.png");
        assert!(png.is_file() && annotated.is_file());
    }

    #[test]
    fn an_svg_imports_as_editable_objects_and_says_so_when_it_cannot() {
        let (_t, s) = studio();
        let hand_off = tempfile::tempdir().unwrap();
        let svg = hand_off.path().join("mark.svg");
        std::fs::write(
            &svg,
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="40" height="20">
                <rect x="2" y="2" width="16" height="16" fill="#ff0000"/>
                <circle cx="30" cy="10" r="8" fill="#00ff00"/></svg>"##,
        )
        .unwrap();

        // The active document is raster: an SVG cannot be a pixel layer, and the error
        // has to name what would work.
        let err = s
            .dispatch("io.import", &json!({ "path": svg, "mode": "layer" }))
            .unwrap_err();
        assert!(
            err.to_string().contains("vector") && err.to_string().contains("new document"),
            "got: {err}"
        );

        let out = s
            .dispatch("io.import", &json!({ "path": svg, "mode": "document" }))
            .unwrap();
        let doc = out["doc"].as_str().unwrap().to_string();
        let shapes = |st: &Value, doc: &str| -> Vec<String> {
            st["documents"]
                .as_array()
                .unwrap()
                .iter()
                .find(|d| d["id"] == json!(doc))
                .unwrap()["objects"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|o| o["category"] == "object")
                .map(|o| o["id"].as_str().unwrap().to_string())
                .collect()
        };

        let st = s.dispatch("state", &json!({})).unwrap();
        let created = st["documents"]
            .as_array()
            .unwrap()
            .iter()
            .find(|d| d["id"] == json!(doc))
            .unwrap();
        assert_eq!(created["kind"], "vector");
        assert_eq!(created["size"], json!([40.0, 20.0]));
        assert_eq!(
            shapes(&st, &doc).len(),
            2,
            "the geometry is editable, not a referenced blob: {created}"
        );

        // Now that a vector document exists, the same file can go into it as objects.
        let into = s
            .dispatch(
                "io.import",
                &json!({ "path": svg, "mode": "layer", "doc": doc }),
            )
            .unwrap();
        assert_eq!(
            into["created"].as_array().unwrap().len(),
            3,
            "a group plus its two shapes"
        );
        let st = s.dispatch("state", &json!({})).unwrap();
        let ids = shapes(&st, &doc);
        assert_eq!(ids.len(), 5, "{ids:?}");
        let mut unique = ids.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(
            unique.len(),
            5,
            "the second import's ids collided with the first and had to be renamed: {ids:?}"
        );

        // A raster file in a vector document is a placed image object, provenance and all.
        let photo = hand_off.path().join("still.png");
        image::RgbaImage::from_pixel(12, 9, image::Rgba([1, 2, 3, 255]))
            .save(&photo)
            .unwrap();
        std::fs::write(
            hand_off.path().join("still.json"),
            r#"{"prompt":"a still","model":"flux/dev"}"#,
        )
        .unwrap();
        let placed = s
            .dispatch(
                "io.import",
                &json!({ "path": photo, "mode": "layer", "doc": doc }),
            )
            .unwrap();
        let id = placed["created"][0].as_str().unwrap().to_string();
        let saved: Project =
            serde_json::from_slice(&std::fs::read(s.root().unwrap().join("project.json")).unwrap())
                .unwrap();
        let object = saved
            .vector(&DocId::from(doc.as_str()))
            .unwrap()
            .object(&dpaint_core::ObjectId::from(id.as_str()))
            .unwrap();
        assert_eq!(object.type_name(), "image");
        assert_eq!(
            object.provenance.as_ref().unwrap().prompt.as_deref(),
            Some("a still")
        );
    }

    #[test]
    fn a_model_survives_the_round_trip_out_through_export_and_back_through_import() {
        sandbox();
        let tmp = tempfile::tempdir().unwrap();
        let s = Studio::empty();
        s.dispatch(
            "project.new",
            &json!({ "path": tmp.path().join("rig.dpaint"), "name": "rig", "kind": "model" }),
        )
        .unwrap();
        s.dispatch(
            "op",
            &json!({ "op": "model.mesh.primitive", "args": { "shape": "box", "name": "crate" } }),
        )
        .unwrap();

        let glb = tmp.path().join("crate.glb");
        s.dispatch("io.export", &json!({ "path": glb, "overwrite": false }))
            .unwrap();
        assert!(std::fs::metadata(&glb).unwrap().len() > 0);

        let back = s
            .dispatch("io.import", &json!({ "path": glb, "mode": "document" }))
            .unwrap();
        let doc = back["doc"].as_str().unwrap().to_string();
        let st = s.dispatch("state", &json!({})).unwrap();
        let created = st["documents"]
            .as_array()
            .unwrap()
            .iter()
            .find(|d| d["id"] == json!(doc))
            .unwrap();
        assert_eq!(created["kind"], "model");
        assert!(
            !created["objects"].as_array().unwrap().is_empty(),
            "the imported geometry is in the tree: {created}"
        );

        // A turntable writes its frames beside the contact sheet the path names.
        let sheet = tmp.path().join("turn.png");
        let out = s
            .dispatch(
                "io.export",
                &json!({ "path": sheet, "doc": doc, "frames": 4, "scale": 0.25, "overwrite": false }),
            )
            .unwrap();
        assert_eq!(out["size"], json!([256, 256]), "2x2 grid of 128px frames");
        assert!(tmp.path().join("turn_frames/frame_003.png").is_file());
    }
}
