//! A small localhost HTTP bridge so the browser build of the UI can drive the native engine.
//!
//! Hand-rolled on `std::net` rather than pulling in a web framework and an async runtime:
//! the surface is a handful of routes on loopback, and a dependency-free server keeps
//! `dpaint serve` as cheap to start as the rest of the CLI.
//!
//! Two kinds of route. `POST /api` is the whole GUI surface and mutates; the read-only
//! `GET /api/v1/…` family is the grounding side channel — the answers an agent uses to verify
//! the step it just took, in the Studio's own JSON so there is no second serializer to drift.
//!
//! Loopback is not a trust boundary: any page in any browser tab can reach 127.0.0.1. So
//! every route, the mutating one included, refuses a request that announces a `Host` or
//! `Origin` other than this server's own.

use crate::api::Studio;
use dpaint_core::{AssetStore, Error, Result};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub struct ServerConfig {
    pub addr: String,
    /// Directory holding the UI. Falls back to the bundled copy when absent.
    pub ui_dir: Option<PathBuf>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            addr: "127.0.0.1:4317".into(),
            ui_dir: None,
        }
    }
}

/// Files of the bundled UI, compiled in so `dpaint serve` works from any directory.
const UI: &[(&str, &str, &str)] = &[
    (
        "/",
        "text/html; charset=utf-8",
        include_str!("../ui/index.html"),
    ),
    (
        "/index.html",
        "text/html; charset=utf-8",
        include_str!("../ui/index.html"),
    ),
    (
        "/studio.css",
        "text/css; charset=utf-8",
        include_str!("../ui/studio.css"),
    ),
    (
        "/studio.js",
        "text/javascript; charset=utf-8",
        include_str!("../ui/studio.js"),
    ),
];

pub fn serve(studio: Studio, config: ServerConfig) -> Result<()> {
    let listener = TcpListener::bind(&config.addr)
        .map_err(|e| Error::Invalid(format!("cannot bind {}: {e}", config.addr)))?;
    // The bound port is what the origin check compares against, so it has to be the real
    // one: `--addr 127.0.0.1:0` is a legitimate way to start.
    let local = listener
        .local_addr()
        .map_err(|e| Error::Invalid(format!("cannot read the bound address: {e}")))?;
    println!(
        "degen-paint studio on http://{local}  (project: {})",
        match studio.root() {
            Some(r) => r.display().to_string(),
            None => "none — open one from the Welcome screen".into(),
        }
    );

    let studio = Arc::new(studio);
    let ui_dir = config.ui_dir.clone();
    let port = local.port();
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let studio = Arc::clone(&studio);
        let ui_dir = ui_dir.clone();
        std::thread::spawn(move || {
            if let Err(e) = handle(stream, &studio, ui_dir.as_deref(), port) {
                eprintln!("studio: {e}");
            }
        });
    }
    Ok(())
}

fn handle(mut stream: TcpStream, studio: &Studio, ui_dir: Option<&Path>, port: u16) -> Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request_line = String::new();
    if reader.read_line(&mut request_line)? == 0 {
        return Ok(());
    }
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let target = parts.next().unwrap_or("/").to_string();

    let mut length = 0usize;
    let mut host = None;
    let mut origin = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            break;
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        if name.eq_ignore_ascii_case("content-length") {
            length = value.parse().unwrap_or(0);
        } else if name.eq_ignore_ascii_case("host") {
            host = Some(value.to_string());
        } else if name.eq_ignore_ascii_case("origin") {
            origin = Some(value.to_string());
        }
    }
    let mut body = vec![0u8; length];
    if length > 0 {
        reader.read_exact(&mut body)?;
    }

    if !is_own_origin(host.as_deref(), origin.as_deref(), port) {
        return reply_json(
            &mut stream,
            403,
            &json!({ "ok": false, "error": format!(
                "refused: this bridge only answers 127.0.0.1:{port}") }),
        );
    }

    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (target.clone(), String::new()),
    };

    match (method.as_str(), path.as_str()) {
        ("POST", "/api") => {
            let req: serde_json::Value = serde_json::from_slice(&body).unwrap_or(json!({}));
            let m = req.get("method").and_then(|m| m.as_str()).unwrap_or("");
            let params = req.get("params").cloned().unwrap_or(json!({}));
            match studio.dispatch(m, &params) {
                Ok(v) => reply_json(&mut stream, 200, &json!({ "ok": true, "result": v })),
                Err(e) => reply_json(
                    &mut stream,
                    200,
                    &json!({ "ok": false, "error": e.detail(), "exitCode": e.exit_code() }),
                ),
            }
        }
        ("GET", "/render.png") => {
            let q = parse_query(&query);
            let doc = q.get("doc").map(|s| s.as_str());
            let scale = q.get("scale").and_then(|s| s.parse().ok()).unwrap_or(1.0);
            let max = q.get("max").and_then(|s| s.parse().ok()).unwrap_or(1600);
            match studio.render_png(doc, scale, max) {
                Ok((png, size)) => reply_png(&mut stream, &png, size, None),
                Err(e) => reply_error(&mut stream, &e),
            }
        }
        ("GET", "/annotate.png") => {
            let q = parse_query(&query);
            let scale = q.get("scale").and_then(|s| s.parse().ok()).unwrap_or(1.0);
            match annotate_png(studio, q.get("doc").map(|s| s.as_str()), scale) {
                Ok((png, size, legend)) => reply_png(&mut stream, &png, size, Some(&legend)),
                Err(e) => reply_error(&mut stream, &e),
            }
        }
        ("GET", _) => match api_get(studio, &path, &parse_query(&query)) {
            Some(Ok(v)) => reply_json(&mut stream, 200, &v),
            Some(Err(e)) => reply_error(&mut stream, &e),
            None => serve_ui(&mut stream, &path, ui_dir),
        },
        _ => reply_json(
            &mut stream,
            405,
            &json!({ "ok": false, "error": "method not allowed" }),
        ),
    }
}

/// Grounding: read-only, and the same JSON `dispatch` hands the UI. `None` means the path is
/// not one of ours, so the UI files get their chance.
fn api_get(studio: &Studio, path: &str, q: &BTreeMap<String, String>) -> Option<Result<Value>> {
    let route = path.strip_prefix("/api/v1/")?;
    let doc = || json!({ "doc": q.get("doc") });
    Some(match route {
        "status" => status(studio),
        "overview" => studio.dispatch("overview", &doc()),
        "history" => studio.dispatch(
            "history",
            &json!({ "limit": q.get("limit").and_then(|l| l.parse::<u64>().ok()) }),
        ),
        "select" => match q.get("q") {
            Some(sel) => {
                studio.dispatch("select", &json!({ "selector": sel, "doc": q.get("doc") }))
            }
            None => Err(Error::Invalid("missing 'q'".into())),
        },
        "skill" => crate::contract::skill_json(studio),
        _ => match route.split('/').collect::<Vec<_>>()[..] {
            ["doc", id, "digest"] => studio.dispatch("digest", &json!({ "doc": id })),
            ["doc", id, "lint"] => studio.dispatch("lint", &json!({ "doc": id })),
            ["jobs", id] => studio.dispatch("job.status", &json!({ "id": id })),
            _ => return None,
        },
    })
}

/// What a navigator asks between steps: has the revision moved, is anything still running.
fn status(studio: &Studio) -> Result<Value> {
    let state = studio.dispatch("state", &json!({}))?;
    Ok(json!({
        "project": state.get("project").cloned().unwrap_or(Value::Null),
        "revision": state.get("revision").cloned().unwrap_or(Value::Null),
        "activeDoc": state.pointer("/project/active").cloned().unwrap_or(Value::Null),
        "busy": crate::jobs::busy(),
    }))
}

/// The render with numbered bboxes, plus the legend that makes the numbers mean something.
/// This is what a vision model is allowed to look at: an image the app itself produced.
fn annotate_png(
    studio: &Studio,
    doc: Option<&str>,
    scale: f64,
) -> Result<(Vec<u8>, [u32; 2], Vec<dpaint_inspect::annotate::Legend>)> {
    let root = crate::jobs::project_root(studio)?;
    let ws = dpaint_core::Workspace::open(&root)?;
    let id = ws.project.resolve_doc(doc)?;
    let assets = AssetStore::new(&root);
    let opts = dpaint_inspect::DigestOptions {
        render: dpaint_render::RenderOptions {
            scale,
            ..Default::default()
        },
        ..Default::default()
    };
    let digest = dpaint_inspect::digest::digest(&ws.project, &id, &assets, &opts)?;
    let base = dpaint_render::render_document(&ws.project, &id, &assets, &opts.render)?;
    let (marked, legend) = dpaint_inspect::annotate::annotate(&base, &digest)?;
    let size = [marked.width(), marked.height()];
    let png = dpaint_render::encode::encode(
        &dpaint_render::encode::to_rgba(&marked),
        dpaint_render::ImageFormat::Png,
        100,
    )?;
    Ok((png, size, legend))
}

/// `Host`/`Origin` absent is a plain `curl` probe and allowed; anything that names a host
/// other than this server is a page somewhere else trying its luck.
fn is_own_origin(host: Option<&str>, origin: Option<&str>, port: u16) -> bool {
    let ours = |authority: &str| {
        let (h, p) = match authority.rsplit_once(':') {
            Some((h, p)) => (h, p.parse::<u16>().ok()),
            // A browser omits the port only when it is the scheme's default.
            None => (authority, Some(80)),
        };
        let h = h.trim_start_matches('[').trim_end_matches(']');
        matches!(h, "127.0.0.1" | "localhost" | "::1") && p == Some(port)
    };
    host.map_or(true, &ours)
        && origin.map_or(true, |o| ours(o.split_once("://").map_or(o, |(_, a)| a)))
}

fn serve_ui(stream: &mut TcpStream, path: &str, ui_dir: Option<&Path>) -> Result<()> {
    // A --ui-dir wins, so the frontend can be edited with live reload during development.
    if let Some(dir) = ui_dir {
        let rel = if path == "/" {
            "index.html"
        } else {
            path.trim_start_matches('/')
        };
        let file = dir.join(rel);
        if file.is_file() {
            let bytes = std::fs::read(&file)?;
            let ct = content_type(rel);
            let headers = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: {ct}\r\nContent-Length: {}\r\nCache-Control: no-store\r\n\r\n",
                bytes.len()
            );
            stream.write_all(headers.as_bytes())?;
            stream.write_all(&bytes)?;
            return Ok(());
        }
    }
    if let Some((_, ct, body)) = UI.iter().find(|(p, _, _)| *p == path) {
        let headers = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: {ct}\r\nContent-Length: {}\r\nCache-Control: no-store\r\n\r\n",
            body.len()
        );
        stream.write_all(headers.as_bytes())?;
        stream.write_all(body.as_bytes())?;
        return Ok(());
    }
    let body = b"not found";
    let headers = format!(
        "HTTP/1.1 404 Not Found\r\nContent-Type: text/plain\r\nContent-Length: {}\r\n\r\n",
        body.len()
    );
    stream.write_all(headers.as_bytes())?;
    stream.write_all(body)?;
    Ok(())
}

fn content_type(name: &str) -> &'static str {
    match name.rsplit('.').next().unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "json" => "application/json",
        "png" => "image/png",
        "svg" => "image/svg+xml",
        _ => "application/octet-stream",
    }
}

fn reply_json(stream: &mut TcpStream, status: u16, value: &serde_json::Value) -> Result<()> {
    let body = serde_json::to_vec(value)?;
    let reason = if status == 200 { "OK" } else { "Error" };
    let headers = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nCache-Control: no-store\r\n\r\n",
        body.len()
    );
    stream.write_all(headers.as_bytes())?;
    stream.write_all(&body)?;
    Ok(())
}

fn reply_png(
    stream: &mut TcpStream,
    png: &[u8],
    size: [u32; 2],
    legend: Option<&[dpaint_inspect::annotate::Legend]>,
) -> Result<()> {
    // The legend travels as a header so the image stays a plain PNG a browser can show.
    let legend = match legend {
        Some(l) => format!("X-Annotate-Legend: {}\r\n", serde_json::to_string(l)?),
        None => String::new(),
    };
    let headers = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: {}\r\nX-Render-Size: {}x{}\r\n{legend}Cache-Control: no-store\r\n\r\n",
        png.len(), size[0], size[1]
    );
    stream.write_all(headers.as_bytes())?;
    stream.write_all(png)?;
    Ok(())
}

/// A GET carries its outcome in the status line: a grounding probe branches on it without
/// parsing the body.
fn reply_error(stream: &mut TcpStream, e: &Error) -> Result<()> {
    let status = match e.code() {
        "no_such_document" => 404,
        "io_error" | "json_error" => 500,
        _ => 400,
    };
    reply_json(
        stream,
        status,
        &json!({ "ok": false, "error": e.detail(), "exitCode": e.exit_code() }),
    )
}

fn parse_query(q: &str) -> BTreeMap<String, String> {
    q.split('&')
        .filter(|s| !s.is_empty())
        .filter_map(|pair| pair.split_once('='))
        .map(|(k, v)| (k.to_string(), percent_decode(v)))
        .collect()
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => match u8::from_str_radix(&s[i + 1..i + 3], 16) {
                Ok(b) => {
                    out.push(b);
                    i += 3;
                }
                Err(_) => {
                    out.push(bytes[i]);
                    i += 1;
                }
            },
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use dpaint_core::doc::{Document, RasterDoc};
    use dpaint_core::{DocId, Project, Workspace};

    fn studio() -> (tempfile::TempDir, Studio) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("p.dpaint");
        let project = Project::new(
            "demo",
            Document::Raster(RasterDoc::new(DocId::from("doc_main"), "main", 32, 24)),
        );
        Workspace::create(&root, project).unwrap();
        let s = Studio::open(&root).unwrap();
        (tmp, s)
    }

    /// One real request over a real socket, through the real header parser. `{port}` in the
    /// request is substituted with the port the server believes it is bound to.
    fn request(studio: &Studio, raw: &str) -> (u16, String) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let mut client = TcpStream::connect(("127.0.0.1", port)).unwrap();
        client
            .write_all(raw.replace("{port}", &port.to_string()).as_bytes())
            .unwrap();
        let (server, _) = listener.accept().unwrap();
        handle(server, studio, None, port).unwrap();
        let mut out = String::new();
        client.read_to_string(&mut out).unwrap();
        let status = out
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let body = out.split_once("\r\n\r\n").map(|(_, b)| b).unwrap_or("");
        (status, body.to_string())
    }

    fn json_body(body: &str) -> Value {
        serde_json::from_str(body).unwrap_or_else(|e| panic!("not json: {e}: {body}"))
    }

    #[test]
    fn query_strings_decode_selectors_and_numbers() {
        let q = parse_query("doc=doc_main&scale=1.5&sel=%23lyr_sky");
        assert_eq!(q.get("doc").unwrap(), "doc_main");
        assert_eq!(q.get("scale").unwrap(), "1.5");
        assert_eq!(
            q.get("sel").unwrap(),
            "#lyr_sky",
            "selectors arrive percent-encoded"
        );
    }

    #[test]
    fn the_ui_is_compiled_in_so_serve_works_from_any_directory() {
        let index = UI.iter().find(|(p, _, _)| *p == "/").expect("index route");
        assert!(index.2.contains("<html"), "bundled index must be real html");
        assert!(UI.iter().any(|(p, _, _)| *p == "/studio.js"));
    }

    #[test]
    fn content_types_are_correct_for_the_files_the_ui_loads() {
        assert_eq!(content_type("studio.js"), "text/javascript; charset=utf-8");
        assert_eq!(content_type("studio.css"), "text/css; charset=utf-8");
        assert_eq!(content_type("thing.bin"), "application/octet-stream");
    }

    /// Any page in any tab can POST to loopback. Ops are not for them.
    #[test]
    fn a_foreign_origin_is_refused_while_the_same_request_without_one_is_served() {
        let (_t, s) = studio();
        let body = r#"{"method":"state","params":{}}"#;
        let with_origin = format!(
            "POST /api HTTP/1.1\r\nHost: 127.0.0.1:{{port}}\r\nOrigin: http://evil.example\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        let (status, refused) = request(&s, &with_origin);
        assert_eq!(status, 403);
        assert_eq!(json_body(&refused)["ok"], false);

        let plain = format!(
            "POST /api HTTP/1.1\r\nHost: 127.0.0.1:{{port}}\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        let (status, served) = request(&s, &plain);
        assert_eq!(status, 200);
        assert_eq!(json_body(&served)["ok"], true);
    }

    #[test]
    fn the_origin_check_covers_the_read_only_routes_and_a_foreign_host_header() {
        let (_t, s) = studio();
        let (status, _) = request(
            &s,
            "GET /api/v1/status HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nOrigin: http://evil.example\r\n\r\n",
        );
        assert_eq!(status, 403, "grounding must not be readable cross-origin");

        let (status, _) = request(
            &s,
            "GET /api/v1/status HTTP/1.1\r\nHost: dpaint.evil.example\r\n\r\n",
        );
        assert_eq!(status, 403, "a rebound DNS name is not this server");

        // curl, no Host at all: a legitimate probe.
        let (status, body) = request(&s, "GET /api/v1/status HTTP/1.1\r\n\r\n");
        assert_eq!(status, 200);
        assert_eq!(json_body(&body)["project"]["name"], "demo");
    }

    #[test]
    fn the_grounding_routes_answer_with_the_studios_own_json() {
        let (_t, s) = studio();
        s.dispatch(
            "op",
            &json!({ "op": "raster.layer.add",
                     "args": { "type": "fill", "color": "#ff0000", "name": "bg" } }),
        )
        .unwrap();

        let get = |path: &str| {
            let (status, body) = request(
                &s,
                &format!("GET {path} HTTP/1.1\r\nHost: localhost:{{port}}\r\n\r\n"),
            );
            (status, json_body(&body))
        };

        let (status, v) = get("/api/v1/status");
        assert_eq!(status, 200);
        assert_eq!(v["revision"], 1);
        assert_eq!(v["activeDoc"], "doc_main");
        // Membership, not emptiness: the job registry is per process, and a sibling test's
        // job may legitimately be running while this one reads.
        assert!(v["busy"].is_array(), "{v}");

        let (_, v) = get("/api/v1/overview");
        assert_eq!(v["documents"][0]["layers"], 1);

        let (_, v) = get("/api/v1/doc/doc_main/digest");
        assert_eq!(v["document"], "doc_main");

        let (_, v) = get("/api/v1/doc/doc_main/lint");
        assert!(v["findings"].is_array(), "{v}");

        let (_, v) = get("/api/v1/history?limit=1");
        assert_eq!(v["entries"].as_array().unwrap().len(), 1);

        let (_, v) = get("/api/v1/select?q=%23lyr_bg");
        assert_eq!(v[0]["id"], "lyr_bg", "{v}");

        let (_, v) = get("/api/v1/skill");
        assert_eq!(v["app"]["id"], "dev.degenpaint.studio");
        assert!(v["app"]["shortcuts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["id"] == "project.new"));

        let (status, v) = get("/api/v1/doc/doc_nope/digest");
        assert_eq!(status, 404, "{v}");
        assert_eq!(v["error"]["code"], "no_such_document");

        let (status, _) = get("/api/v1/jobs/job_nope");
        assert_eq!(status, 400);
    }

    #[test]
    fn the_annotated_preview_is_a_png_with_a_legend_for_its_numbers() {
        let (_t, s) = studio();
        s.dispatch(
            "op",
            &json!({ "op": "raster.layer.add",
                     "args": { "type": "fill", "color": "#00ff00", "name": "bg" } }),
        )
        .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let mut client = TcpStream::connect(("127.0.0.1", port)).unwrap();
        client
            .write_all(b"GET /annotate.png HTTP/1.1\r\n\r\n")
            .unwrap();
        let (server, _) = listener.accept().unwrap();
        handle(server, &s, None, port).unwrap();
        let mut out = Vec::new();
        client.read_to_end(&mut out).unwrap();
        let head = String::from_utf8_lossy(&out[..out.len().min(512)]).to_string();
        assert!(head.starts_with("HTTP/1.1 200"), "{head}");
        assert!(head.contains("X-Render-Size: 32x24"), "{head}");
        assert!(head.contains("X-Annotate-Legend: ["), "{head}");
        let split = out
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .expect("headers end");
        assert_eq!(&out[split + 5..split + 8], b"PNG");
    }

    /// The UI files still load: the origin check and the API routes must not shadow them.
    #[test]
    fn the_ui_is_still_served_to_its_own_origin() {
        let (_t, s) = studio();
        let (status, body) = request(
            &s,
            "GET /studio.js HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nOrigin: http://127.0.0.1:{port}\r\n\r\n",
        );
        assert_eq!(status, 200);
        assert!(!body.is_empty());
    }
}
