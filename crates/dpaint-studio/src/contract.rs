//! What the Studio promises: one action table, one list of consequential labels.
//!
//! The web UI's `?` overlay, the Tauri menu bar, `dpaint skill` and the accessibility audit
//! all read this table instead of keeping their own copy. A shortcut that exists in the app
//! but not here is invisible to the agent driving it, so this is the definition, not a
//! summary of one.

use crate::api::Studio;
use dpaint_core::Result;
use serde_json::{json, Value};

/// One keyboard-reachable action. `keys` is in Tauri accelerator spelling so the native menu
/// can use it verbatim; `scope` is `global`, `document` or `dialog`.
pub struct Shortcut {
    pub id: &'static str,
    pub label: &'static str,
    pub keys: &'static str,
    pub scope: &'static str,
}

/// Every action the UI offers by keyboard. The first thirteen ids are also the native menu's
/// items and the ids the shell fires as `dpaint:menu` events.
pub const SHORTCUTS: &[Shortcut] = &[
    Shortcut {
        id: "project.new",
        label: "New Project…",
        keys: "CmdOrCtrl+N",
        scope: "global",
    },
    Shortcut {
        id: "project.open",
        label: "Open Project…",
        keys: "CmdOrCtrl+O",
        scope: "global",
    },
    Shortcut {
        id: "project.close",
        label: "Close Project",
        keys: "CmdOrCtrl+Shift+W",
        scope: "global",
    },
    Shortcut {
        id: "io.import",
        label: "Import…",
        keys: "CmdOrCtrl+I",
        scope: "document",
    },
    Shortcut {
        id: "io.export",
        label: "Export…",
        keys: "CmdOrCtrl+E",
        scope: "document",
    },
    Shortcut {
        id: "io.exportPreview",
        label: "Export Preview",
        keys: "CmdOrCtrl+Shift+P",
        scope: "document",
    },
    Shortcut {
        id: "io.sendToEditor",
        label: "Send to Editor…",
        keys: "CmdOrCtrl+Shift+E",
        scope: "document",
    },
    Shortcut {
        id: "edit.undo",
        label: "Undo",
        keys: "CmdOrCtrl+Z",
        scope: "document",
    },
    Shortcut {
        id: "edit.redo",
        label: "Redo",
        keys: "CmdOrCtrl+Shift+Z",
        scope: "document",
    },
    Shortcut {
        id: "view.fit",
        label: "Fit",
        keys: "CmdOrCtrl+0",
        scope: "document",
    },
    Shortcut {
        id: "view.oneToOne",
        label: "100 %",
        keys: "CmdOrCtrl+1",
        scope: "document",
    },
    Shortcut {
        id: "help.doctor",
        label: "Doctor",
        keys: "CmdOrCtrl+Shift+D",
        scope: "global",
    },
    Shortcut {
        id: "help.shortcuts",
        label: "Keyboard Shortcuts",
        keys: "?",
        scope: "global",
    },
    Shortcut {
        id: "palette.open",
        label: "Run op",
        keys: "CmdOrCtrl+K",
        scope: "global",
    },
    Shortcut {
        id: "palette.close",
        label: "Close the command palette",
        keys: "Escape",
        scope: "dialog",
    },
    Shortcut {
        id: "filter.focus",
        label: "Filter the active list",
        keys: "CmdOrCtrl+F",
        scope: "document",
    },
    Shortcut {
        id: "pane.focus",
        label: "Focus the next pane",
        keys: "F6",
        scope: "global",
    },
    Shortcut {
        id: "view.zoomIn",
        label: "Zoom in",
        keys: "CmdOrCtrl+=",
        scope: "document",
    },
    Shortcut {
        id: "view.zoomOut",
        label: "Zoom out",
        keys: "CmdOrCtrl+-",
        scope: "document",
    },
    Shortcut {
        id: "view.orbitLeft",
        label: "Orbit left",
        keys: "Alt+Left",
        scope: "document",
    },
    Shortcut {
        id: "view.orbitRight",
        label: "Orbit right",
        keys: "Alt+Right",
        scope: "document",
    },
    Shortcut {
        id: "view.orbitUp",
        label: "Orbit up",
        keys: "Alt+Up",
        scope: "document",
    },
    Shortcut {
        id: "view.orbitDown",
        label: "Orbit down",
        keys: "Alt+Down",
        scope: "document",
    },
    Shortcut {
        id: "edit.selectAll",
        label: "Select All",
        keys: "CmdOrCtrl+A",
        scope: "document",
    },
    Shortcut {
        id: "object.delete",
        label: "Delete",
        keys: "Delete",
        scope: "document",
    },
    Shortcut {
        id: "object.rename",
        label: "Rename",
        keys: "F2",
        scope: "document",
    },
    Shortcut {
        id: "object.duplicate",
        label: "Duplicate",
        keys: "CmdOrCtrl+D",
        scope: "document",
    },
    // View › Panes: each row both raises the dock tab and is the menu item for it.
    Shortcut {
        id: "view.pane.tree",
        label: "Layers",
        keys: "CmdOrCtrl+Alt+1",
        scope: "document",
    },
    Shortcut {
        id: "view.pane.inspector",
        label: "Inspector",
        keys: "CmdOrCtrl+Alt+2",
        scope: "document",
    },
    Shortcut {
        id: "view.pane.history",
        label: "History",
        keys: "CmdOrCtrl+Alt+3",
        scope: "document",
    },
    Shortcut {
        id: "view.pane.lint",
        label: "Lint",
        keys: "CmdOrCtrl+Alt+4",
        scope: "document",
    },
    Shortcut {
        id: "view.pane.console",
        label: "Console",
        keys: "CmdOrCtrl+Alt+5",
        scope: "document",
    },
];

/// Primary buttons whose labels start with one of these spend money, overwrite a file or
/// destroy work. Starkbot turns each into a confirm card, so a new consequential action must
/// be named with one of these prefixes — or added here.
pub const CONFIRM_LABEL_PREFIXES: &[&str] = &[
    "Overwrite ",
    "Delete document ",
    "Delete layer ",
    "Send ",
    "Run ai.",
];

/// The read-only probes of the grounding API: what a navigator may ask the app about a step
/// it has just taken.
pub(crate) const PROBES: &[(&str, &str, &str)] = &[
    (
        "status",
        "/api/v1/status",
        "project, revision, active document, running jobs",
    ),
    ("overview", "/api/v1/overview", "documents and their counts"),
    (
        "digest",
        "/api/v1/doc/:id/digest",
        "what a render actually contains",
    ),
    ("lint", "/api/v1/doc/:id/lint", "findings for one document"),
    (
        "history",
        "/api/v1/history?limit=",
        "recent journal entries",
    ),
    (
        "select",
        "/api/v1/select?q=&doc=",
        "ids a selector resolves to",
    ),
    ("job", "/api/v1/jobs/:id", "state of one job"),
    ("skill", "/api/v1/skill", "this contract, as data"),
    ("render", "/render.png?doc=&scale=", "the document as PNG"),
    (
        "annotate",
        "/annotate.png?doc=&scale=",
        "the same PNG with numbered bboxes and a legend",
    ),
];

/// The contract as data: the shortcut table, the labels that need a confirm card, the
/// grounding probes and the live project. `dpaint skill` and `GET /api/v1/skill` both serve
/// this, which is what stops the pack from drifting away from the app.
pub fn skill_json(studio: &Studio) -> Result<Value> {
    let shortcuts: Vec<Value> = SHORTCUTS
        .iter()
        .map(|s| json!({ "id": s.id, "label": s.label, "keys": s.keys, "scope": s.scope }))
        .collect();
    let probes: Vec<Value> = PROBES
        .iter()
        .map(|(id, path, about)| json!({ "id": id, "path": path, "about": about }))
        .collect();
    Ok(json!({
        "pack": "media-apps",
        "app": {
            "id": "dev.degenpaint.studio",
            "name": "degen-paint Studio",
            "axStrategy": "none",
            // A render settles in a frame; an export writes a file and reports its size.
            "settleMs": { "render": 300, "export": 1500 },
            "shortcuts": shortcuts,
            "confirmLabels": CONFIRM_LABEL_PREFIXES,
        },
        "grounding": {
            "baseUrlEnv": "DPAINT_BASE_URL",
            "probes": probes,
            "checks": [
                { "id": "lint-clean", "probe": "lint", "assert": "errors == 0" },
                { "id": "revision-advanced", "probe": "status", "assert": "revision > before" },
                { "id": "layer-count", "probe": "overview", "assert": "documents[i].layers == n" },
                { "id": "idle", "probe": "status", "assert": "busy == []" },
            ],
        },
        "project": studio.dispatch("state", &json!({}))?,
        // The vendorable half: `dpaint skill --out` writes exactly these files.
        "files": crate::skill::pack()["files"],
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shell fires these thirteen ids at the page; a missing one is a dead menu item.
    #[test]
    fn every_menu_action_has_a_shortcut_row() {
        for id in [
            "project.new",
            "project.open",
            "project.close",
            "io.import",
            "io.export",
            "io.exportPreview",
            "io.sendToEditor",
            "edit.undo",
            "edit.redo",
            "view.fit",
            "view.oneToOne",
            "help.doctor",
            "help.shortcuts",
        ] {
            assert!(
                SHORTCUTS.iter().any(|s| s.id == id),
                "the menu fires '{id}' and nothing here names it"
            );
        }
    }

    #[test]
    fn ids_and_accelerators_are_unique_per_scope() {
        for (i, a) in SHORTCUTS.iter().enumerate() {
            assert!(!a.label.is_empty() && !a.keys.is_empty(), "{}", a.id);
            assert!(
                matches!(a.scope, "global" | "document" | "dialog"),
                "{} has scope '{}'",
                a.id,
                a.scope
            );
            for b in SHORTCUTS.iter().skip(i + 1) {
                assert_ne!(a.id, b.id, "duplicate id");
                assert!(
                    a.keys != b.keys || a.scope != b.scope,
                    "{} and {} both answer to {} in scope {}",
                    a.id,
                    b.id,
                    a.keys,
                    a.scope
                );
            }
        }
    }
}
