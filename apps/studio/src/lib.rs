//! degen-paint Studio, desktop shell.
//!
//! A thin native wrapper around exactly the same static UI a browser gets from `dpaint serve`,
//! and exactly the same [`Studio`] API the HTTP bridge calls. Two commands, one managed
//! `Studio`, one init script that makes `invoke` look like `fetch` to the frontend, and a menu
//! bar whose items fire the UI's own events. Nothing about the document model lives here.

pub mod bridge;
pub mod menu;

use dpaint_studio::Studio;
use parking_lot::RwLock;
use serde_json::Value;
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Listener, Manager, State, WebviewUrl, WebviewWindowBuilder};

/// The managed state: one `Studio`, never absent.
///
/// A shell launched with no project holds an empty one, `dispatch("state")` answers
/// `project: null`, and the UI renders its Welcome screen from that — so there is one Welcome
/// screen in the product rather than a native one and a web one. Opening a project is a
/// `project.open` dispatch on the same `Studio`, not a new one.
///
/// `RwLock` and not `Mutex` because `state` is polled roughly every second per window and reads
/// should not queue behind each other.
pub struct Shell {
    studio: RwLock<Studio>,
}

impl Shell {
    pub fn new(studio: Studio) -> Self {
        Self {
            studio: RwLock::new(studio),
        }
    }

    pub fn root(&self) -> Option<PathBuf> {
        self.studio.read().root()
    }

    /// `state`, `op`, `undo`, `project.open`, … — the whole GUI surface.
    pub fn call(&self, method: &str, params: &Value) -> Result<Value, String> {
        bridge::call(&self.studio.read(), method, params)
    }

    pub fn render(&self, doc: Option<&str>, scale: f64, max: u32) -> Result<String, String> {
        bridge::render_data_uri(&self.studio.read(), doc, scale, max)
    }
}

// ---------------------------------------------------------------------------- commands

#[tauri::command]
async fn dpaint_call(
    app: AppHandle,
    shell: State<'_, Shell>,
    method: String,
    params: Value,
) -> Result<Value, String> {
    let out = shell.call(&method, &params);
    // A project opened or closed in the UI changes what this window is looking at. The title
    // bar and File > Open Recent are the shell's share of that, and nothing in the page can
    // reach either.
    if out.is_ok()
        && matches!(
            method.as_str(),
            "project.new" | "project.open" | "project.close"
        )
    {
        retitle(&app);
        let _ = menu::install(&app);
    }
    out
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

fn window_title(root: Option<&Path>) -> String {
    match root
        .and_then(Path::file_name)
        .map(|n| n.to_string_lossy().into_owned())
    {
        Some(n) => format!("degen-paint — {n}"),
        None => "degen-paint".to_string(),
    }
}

/// Put the open project's name back in the title bar.
pub fn retitle(app: &AppHandle) {
    let title = window_title(app.state::<Shell>().root().as_deref());
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.set_title(&title);
    }
}

/// The Studio window: the bundled UI plus the bridge, nothing else.
fn open_studio_window(app: &AppHandle) -> tauri::Result<()> {
    if let Some(w) = app.get_webview_window("main") {
        w.set_focus()?;
        return Ok(());
    }
    let title = window_title(app.state::<Shell>().root().as_deref());
    WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
        .title(title)
        .inner_size(1440.0, 900.0)
        .min_inner_size(920.0, 600.0)
        .initialization_script(bridge::INIT_SCRIPT)
        .build()?;
    Ok(())
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

/// Resolve the project: explicit `--project`, else discovery from the cwd, else nothing open.
///
/// Never fatal. A shell that refuses to start has nowhere to put the reason, and "no project"
/// is a state the Studio renders — so the reason goes to the terminal and the window opens
/// anyway. Only an explicit `--project` that does not open produces a complaint, because that
/// one the user typed.
pub fn resolve_project(arg: Option<PathBuf>) -> (Studio, Option<String>) {
    match arg {
        Some(p) => match Studio::open(&p) {
            Ok(s) => (s, None),
            Err(e) => (
                Studio::empty(),
                Some(format!("--project {}: {e}", p.display())),
            ),
        },
        None => match Studio::discover(".") {
            Ok(s) => (s, None),
            Err(_) => (Studio::empty(), None),
        },
    }
}

pub fn run() {
    // WebKitGTK's DMA-BUF renderer dies with "Error 71 (Protocol error) dispatching to
    // Wayland display" on Wayland compositors driven by the NVIDIA proprietary stack
    // (observed on Hyprland + webkit2gtk 2.52), taking the window with it before the UI
    // paints. WebKit's own escape hatch is this variable; set it before GTK initialises,
    // and only when the user has not already decided for themselves.
    #[cfg(target_os = "linux")]
    if std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none() {
        std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
    }

    // A GTK 3 window's Wayland `app_id` is `g_get_prgname()` — the executable's name — and not
    // the GApplication id, which `enableGTKAppId` sets. Measured on Hyprland 0.56: without
    // this the compositor reports `degen-paint`, so every `app_id` the pack hints, the
    // navigator's `neo app` and Starkbot's deny list use would miss this window. `gtk_init`
    // only fills prgname in when it is unset, so setting it first wins.
    #[cfg(target_os = "linux")]
    glib::set_prgname(Some(dpaint_studio::skill::APP_ID));

    let (studio, notice) = resolve_project(project_arg(std::env::args().skip(1)));

    // This binary is normally launched from a terminal, so say what it decided to open.
    // A window that comes up on the wrong project is otherwise silent.
    if let Some(n) = &notice {
        eprintln!("degen-paint: {n}");
    }
    match studio.root() {
        Some(root) => eprintln!("degen-paint: project {}", root.display()),
        None => eprintln!("degen-paint: no project open; the Studio opens on Welcome"),
    }

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(Shell::new(studio))
        .invoke_handler(tauri::generate_handler![dpaint_call, dpaint_render])
        // A blank webview is the hardest desktop bug to diagnose from a terminal, so each
        // window says when it finished loading and reports whether the injected bridge landed.
        .on_page_load(|webview, payload| {
            if matches!(payload.event(), tauri::webview::PageLoadEvent::Finished) {
                eprintln!(
                    "degen-paint: window '{}' loaded {}",
                    webview.label(),
                    payload.url()
                );
                let _ = webview.eval(bridge::BRIDGE_PROBE);
            }
        })
        .setup(move |app| {
            let handle = app.handle().clone();
            handle.listen("dpaint:bridge", |event| {
                eprintln!("degen-paint: bridge {}", event.payload());
            });
            menu::install(&handle)?;
            open_studio_window(&handle)?;
            Ok(())
        })
        .on_menu_event(|app, event| menu::on_event(app, event.id().as_ref()))
        .run(tauri::generate_context!())
        .expect("degen-paint failed to start");
}
