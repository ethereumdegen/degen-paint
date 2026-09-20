//! The degen-paint engine, compiled into a browser tab.
//!
//! There is no server here and no filesystem. The project, its journal and every asset blob
//! live in a [`MemVfs`]; the page persists that tree to OPFS (or IndexedDB) and hands it back
//! on reload. Everything else — the op registry, the undo journal, the compositor, the digest
//! — is the same code the CLI runs, which is the whole point: the pixels in the tab and the
//! pixels from `dpaint render` come out of one implementation.
//!
//! The two entry points the UI looks for are [`DpaintEngine::dispatch`] and
//! [`DpaintEngine::render_png`]. They are shaped exactly like `dpaint_studio::api::Studio`'s
//! HTTP bridge, down to the error object, so `studio.js` needs no browser-specific branch.

use base64::Engine as _;
use dpaint_core::journal::Actor;
use dpaint_core::vfs::{MemVfs, Vfs};
use dpaint_core::{
    AssetRef, AssetStore, DocId, Document, Engine, Error, ModelDoc, Project, RasterDoc, Registry,
    Result, VectorDoc, Workspace,
};
use dpaint_inspect::DigestOptions;
use dpaint_render::RenderOptions;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use wasm_bindgen::prelude::*;

/// Where the project lives inside the in-memory tree. Arbitrary but stable: the page keys
/// its OPFS entries off the paths below it.
const ROOT: &str = "/project.dpaint";

/// Every op the engine knows. Identical to the desktop registry minus `dpaint-ai`, which
/// needs the network and an OS keychain.
fn registry() -> Registry {
    let mut r = Registry::new();
    r.extend(dpaint_core::ops::ops());
    r.extend(dpaint_raster::ops());
    r.extend(dpaint_vector::ops());
    r.extend(dpaint_model3d::ops());
    r.extend(dpaint_render::ops());
    r.extend(dpaint_inspect::ops());
    r
}

/// The whole engine, owning the in-memory tree the project lives in.
#[wasm_bindgen]
pub struct DpaintEngine {
    root: PathBuf,
    vfs: Arc<dyn Vfs>,
    registry: Registry,
}

// ------------------------------------------------------------------ construction

#[wasm_bindgen]
impl DpaintEngine {
    /// A fresh project with one 1024×1024 raster document.
    #[wasm_bindgen(constructor)]
    pub fn new() -> std::result::Result<DpaintEngine, JsValue> {
        DpaintEngine::create("untitled", "raster", 1024.0, 1024.0)
    }

    /// A fresh project whose first document is `kind` at `width`×`height`.
    pub fn create(
        name: &str,
        kind: &str,
        width: f64,
        height: f64,
    ) -> std::result::Result<DpaintEngine, JsValue> {
        console_error_panic_hook::set_once();
        let doc = first_document(name, kind, width, height).map_err(to_js)?;
        let engine = DpaintEngine::blank();
        Workspace::create_with_vfs(
            &engine.root,
            Project::new(name, doc),
            Arc::clone(&engine.vfs),
        )
        .map_err(to_js)?;
        Ok(engine)
    }

    /// Adopt an existing `project.json` — this is what a reload out of OPFS calls. Assets
    /// are restored separately with [`DpaintEngine::put_asset`], and the journal with
    /// [`DpaintEngine::restore_history`].
    pub fn load(project_json: &str) -> std::result::Result<DpaintEngine, JsValue> {
        console_error_panic_hook::set_once();
        let project: Project = serde_json::from_str(project_json)
            .map_err(|e| to_js(Error::Invalid(format!("project.json is not readable: {e}"))))?;
        let engine = DpaintEngine::blank();
        engine
            .vfs
            .create_dir_all(&engine.root.join("assets"))
            .map_err(to_js)?;
        engine
            .vfs
            .write(&engine.root.join("project.json"), project_json.as_bytes())
            .map_err(to_js)?;
        // Reopen through the normal path so the format check and the parse are the same
        // ones the CLI applies; `project` above only proves the text is JSON.
        drop(project);
        engine.workspace().map_err(to_js)?;
        Ok(engine)
    }

    /// Restore a previously persisted `history.jsonl` so undo survives a page reload.
    pub fn restore_history(&self, jsonl: &str) -> std::result::Result<(), JsValue> {
        self.vfs
            .write(&self.root.join("history.jsonl"), jsonl.as_bytes())
            .map_err(to_js)
    }
}

impl DpaintEngine {
    fn blank() -> Self {
        Self {
            root: PathBuf::from(ROOT),
            vfs: MemVfs::shared(),
            registry: registry(),
        }
    }

    /// Reopened per call, exactly like the native `Studio`: the journal and the project are
    /// read back from storage so a stale in-process copy can never be shown.
    fn workspace(&self) -> Result<Workspace> {
        Workspace::open_with_vfs(&self.root, Arc::clone(&self.vfs))
    }

    fn engine(&self) -> Result<Engine> {
        Ok(Engine::new(self.registry.clone(), self.workspace()?).as_human())
    }

    fn assets(&self) -> AssetStore {
        AssetStore::with_vfs(&self.root, Arc::clone(&self.vfs))
    }
}

fn first_document(name: &str, kind: &str, width: f64, height: f64) -> Result<Document> {
    if width <= 0.0 || height <= 0.0 {
        return Err(Error::Invalid(format!(
            "size must be positive, got {width}x{height}"
        )));
    }
    let id = DocId::from_name(name);
    Ok(match kind {
        "raster" => Document::Raster(RasterDoc::new(id, name, width as u32, height as u32)),
        "vector" => Document::Vector(VectorDoc::new(id, name, width, height)),
        "model" => Document::Model(ModelDoc::new(id, name)),
        other => {
            return Err(Error::Invalid(format!(
                "unknown document kind '{other}' (expected raster, vector or model)"
            )))
        }
    })
}

// ------------------------------------------------------------------------ bridge

/// The error object the UI already knows: `{code, message, candidates?, suggestion?}`,
/// byte-for-byte what the HTTP bridge puts in its `error` field.
fn to_js(e: Error) -> JsValue {
    let detail = serde_json::to_string(&e.detail())
        .unwrap_or_else(|_| json!({ "code": "invalid", "message": e.to_string() }).to_string());
    js_sys::JSON::parse(&detail).unwrap_or_else(|_| JsValue::from_str(&e.to_string()))
}

/// JSON in both directions. `JSON.parse`/`stringify` rather than a serde bridge so the
/// values a browser sees are the exact values the HTTP bridge sends — plain objects, not
/// `Map`s, and with `serde_json`'s key order preserved.
fn from_js(v: &JsValue) -> Result<Value> {
    if v.is_undefined() || v.is_null() {
        return Ok(json!({}));
    }
    let text = js_sys::JSON::stringify(v)
        .map(|s| String::from(s))
        .map_err(|_| Error::Invalid("params are not JSON-serializable".into()))?;
    Ok(serde_json::from_str(&text)?)
}

fn to_js_value(v: &Value) -> std::result::Result<JsValue, JsValue> {
    let text = serde_json::to_string(v).map_err(|e| to_js(Error::Json(e)))?;
    js_sys::JSON::parse(&text)
        .map_err(|_| to_js(Error::Invalid("result is not representable in JS".into())))
}

#[wasm_bindgen]
impl DpaintEngine {
    /// `window.__DPAINT_INVOKE__`. The methods are the studio API's: `state`, `catalog`,
    /// `schema`, `op`, `undo`, `redo`, `digest`, `lint`, `history`, `select`.
    pub fn dispatch(&self, method: &str, params: JsValue) -> std::result::Result<JsValue, JsValue> {
        let params = from_js(&params).map_err(to_js)?;
        let value = self.dispatch_json(method, &params).map_err(to_js)?;
        to_js_value(&value)
    }

    /// `window.__DPAINT_RENDER_URL__`. A `data:` URI because there is no origin to serve
    /// `/render.png` from.
    pub fn render_png(
        &self,
        doc: Option<String>,
        scale: f64,
        max: u32,
    ) -> std::result::Result<String, JsValue> {
        let (png, _size) = self
            .render_bytes(doc.as_deref(), scale, max)
            .map_err(to_js)?;
        let mut uri = String::from("data:image/png;base64,");
        base64::engine::general_purpose::STANDARD.encode_string(&png, &mut uri);
        Ok(uri)
    }

    /// The canonical document, for the page to persist.
    pub fn project_json(&self) -> std::result::Result<String, JsValue> {
        let bytes = self
            .vfs
            .read(&self.root.join("project.json"))
            .map_err(to_js)?;
        String::from_utf8(bytes)
            .map_err(|_| to_js(Error::Invalid("project.json is not utf-8".into())))
    }

    /// The journal, for the page to persist so undo survives a reload.
    pub fn history_jsonl(&self) -> std::result::Result<String, JsValue> {
        let path = self.root.join("history.jsonl");
        if !self.vfs.exists(&path) {
            return Ok(String::new());
        }
        let bytes = self.vfs.read(&path).map_err(to_js)?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    /// Import bytes as a content-addressed asset; returns the `blake3:…` ref. Also the
    /// restore path — putting the same bytes back yields the same ref, so the page just
    /// replays what it saved.
    pub fn put_asset(&self, bytes: &[u8], ext: &str) -> std::result::Result<String, JsValue> {
        Ok(self.assets().put(bytes, ext).map_err(to_js)?.0)
    }

    /// Every blob in the store, so the page knows what to write out.
    pub fn asset_refs(&self) -> std::result::Result<Vec<String>, JsValue> {
        Ok(self
            .assets()
            .list()
            .map_err(to_js)?
            .into_iter()
            .map(|r| r.0)
            .collect())
    }

    pub fn asset_bytes(&self, asset: &str) -> std::result::Result<Vec<u8>, JsValue> {
        self.assets()
            .get(&AssetRef(asset.to_string()))
            .map_err(to_js)
    }

    /// Bytes currently held in memory — the page shows this next to its storage notice.
    pub fn storage_bytes(&self) -> f64 {
        self.assets()
            .list()
            .map(|refs| {
                refs.iter()
                    .filter_map(|r| self.assets().size_of(r).ok())
                    .sum::<u64>() as f64
            })
            .unwrap_or(0.0)
    }
}

// -------------------------------------------------------------- the studio surface
//
// Mirrors `dpaint_studio::api::Studio`, which cannot be reused directly: it opens a
// `Workspace` from a host directory and its crate carries the `std::net` bridge. The
// `catalog_matches_the_desktop_studio` test pins the two registries together.

impl DpaintEngine {
    fn dispatch_json(&self, method: &str, params: &Value) -> Result<Value> {
        match method {
            "state" => self.state(),
            "catalog" => Ok(self.registry.catalog()),
            "schema" => {
                let id = str_param(params, "op")?;
                let op = self.registry.get(&id)?;
                Ok(json!({ "id": op.id(), "about": op.about(), "schema": op.schema() }))
            }
            "op" => self.op(params),
            "undo" => Ok(json!({ "op": self.engine()?.undo()? })),
            "redo" => Ok(json!({ "op": self.engine()?.redo()? })),
            "digest" => self.digest(params),
            "lint" => self.lint(params),
            "history" => self.history(params),
            "select" => self.select(params),
            other => Err(Error::Invalid(format!("unknown studio method '{other}'"))),
        }
    }

    fn state(&self) -> Result<Value> {
        let mut ws = self.workspace()?;
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
            "project": { "name": p.name, "root": "browser (in-memory)",
                         "active": p.active.as_str(), "modified": p.modified },
            "documents": documents,
            "palette": p.palette.iter().map(|(k, v)| (k.clone(), v.to_hex()))
                        .collect::<std::collections::BTreeMap<_, _>>(),
            "canUndo": entries.iter().any(|e| !e.undone),
            "canRedo": entries.iter().any(|e| e.undone),
            "revision": seq,
        }))
    }

    fn op(&self, params: &Value) -> Result<Value> {
        let id = str_param(params, "op")?;
        let args = params.get("args").cloned().unwrap_or(json!({}));
        let doc = params.get("doc").and_then(|d| d.as_str()).map(String::from);
        let dry = params
            .get("dryRun")
            .and_then(|d| d.as_bool())
            .unwrap_or(false);
        let applied = self.engine()?.apply(&id, args, doc, dry)?;
        Ok(serde_json::to_value(applied)?)
    }

    fn digest(&self, params: &Value) -> Result<Value> {
        let ws = self.workspace()?;
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
        let d = dpaint_inspect::digest::digest(&ws.project, &doc, &self.assets(), &opts)?;
        Ok(serde_json::to_value(d)?)
    }

    fn lint(&self, params: &Value) -> Result<Value> {
        let ws = self.workspace()?;
        let assets = self.assets();
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

    fn history(&self, params: &Value) -> Result<Value> {
        let mut ws = self.workspace()?;
        let limit = params.get("limit").and_then(|l| l.as_u64()).unwrap_or(50) as usize;
        let rows: Vec<Value> = ws
            .journal
            .load()?
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

    fn render_bytes(
        &self,
        doc: Option<&str>,
        scale: f64,
        max_side: u32,
    ) -> Result<(Vec<u8>, [u32; 2])> {
        let ws = self.workspace()?;
        let id = ws.project.resolve_doc(doc)?;
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
        let pm = dpaint_render::render_document(&ws.project, &id, &self.assets(), &opts)?;
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

/// The engine is usable from native test code too — `dispatch` is the wasm wrapper around
/// `dispatch_json`, and everything below it is ordinary Rust.
impl DpaintEngine {
    #[doc(hidden)]
    pub fn call(&self, method: &str, params: Value) -> Result<Value> {
        self.dispatch_json(method, &params)
    }

    #[doc(hidden)]
    pub fn root_path(&self) -> &Path {
        &self.root
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engine() -> DpaintEngine {
        let e = DpaintEngine::blank();
        let doc = first_document("main", "raster", 64.0, 64.0).unwrap();
        Workspace::create_with_vfs(&e.root, Project::new("browser", doc), Arc::clone(&e.vfs))
            .unwrap();
        e
    }

    /// The browser catalog must be the desktop catalog. If a new mode crate is registered in
    /// `dpaint-studio` and not here, the UI silently loses ops in the web build.
    #[test]
    fn the_catalog_is_exactly_the_desktop_studios_minus_nothing() {
        let ids = |c: &Value| -> Vec<String> {
            c.as_array()
                .unwrap()
                .iter()
                .map(|o| o["id"].as_str().unwrap().to_string())
                .collect()
        };
        assert_eq!(
            ids(&registry().catalog()),
            ids(&dpaint_studio::api::registry().catalog())
        );
    }

    #[test]
    fn a_document_survives_an_op_an_undo_and_a_redo_in_memory() {
        let e = engine();
        let before = e.call("state", json!({})).unwrap();
        assert_eq!(before["canUndo"], json!(false));
        assert_eq!(before["documents"][0]["size"], json!([64.0, 64.0]));

        e.call(
            "op",
            json!({ "op": "raster.layer.add", "args": { "type": "fill", "color": "#fb8500", "name": "bg" } }),
        )
        .unwrap();
        let after = e.call("state", json!({})).unwrap();
        assert_eq!(after["canUndo"], json!(true));
        assert_eq!(after["revision"], json!(1));
        assert_eq!(after["documents"][0]["objects"][0]["name"], json!("bg"));

        assert_eq!(
            e.call("undo", json!({})).unwrap()["op"],
            json!("raster.layer.add")
        );
        let undone = e.call("state", json!({})).unwrap();
        assert_eq!(
            undone["documents"][0]["objects"].as_array().unwrap().len(),
            0
        );
        assert_eq!(undone["canRedo"], json!(true));

        assert_eq!(
            e.call("redo", json!({})).unwrap()["op"],
            json!("raster.layer.add")
        );
        assert_eq!(
            e.call("state", json!({})).unwrap()["documents"][0]["objects"][0]["name"],
            json!("bg")
        );
    }

    #[test]
    fn a_failing_op_reports_the_same_structured_error_the_http_bridge_does() {
        let e = engine();
        let err = e
            .call("op", json!({ "op": "raster.layer.nope" }))
            .unwrap_err();
        assert_eq!(err.code(), "unknown_op");

        let miss = e
            .call("select", json!({ "selector": "#nothing-here" }))
            .unwrap_err();
        let detail = serde_json::to_value(miss.detail()).unwrap();
        assert_eq!(detail["code"], json!("selector_no_match"));
        assert!(detail.get("message").is_some());

        assert_eq!(e.call("bogus", json!({})).unwrap_err().code(), "invalid");
    }

    #[test]
    fn rendering_produces_a_png_and_the_render_reflects_the_edit() {
        let e = engine();
        let (blank, size) = e.render_bytes(None, 1.0, 1600).unwrap();
        assert_eq!(size, [64, 64]);
        assert_eq!(&blank[1..4], b"PNG");

        e.call(
            "op",
            json!({ "op": "raster.layer.add", "args": { "type": "fill", "color": "#fb8500" } }),
        )
        .unwrap();
        let (filled, _) = e.render_bytes(None, 1.0, 1600).unwrap();
        assert_ne!(
            blank, filled,
            "the viewport must change when the document does"
        );
    }

    #[test]
    fn a_project_round_trips_through_the_text_and_blobs_the_page_persists() {
        let e = engine();
        let asset = e.assets().put(b"\x89PNG not really", "png").unwrap();
        e.call(
            "op",
            json!({ "op": "palette.set", "args": { "name": "brand", "color": "#fb8500" } }),
        )
        .unwrap();

        let json_text =
            String::from_utf8(e.vfs.read(&e.root.join("project.json")).unwrap()).unwrap();
        let history =
            String::from_utf8(e.vfs.read(&e.root.join("history.jsonl")).unwrap()).unwrap();
        let blob = e.assets().get(&asset).unwrap();

        // What a reload does: adopt the JSON, replay the blobs, restore the journal.
        let back = DpaintEngine::blank();
        back.vfs.create_dir_all(&back.root.join("assets")).unwrap();
        back.vfs
            .write(&back.root.join("project.json"), json_text.as_bytes())
            .unwrap();
        back.vfs
            .write(&back.root.join("history.jsonl"), history.as_bytes())
            .unwrap();
        assert_eq!(
            back.assets().put(&blob, "png").unwrap(),
            asset,
            "the ref is the content"
        );

        assert_eq!(
            back.call("state", json!({})).unwrap(),
            e.call("state", json!({})).unwrap()
        );
        assert_eq!(
            back.call("history", json!({})).unwrap(),
            e.call("history", json!({})).unwrap()
        );
        assert_eq!(
            back.call("undo", json!({})).unwrap()["op"],
            json!("palette.set")
        );
    }

    #[test]
    fn every_document_kind_can_start_a_project() {
        for (kind, expect) in [
            ("raster", "raster"),
            ("vector", "vector"),
            ("model", "model"),
        ] {
            let d = first_document("start", kind, 32.0, 32.0).unwrap();
            assert_eq!(d.kind().as_str(), expect);
        }
        assert_eq!(
            first_document("x", "raster", 0.0, 10.0).unwrap_err().code(),
            "invalid"
        );
        assert_eq!(
            first_document("x", "sculpt", 10.0, 10.0)
                .unwrap_err()
                .code(),
            "invalid"
        );
    }
}
