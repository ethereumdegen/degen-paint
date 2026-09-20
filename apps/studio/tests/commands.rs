//! The desktop shell's command layer, exercised without a webview.
//!
//! `dpaint_call` and `dpaint_render` are two-line `#[tauri::command]` wrappers over
//! [`Shell::call`] and [`Shell::render`]; these call the wrapped functions directly against a
//! temp project, so what is under test here is exactly what the webview reaches.

use dpaint_core::doc::{Document, RasterDoc};
use dpaint_core::{DocId, Engine, Project, Workspace};
use dpaint_studio::Studio;
use dpaint_studio_app::{bridge, menu_method, project_arg, Shell, MENU_OPEN, MENU_REDO, MENU_UNDO};
use serde_json::{json, Value};

fn shell() -> (tempfile::TempDir, Shell) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("demo.dpaint");
    let project = Project::new(
        "demo",
        Document::Raster(RasterDoc::new(DocId::from("doc_main"), "main", 120, 90)),
    );
    Workspace::create(&root, project).unwrap();
    let studio = Studio::open(&root).unwrap();
    (tmp, Shell::new(Some(studio), ""))
}

/// The structured error the bridge hands the webview, parsed back the way the init script does.
fn detail(payload: &str) -> Value {
    serde_json::from_str(payload)
        .unwrap_or_else(|e| panic!("bridge error payload must be JSON ({e}): {payload}"))
}

#[test]
fn state_round_trips_through_the_command_layer() {
    let (_t, sh) = shell();
    let st = sh.call("state", &json!({})).unwrap();

    assert_eq!(st["project"]["name"], "demo");
    assert_eq!(st["documents"][0]["id"], "doc_main");
    assert_eq!(st["documents"][0]["size"], json!([120.0, 90.0]));
    assert_eq!(st["canUndo"], false);
    assert_eq!(st["revision"], 0);
}

#[test]
fn a_desktop_op_is_journaled_as_human_and_undo_reverts_it() {
    let (_t, sh) = shell();
    let applied = sh
        .call(
            "op",
            &json!({ "op": "raster.layer.add",
                     "args": { "type": "fill", "color": "#14213d", "name": "bg" } }),
        )
        .unwrap();
    assert_eq!(applied["created"], json!(["lyr_bg"]));

    // The whole point of the shared journal: the desktop shell's writes are attributable.
    let h = sh.call("history", &json!({})).unwrap();
    assert_eq!(h["entries"][0]["op"], "raster.layer.add");
    assert_eq!(h["entries"][0]["actor"], "human");

    let st = sh.call("state", &json!({})).unwrap();
    assert_eq!(st["documents"][0]["objects"][0]["name"], "bg");
    assert_eq!(st["revision"], 1);

    assert_eq!(
        sh.call("undo", &json!({})).unwrap()["op"],
        "raster.layer.add"
    );

    let st = sh.call("state", &json!({})).unwrap();
    assert_eq!(st["documents"][0]["objects"].as_array().unwrap().len(), 0);
    assert_eq!(st["canRedo"], true);
}

#[test]
fn an_agents_write_shows_up_and_the_desktop_can_undo_it() {
    let (_t, sh) = shell();
    let root = sh.root().expect("project open");
    let mut agent = Engine::new(dpaint_studio::registry(), Workspace::open(&root).unwrap());
    agent
        .apply(
            "raster.layer.add",
            json!({ "type": "fill", "color": "#e76f51", "name": "agent-bg" }),
            None,
            false,
        )
        .unwrap();

    let h = sh.call("history", &json!({})).unwrap();
    assert_eq!(h["entries"][0]["actor"], "agent");
    assert_eq!(
        sh.call("undo", &json!({})).unwrap()["op"],
        "raster.layer.add"
    );
    assert_eq!(
        sh.call("state", &json!({})).unwrap()["documents"][0]["objects"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
}

#[test]
fn a_bad_selector_crosses_the_bridge_as_the_http_error_shape() {
    let (_t, sh) = shell();
    sh.call(
        "op",
        &json!({ "op": "raster.layer.add",
                 "args": { "type": "fill", "color": "#14213d", "name": "sky-grad" } }),
    )
    .unwrap();

    let payload = sh
        .call("select", &json!({ "selector": "#sky" }))
        .unwrap_err();
    let d = detail(&payload);
    assert_eq!(d["code"], "selector_no_match");
    assert!(
        d["message"].as_str().unwrap().contains("#sky"),
        "message must name the selector: {d}"
    );
    assert_eq!(
        d["suggestion"], "#lyr_sky-grad",
        "an error that does not carry the fix costs a round trip: {d}"
    );
    assert!(d["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c == "#lyr_sky-grad"));
}

#[test]
fn the_native_edit_menu_drives_the_shared_journal() {
    // The OS click cannot be simulated headlessly, but everything downstream of it can: the
    // menu handler looks the id up with `menu_method` and hands the result to `Shell::call`.
    assert_eq!(menu_method(MENU_UNDO), Some("undo"));
    assert_eq!(menu_method(MENU_REDO), Some("redo"));
    assert_eq!(
        menu_method(MENU_OPEN),
        None,
        "Open Project is not a journal method"
    );
    assert_eq!(menu_method("Edit"), None);

    let (_t, sh) = shell();
    sh.call(
        "op",
        &json!({ "op": "raster.layer.add",
                 "args": { "type": "fill", "color": "#264653", "name": "bg" } }),
    )
    .unwrap();

    sh.call(menu_method(MENU_UNDO).unwrap(), &json!({}))
        .unwrap();
    assert_eq!(sh.call("state", &json!({})).unwrap()["canUndo"], false);

    sh.call(menu_method(MENU_REDO).unwrap(), &json!({}))
        .unwrap();
    assert_eq!(
        sh.call("state", &json!({})).unwrap()["documents"][0]["objects"][0]["name"],
        "bg"
    );
}

#[test]
fn render_hands_the_webview_a_png_data_uri() {
    let (_t, sh) = shell();
    sh.call(
        "op",
        &json!({ "op": "raster.layer.add",
                 "args": { "type": "fill", "color": "#f4a261", "name": "bg" } }),
    )
    .unwrap();

    let uri = sh.render(None, 1.0, 1600).unwrap();
    let b64 = uri
        .strip_prefix("data:image/png;base64,")
        .unwrap_or_else(|| panic!("not a png data uri: {}", &uri[..uri.len().min(40)]));

    use base64::Engine as _;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(b64)
        .expect("valid base64");
    assert_eq!(
        &bytes[..8],
        b"\x89PNG\r\n\x1a\n",
        "decoded payload must be a PNG"
    );
}

#[test]
fn render_rejects_a_missing_document_with_a_structured_error() {
    let (_t, sh) = shell();
    let d = detail(&sh.render(Some("doc_nope"), 1.0, 1600).unwrap_err());
    assert_eq!(d["code"], "no_such_document");
}

#[test]
fn with_no_project_open_both_commands_answer_like_the_engine_does() {
    let sh = Shell::new(None, "nothing here");
    for payload in [
        sh.call("state", &json!({})).unwrap_err(),
        sh.render(None, 1.0, 1600).unwrap_err(),
    ] {
        let d = detail(&payload);
        assert_eq!(d["code"], "invalid");
        assert!(d["message"].as_str().unwrap().contains("Open Project"));
    }
    assert!(sh.root().is_none());
    assert_eq!(sh.notice(), "nothing here");
}

#[test]
fn the_project_flag_is_read_off_argv_in_both_spellings() {
    let a = |v: &[&str]| project_arg(v.iter().map(|s| s.to_string()));
    assert_eq!(
        a(&["--project", "/tmp/x.dpaint"]).unwrap().to_str(),
        Some("/tmp/x.dpaint")
    );
    assert_eq!(
        a(&["--project=/tmp/y.dpaint"]).unwrap().to_str(),
        Some("/tmp/y.dpaint")
    );
    // macOS hands a bundle its own flags; they must not shadow the project.
    assert_eq!(
        a(&[
            "-NSDocumentRevisionsDebugMode",
            "YES",
            "--project",
            "/tmp/z.dpaint"
        ])
        .unwrap()
        .to_str(),
        Some("/tmp/z.dpaint")
    );
    assert!(a(&[]).is_none());
    assert!(a(&["--project"]).is_none());
}

#[test]
fn the_injected_bridge_ships_inside_the_binary() {
    // The UI files know nothing about Tauri; this script is the only thing that makes
    // `invoke` look like `fetch` to them, so it has to be in the shipped executable.
    let exe = std::fs::read(env!("CARGO_BIN_EXE_degen-paint")).unwrap();
    let needle = bridge::INIT_SCRIPT.as_bytes();
    assert!(
        exe.windows(needle.len()).any(|w| w == needle),
        "the init script the window builder injects is not present in the built app"
    );

    for global in ["window.__DPAINT_INVOKE__", "window.__DPAINT_RENDER_URL__"] {
        assert!(bridge::INIT_SCRIPT.contains(global), "missing {global}");
    }
    assert!(bridge::INIT_SCRIPT.contains("dpaint_call"));
    assert!(bridge::INIT_SCRIPT.contains("dpaint_render"));
}
