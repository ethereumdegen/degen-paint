//! The native menu bar.
//!
//! Labels and accelerators come from [`dpaint_studio::contract::SHORTCUTS`], which is also what
//! the `?` overlay and `dpaint skill` read: a menu bar that names a different key from the
//! overlay is worse than no menu bar. An id the contract does not carry gets no item, so the
//! menu cannot offer something nothing handles.
//!
//! Every item does one thing — fire `dpaint:menu` at the page with its contract id — so the
//! dialog that opens is the UI's own, the same one the browser build opens from its
//! `role=menubar`. macOS exposes this bar to `AxObserver`, which is what gives Starkbot
//! `select_menu` without any knowledge of the Studio's layout.

use crate::Shell;
use dpaint_studio::contract::{Shortcut, SHORTCUTS};
use serde_json::{json, Value};
use tauri::menu::{MenuBuilder, MenuItem, Submenu, SubmenuBuilder};
use tauri::{AppHandle, Emitter, Manager, Wry};

/// Recent projects are the shell's own business: the page has no vocabulary for "that one".
const RECENT_PREFIX: &str = "dpaint:recent:";
/// Longest File > Open Recent list. Past ten the menu is a filing cabinet, not a shortcut.
const RECENT_MAX: usize = 10;

/// A separator, where an id would otherwise be.
const SEP: &str = "";
/// Positions for the two nested submenus, which are built from live data rather than one id.
const RECENT: &str = "@recent";
const PANES: &str = "@panes";

const FILE: &[&str] = &[
    "project.new",
    "project.open",
    RECENT,
    SEP,
    "io.import",
    "io.export",
    "io.exportPreview",
    "io.sendToEditor",
    SEP,
    "project.close",
];
const EDIT: &[&str] = &[
    "edit.undo",
    "edit.redo",
    SEP,
    "object.delete",
    "edit.selectAll",
];
const VIEW: &[&str] = &["view.fit", "view.oneToOne", SEP, PANES];
const HELP: &[&str] = &["help.doctor", "help.shortcuts"];

/// Build the bar and hand it to the app.
///
/// Called again whenever a project opens or closes, because File > Open Recent is a snapshot of
/// something that just changed.
pub fn install(app: &AppHandle) -> tauri::Result<()> {
    let file = section(app, "File", FILE)?;
    let edit = section(app, "Edit", EDIT)?;
    let view = section(app, "View", VIEW)?;
    let help = section(app, "Help", HELP)?;

    let bar = MenuBuilder::new(app);
    // The application menu is macOS's; its items (Services, Hide Others, Quit) are documented
    // unsupported elsewhere, and an empty submenu is worse than no submenu.
    #[cfg(target_os = "macos")]
    let bar = {
        let app_menu = SubmenuBuilder::new(app, "degen-paint")
            .about(Some(tauri::menu::AboutMetadata::default()))
            .separator()
            .services()
            .separator()
            .hide()
            .hide_others()
            .separator()
            .quit()
            .build()?;
        bar.item(&app_menu)
    };
    let menu = bar.items(&[&file, &edit, &view, &help]).build()?;
    app.set_menu(menu)?;
    Ok(())
}

/// Route a menu event.
pub fn on_event(app: &AppHandle, id: &str) {
    match id.strip_prefix(RECENT_PREFIX) {
        Some(path) => open_recent(app, path),
        None => fire(app, id),
    }
}

/// The one mechanism the UI contract names: a `dpaint:menu` `CustomEvent` on the page's window.
///
/// `eval` rather than Tauri's `emit`, because the listener has to be the same one the browser
/// build uses, and a browser has no Tauri event API.
fn fire(app: &AppHandle, id: &str) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    let detail = json!({ "id": id });
    let _ = window.eval(format!(
        "window.dispatchEvent(new CustomEvent('dpaint:menu',{{detail:{detail}}}))"
    ));
}

/// Open a project the menu named, then rebuild the menu around the new recent list.
fn open_recent(app: &AppHandle, path: &str) {
    let shell = app.state::<Shell>();
    match shell.call("project.open", &json!({ "path": path })) {
        Ok(_) => {
            crate::retitle(app);
            let _ = install(app);
            let _ = app.emit("dpaint:changed", json!({ "reason": "project.open" }));
        }
        Err(payload) => {
            let detail: Value = serde_json::from_str(&payload).unwrap_or(Value::String(payload));
            let _ = app.emit(
                "dpaint:error",
                json!({ "reason": "project.open", "error": detail }),
            );
        }
    }
}

fn section(app: &AppHandle, title: &str, ids: &[&str]) -> tauri::Result<Submenu<Wry>> {
    let mut builder = SubmenuBuilder::new(app, title);
    for id in ids {
        match *id {
            SEP => builder = builder.separator(),
            RECENT => builder = builder.item(&recent(app)?),
            PANES => match panes(app)? {
                Some(sub) => builder = builder.item(&sub),
                None => continue,
            },
            id => {
                if let Some(s) = shortcut(id) {
                    builder = builder.item(&item(app, s)?);
                }
            }
        }
    }
    builder.build()
}

fn shortcut(id: &str) -> Option<&'static Shortcut> {
    SHORTCUTS.iter().find(|s| s.id == id)
}

/// One item, named and keyed by the contract.
///
/// Tauri drops an accelerator it cannot parse rather than failing, so a key the menu bar has no
/// spelling for (`?`) costs that item its shortcut and nothing else. The `?` overlay is bound in
/// the page anyway.
fn item(app: &AppHandle, s: &Shortcut) -> tauri::Result<MenuItem<Wry>> {
    let keys = (!s.keys.is_empty()).then_some(s.keys);
    MenuItem::with_id(app, s.id, s.label, true, keys)
}

/// File > Open Recent, from the project's own recent list.
fn recent(app: &AppHandle) -> tauri::Result<Submenu<Wry>> {
    let entries = app
        .state::<Shell>()
        .call("project.recent", &json!({}))
        .ok()
        .and_then(|v| v.get("entries").and_then(Value::as_array).cloned())
        .unwrap_or_default();

    let mut builder = SubmenuBuilder::new(app, "Open Recent");
    if entries.is_empty() {
        let empty = MenuItem::with_id(
            app,
            "dpaint:recent:",
            "No recent projects",
            false,
            None::<&str>,
        )?;
        return builder.item(&empty).build();
    }
    for e in entries.iter().take(RECENT_MAX) {
        let Some(path) = e.get("path").and_then(Value::as_str) else {
            continue;
        };
        let label = e.get("name").and_then(Value::as_str).unwrap_or(path);
        builder = builder.item(&MenuItem::with_id(
            app,
            format!("{RECENT_PREFIX}{path}"),
            label,
            true,
            None::<&str>,
        )?);
    }
    builder.build()
}

/// View > Panes, from whichever pane shortcuts the contract carries.
fn panes(app: &AppHandle) -> tauri::Result<Option<Submenu<Wry>>> {
    let pane_shortcuts: Vec<&Shortcut> = SHORTCUTS
        .iter()
        .filter(|s| s.id.starts_with("view.pane."))
        .collect();
    if pane_shortcuts.is_empty() {
        return Ok(None);
    }
    let mut builder = SubmenuBuilder::new(app, "Panes");
    for s in pane_shortcuts {
        builder = builder.item(&item(app, s)?);
    }
    Ok(Some(builder.build()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The menu is the only place these ids are written down on the Rust side; a typo makes an
    /// item vanish silently, because `section` skips an id the contract does not carry.
    #[test]
    fn every_menu_id_exists_in_the_contract() {
        for ids in [FILE, EDIT, VIEW, HELP] {
            for id in ids {
                if matches!(*id, SEP | RECENT | PANES) {
                    continue;
                }
                assert!(
                    shortcut(id).is_some(),
                    "menu id '{id}' is not in dpaint_studio::contract::SHORTCUTS"
                );
            }
        }
    }
}
