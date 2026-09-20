//! The browser build's guarantee, tested natively: an entire project life cycle — create,
//! ten ops, undo to the start, redo to the end — with no filesystem underneath at all.
//!
//! Byte-identical at every step is the same gate P0 held the native engine to. If the
//! in-memory backend were subtly different (dropped bytes, lost append, stale read) the
//! undo walk would diverge on the very first state.

use dpaint_core::doc::{Document, RasterDoc};
use dpaint_core::vfs::{MemVfs, Vfs};
use dpaint_core::{DocId, Engine, Project, Registry, Workspace};
use serde_json::json;
use std::path::Path;
use std::sync::Arc;

const ROOT: &str = "/browser/session.dpaint";

fn registry() -> Registry {
    let mut r = Registry::new();
    r.extend(dpaint_core::ops::ops());
    r
}

fn new_project() -> Project {
    Project::new(
        "session",
        Document::Raster(RasterDoc::new(DocId::from("doc_main"), "main", 64, 64)),
    )
}

/// The ten edits the walk replays. Deliberately a mix: documents added and renamed and
/// resized, the active document moved, palette entries set and removed — so the patches
/// touch every corner of `project.json` rather than one scalar.
fn journey() -> Vec<(&'static str, serde_json::Value)> {
    vec![
        ("doc.add", json!({ "name": "logo", "kind": "vector", "width": 128, "height": 128 })),
        ("doc.add", json!({ "name": "badge", "kind": "model" })),
        ("palette.set", json!({ "name": "brand", "color": "#fb8500" })),
        ("palette.set", json!({ "name": "ink", "color": "#1d1d1f" })),
        ("doc.rename", json!({ "document": "logo", "name": "mark" })),
        ("doc.set-active", json!({ "document": "mark" })),
        ("doc.resize", json!({ "document": "main", "width": 96, "height": 48 })),
        ("doc.duplicate", json!({ "document": "badge" })),
        ("palette.remove", json!({ "name": "ink" })),
        ("doc.set-active", json!({ "document": "main" })),
    ]
}

#[test]
fn a_whole_project_life_cycle_runs_byte_identically_with_no_filesystem() {
    let vfs = MemVfs::new();
    let shared: Arc<dyn Vfs> = Arc::new(vfs.clone());
    let ws = Workspace::create_with_vfs(ROOT, new_project(), Arc::clone(&shared)).unwrap();
    let mut engine = Engine::new(registry(), ws);

    let mut states = vec![serde_json::to_value(&engine.workspace.project).unwrap()];
    for (op, args) in journey() {
        engine.apply(op, args.clone(), None, false).unwrap_or_else(|e| panic!("{op}: {e}"));
        states.push(serde_json::to_value(&engine.workspace.project).unwrap());
    }
    assert_eq!(states.len(), 11);
    assert_eq!(engine.workspace.project.documents.len(), 4);

    // Every state must be distinct, or "byte-identical at every step" proves nothing.
    for (i, s) in states.iter().enumerate() {
        for (j, t) in states.iter().enumerate().skip(i + 1) {
            assert_ne!(s, t, "states {i} and {j} are the same; the walk is not exercising anything");
        }
    }

    for (i, expect) in states.iter().enumerate().rev().skip(1) {
        let undone = engine.undo().unwrap();
        assert!(undone.is_some(), "undo {i} returned nothing");
        assert_eq!(
            &serde_json::to_value(&engine.workspace.project).unwrap(),
            expect,
            "state after undoing back to step {i} diverged"
        );
    }
    assert!(engine.undo().unwrap().is_none(), "undo past the start must be a no-op");

    for (i, expect) in states.iter().enumerate().skip(1) {
        assert!(engine.redo().unwrap().is_some(), "redo to {i} returned nothing");
        assert_eq!(
            &serde_json::to_value(&engine.workspace.project).unwrap(),
            expect,
            "state after redoing to step {i} diverged"
        );
    }
    assert!(engine.redo().unwrap().is_none());

    // Reopening from the same tree must reconstruct the final project exactly, which is
    // what the browser does after a reload out of OPFS.
    let reopened = Workspace::open_with_vfs(ROOT, Arc::clone(&shared)).unwrap();
    assert_eq!(reopened.project, engine.workspace.project);

    // And nothing escaped to the host.
    let files: Vec<String> =
        vfs.snapshot().into_iter().map(|(p, _)| p.display().to_string()).collect();
    assert_eq!(
        files,
        vec!["/browser/session.dpaint/history.jsonl", "/browser/session.dpaint/project.json"]
    );
    assert!(!Path::new(ROOT).exists(), "the test must not have created a real directory");
}

#[test]
fn the_journal_replays_from_a_persisted_memory_tree() {
    let vfs = MemVfs::new();
    let shared: Arc<dyn Vfs> = Arc::new(vfs.clone());
    {
        let ws = Workspace::create_with_vfs(ROOT, new_project(), Arc::clone(&shared)).unwrap();
        let mut engine = Engine::new(registry(), ws);
        for (op, args) in journey().into_iter().take(4) {
            engine.apply(op, args, None, false).unwrap();
        }
        engine.undo().unwrap();
    }

    // A fresh engine over the same bytes — a page reload — sees the same undo state.
    let mut engine = Engine::new(registry(), Workspace::open_with_vfs(ROOT, shared).unwrap());
    let entries = engine.workspace.journal.load().unwrap().to_vec();
    assert_eq!(entries.len(), 4, "every applied op is on the journal");
    assert!(entries[3].undone, "the undone flag survived the round trip");
    assert_eq!(engine.redo().unwrap().as_deref(), Some("palette.set"));
    assert_eq!(engine.workspace.project.palette.len(), 2);
}

#[test]
fn assets_written_in_memory_are_deduplicated_and_collected() {
    let vfs = MemVfs::new();
    let ws = Workspace::create_with_vfs(ROOT, new_project(), Arc::new(vfs.clone())).unwrap();

    let a = ws.assets.put(b"\x89PNG fake one", "png").unwrap();
    let same = ws.assets.put(b"\x89PNG fake one", "png").unwrap();
    let b = ws.assets.put(b"\x89PNG fake two", "png").unwrap();
    assert_eq!(a, same, "content addressing deduplicates in memory too");
    assert_eq!(ws.assets.list().unwrap().len(), 2);
    assert_eq!(vfs.snapshot().len(), 3, "project.json and exactly two blobs; no op ran");

    // Nothing references either blob, so gc reclaims both and the tree shrinks.
    let keep = ws.project.referenced_assets();
    assert!(keep.is_empty());
    let (removed, freed) = ws.assets.gc(&keep).unwrap();
    assert_eq!(removed.len(), 2);
    assert_eq!(freed, 26);
    assert!(ws.assets.list().unwrap().is_empty());
    assert_eq!(ws.assets.get(&b).unwrap_err().code(), "asset_missing");
    assert_eq!(vfs.bytes(), vfs.snapshot().iter().map(|(_, v)| v.len() as u64).sum::<u64>());
}
