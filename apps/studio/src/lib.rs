//! degen-paint Studio, desktop shell.
//!
//! A thin native wrapper around exactly the same static UI a browser gets from `dpaint serve`,
//! and exactly the same [`Studio`] API the HTTP bridge calls. Two commands, one managed
//! `Studio`, one init script that makes `invoke` look like `fetch` to the frontend. Nothing
//! about the document model lives here.

pub mod bridge;

use dpaint_core::Error;
use dpaint_studio::Studio;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use parking_lot::RwLock;
use tauri::menu::{AboutMetadata, MenuBuilder, MenuItemBuilder, SubmenuBuilder};
use tauri::{AppHandle, Emitter, Manager, State, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_dialog::DialogExt;

/// Scheme for the "no project open" window. Hyphen-free so every platform's URL parser agrees.
const WELCOME_SCHEME: &str = "dpaintwelcome";
const WELCOME_URL: &str = "dpaintwelcome://localhost/";

/// The managed state: one `Studio`, swappable because File > Open Project can change projects
/// without restarting. `RwLock` and not `Mutex` because `state` is polled roughly every second
/// per window and reads should not queue behind each other.
pub struct Shell {
    studio: RwLock<Option<Studio>>,
    /// Why there is no project, shown on the welcome window.
    notice: RwLock<String>,
}

impl Shell {
    pub fn new(studio: Option<Studio>, notice: impl Into<String>) -> Self {
        Self { studio: RwLock::new(studio), notice: RwLock::new(notice.into()) }
    }

    pub fn root(&self) -> Option<PathBuf> {
        self.studio.read().as_ref().map(|s| s.root().to_path_buf())
    }

    pub fn notice(&self) -> String {
        self.notice.read().clone()
    }

    pub fn set(&self, studio: Studio) {
        *self.studio.write() = Some(studio);
    }

    /// Dispatch, or the structured "no project" error if nothing is open yet.
    pub fn call(&self, method: &str, params: &Value) -> Result<Value, String> {
        match self.studio.read().as_ref() {
            Some(s) => bridge::call(s, method, params),
            None => Err(bridge::no_project_payload()),
        }
    }

    pub fn render(&self, doc: Option<&str>, scale: f64, max: u32) -> Result<String, String> {
        match self.studio.read().as_ref() {
            Some(s) => bridge::render_data_uri(s, doc, scale, max),
            None => Err(bridge::no_project_payload()),
        }
    }
}

// ---------------------------------------------------------------------------- commands

#[tauri::command]
async fn dpaint_call(
    shell: State<'_, Shell>,
    method: String,
    params: Value,
) -> Result<Value, String> {
    shell.call(&method, &params)
}

#[tauri::command]
async fn dpaint_render(
    shell: State<'_, Shell>,
    doc: Option<String>,
    scale: f64,
    max: u32,
) -> Result<String, String> {
    shell.render(doc.as_deref(), scale, max)
}

// ---------------------------------------------------------------------------- windows

fn window_title(root: &Path) -> String {
    let name = root.file_name().map(|n| n.to_string_lossy().into_owned());
    match name {
        Some(n) => format!("degen-paint — {n}"),
        None => "degen-paint".to_string(),
    }
}

/// The Studio window: the bundled UI plus the bridge, nothing else.
fn open_studio_window(app: &AppHandle) -> tauri::Result<()> {
    if let Some(w) = app.get_webview_window("main") {
        w.set_focus()?;
        return Ok(());
    }
    let title = app.state::<Shell>().root().map(|r| window_title(&r)).unwrap_or_else(|| "degen-paint".into());
    WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
        .title(title)
        .inner_size(1440.0, 900.0)
        .min_inner_size(920.0, 600.0)
        .initialization_script(bridge::INIT_SCRIPT)
        .build()?;
    if let Some(w) = app.get_webview_window("welcome") {
        w.close()?;
    }
    Ok(())
}

fn open_welcome_window(app: &AppHandle) -> tauri::Result<()> {
    if let Some(w) = app.get_webview_window("welcome") {
        w.set_focus()?;
        return Ok(());
    }
    WebviewWindowBuilder::new(
        app,
        "welcome",
        WebviewUrl::CustomProtocol(WELCOME_URL.parse().expect("static url")),
    )
    .title("degen-paint")
    .inner_size(720.0, 460.0)
    .resizable(false)
    .build()?;
    Ok(())
}

/// Point the shell at `path` and swap the welcome window for the Studio window.
pub fn open_project(app: &AppHandle, path: &Path) -> Result<(), String> {
    let studio = Studio::open(path).map_err(|e| e.to_string())?;
    let root = studio.root().to_path_buf();
    app.state::<Shell>().set(studio);
    open_studio_window(app).map_err(|e| e.to_string())?;
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.set_title(&window_title(&root));
    }
    let _ = app.emit("dpaint:changed", json!({ "reason": "project-opened", "root": root.display().to_string() }));
    Ok(())
}

/// Native directory picker. Used by both File > Open Project and the welcome window's button.
pub fn pick_project(app: &AppHandle) {
    let handle = app.clone();
    app.dialog()
        .file()
        .set_title("Open a degen-paint project")
        .pick_folder(move |picked| {
            let Some(p) = picked else { return };
            let path = match p.into_path() {
                Ok(p) => p,
                Err(e) => {
                    *handle.state::<Shell>().notice.write() = e.to_string();
                    let _ = open_welcome_window(&handle);
                    return;
                }
            };
            if let Err(e) = open_project(&handle, &path) {
                *handle.state::<Shell>().notice.write() = e;
                let _ = open_welcome_window(&handle);
                if let Some(w) = handle.get_webview_window("welcome") {
                    // Reload so the handler re-renders with the new notice.
                    let _ = w.eval("location.reload()");
                }
            }
        });
}

// ---------------------------------------------------------------------------- welcome page

/// Served on `dpaintwelcome://`. Kept out of `crates/dpaint-studio/ui/` on purpose: that
/// directory is the shared frontend, and this page is a shell concern.
pub fn welcome_html(notice: &str) -> String {
    let notice = notice
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    format!(
        r#"<!doctype html>
<html><head><meta charset="utf-8"><title>degen-paint</title><style>
  :root {{ color-scheme: dark; }}
  body {{ margin:0; height:100vh; display:grid; place-items:center; background:#14213d; color:#e9edf5;
         font:14px/1.6 -apple-system,BlinkMacSystemFont,"Segoe UI",sans-serif; }}
  main {{ max-width:34rem; padding:2.5rem; text-align:center; }}
  h1 {{ margin:0 0 .4rem; font-size:1.5rem; letter-spacing:.02em; }}
  p {{ margin:0 0 1.1rem; color:#a7b0c4; }}
  code {{ background:#0d1628; padding:.1rem .35rem; border-radius:.25rem; color:#f4a261; }}
  .notice {{ background:#0d1628; border-left:3px solid #e76f51; padding:.7rem .9rem; border-radius:.3rem;
            text-align:left; color:#f0c8bd; white-space:pre-wrap; margin-bottom:1.3rem; }}
  button {{ font:inherit; font-weight:600; padding:.65rem 1.3rem; border:0; border-radius:.4rem;
           background:#f4a261; color:#14213d; cursor:pointer; }}
  button:hover {{ background:#f7b77e; }}
  button[disabled] {{ opacity:.6; cursor:progress; }}
  .err {{ margin-top:1rem; color:#e76f51; min-height:1.6em; }}
</style></head><body><main>
  <h1>No project open</h1>
  <p>degen-paint Studio needs a <code>.dpaint</code> project directory.</p>
  <div class="notice">{notice}</div>
  <button id="pick">Choose project directory…</button>
  <div class="err" id="err"></div>
<script>
  const btn = document.getElementById('pick'), err = document.getElementById('err');
  btn.addEventListener('click', async () => {{
    btn.disabled = true; err.textContent = '';
    try {{
      const r = await fetch('{WELCOME_URL}pick');
      const j = await r.json();
      if (!j.ok) {{ err.textContent = j.error || 'could not open the picker'; }}
    }} catch (e) {{ err.textContent = String(e && e.message || e); }}
    btn.disabled = false;
  }});
</script>
</main></body></html>"#
    )
}

// ---------------------------------------------------------------------------- menu

pub const MENU_OPEN: &str = "dpaint:open";
pub const MENU_UNDO: &str = "dpaint:undo";
pub const MENU_REDO: &str = "dpaint:redo";

/// Menu item id to the [`Studio`] method it runs.
///
/// The native Undo/Redo items are *not* the webview's built-in ones: they have to reach the
/// shared journal, so that a human's Cmd-Z can undo an agent's op. Keeping the mapping here
/// means the menu and the event handler cannot disagree about which method that is.
pub fn menu_method(id: &str) -> Option<&'static str> {
    match id {
        MENU_UNDO => Some("undo"),
        MENU_REDO => Some("redo"),
        _ => None,
    }
}

fn build_menu(app: &AppHandle) -> tauri::Result<()> {
    let open = MenuItemBuilder::with_id(MENU_OPEN, "Open Project…")
        .accelerator("CmdOrCtrl+O")
        .build(app)?;
    let undo = MenuItemBuilder::with_id(MENU_UNDO, "Undo")
        .accelerator("CmdOrCtrl+Z")
        .build(app)?;
    let redo = MenuItemBuilder::with_id(MENU_REDO, "Redo")
        .accelerator("CmdOrCtrl+Shift+Z")
        .build(app)?;

    let app_menu = SubmenuBuilder::new(app, "degen-paint")
        .about(Some(AboutMetadata::default()))
        .separator()
        .services()
        .separator()
        .hide()
        .hide_others()
        .separator()
        .quit()
        .build()?;
    let file = SubmenuBuilder::new(app, "File")
        .item(&open)
        .separator()
        .close_window()
        .build()?;
    // See `menu_method`: these two go through the journal, not the webview's edit stack.
    let edit = SubmenuBuilder::new(app, "Edit")
        .item(&undo)
        .item(&redo)
        .separator()
        .cut()
        .copy()
        .paste()
        .select_all()
        .build()?;
    let view = SubmenuBuilder::new(app, "View").fullscreen().build()?;

    let menu = MenuBuilder::new(app)
        .items(&[&app_menu, &file, &edit, &view])
        .build()?;
    app.set_menu(menu)?;
    Ok(())
}

/// Run a journal method from the menu and tell every window the document moved.
fn dispatch_from_menu(app: &AppHandle, method: &str) {
    let result = app.state::<Shell>().call(method, &json!({}));
    match result {
        Ok(v) => {
            let _ = app.emit("dpaint:changed", json!({ "reason": method, "result": v }));
        }
        Err(payload) => {
            let detail: Value = serde_json::from_str(&payload).unwrap_or(Value::String(payload));
            let _ = app.emit("dpaint:error", json!({ "reason": method, "error": detail }));
        }
    }
}

// ---------------------------------------------------------------------------- startup

/// `--project <dir>` / `--project=<dir>` from argv. Unknown flags are ignored: macOS injects
/// its own when launching a bundle.
pub fn project_arg<I: IntoIterator<Item = String>>(args: I) -> Option<PathBuf> {
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        if a == "--project" {
            return it.next().map(PathBuf::from);
        }
        if let Some(v) = a.strip_prefix("--project=") {
            return Some(PathBuf::from(v));
        }
    }
    None
}

/// Resolve the project: explicit `--project`, else discovery from the cwd.
///
/// Returns the notice to show when nothing resolved, instead of panicking on a blank screen.
pub fn resolve_project(arg: Option<PathBuf>) -> Result<Studio, String> {
    match arg {
        Some(p) => Studio::open(&p).map_err(|e| format!("--project {}: {e}", p.display())),
        None => Studio::discover(".").map_err(|e| match e {
            Error::Invalid(_) | Error::Io(_) => format!(
                "No project found in the current directory or any parent.\n{e}"
            ),
            other => other.to_string(),
        }),
    }
}

pub fn run() {
    let (studio, notice) = match resolve_project(project_arg(std::env::args().skip(1))) {
        Ok(s) => (Some(s), String::new()),
        Err(msg) => (None, msg),
    };
    let have_project = studio.is_some();

    // This binary is normally launched from a terminal, so say what it decided to open.
    // A window that comes up on the wrong project is otherwise silent.
    match &studio {
        Some(s) => eprintln!("degen-paint: project {}", s.root().display()),
        None => eprintln!("degen-paint: {notice}\ndegen-paint: showing the project picker"),
    }

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(Shell::new(studio, notice))
        .invoke_handler(tauri::generate_handler![dpaint_call, dpaint_render])
        // A blank webview is the hardest desktop bug to diagnose from a terminal; say when a
        // window actually finished loading, and what it loaded.
        .on_page_load(|webview, payload| {
            if matches!(payload.event(), tauri::webview::PageLoadEvent::Finished) {
                eprintln!("degen-paint: window '{}' loaded {}", webview.label(), payload.url());
            }
        })
        .register_uri_scheme_protocol(WELCOME_SCHEME, |ctx, request| {
            let app = ctx.app_handle().clone();
            if request.uri().path() == "/pick" {
                pick_project(&app);
                return tauri::http::Response::builder()
                    .header("Content-Type", "application/json")
                    .header("Access-Control-Allow-Origin", "*")
                    .body(br#"{"ok":true}"#.to_vec())
                    .expect("static response");
            }
            let html = welcome_html(&app.state::<Shell>().notice());
            tauri::http::Response::builder()
                .header("Content-Type", "text/html; charset=utf-8")
                .header("Access-Control-Allow-Origin", "*")
                .body(html.into_bytes())
                .expect("static response")
        })
        .setup(move |app| {
            let handle = app.handle().clone();
            build_menu(&handle)?;
            if have_project {
                open_studio_window(&handle)?;
            } else {
                open_welcome_window(&handle)?;
            }
            Ok(())
        })
        .on_menu_event(|app, event| {
            let id = event.id().as_ref();
            if let Some(method) = menu_method(id) {
                dispatch_from_menu(app, method);
            } else if id == MENU_OPEN {
                pick_project(app);
            }
        })
        .run(tauri::generate_context!())
        .expect("degen-paint failed to start");
}
