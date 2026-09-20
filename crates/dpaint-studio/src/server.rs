//! A small localhost HTTP bridge so the browser build of the UI can drive the native engine.
//!
//! Hand-rolled on `std::net` rather than pulling in a web framework and an async runtime:
//! the surface is four routes on loopback, and a dependency-free server keeps `dpaint serve`
//! as cheap to start as the rest of the CLI.

use crate::api::Studio;
use dpaint_core::{Error, Result};
use serde_json::json;
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
    let local = listener
        .local_addr()
        .map(|a| a.to_string())
        .unwrap_or_else(|_| config.addr.clone());
    println!(
        "degen-paint studio on http://{local}  (project: {})",
        studio.root().display()
    );

    let studio = Arc::new(studio);
    let ui_dir = config.ui_dir.clone();
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let studio = Arc::clone(&studio);
        let ui_dir = ui_dir.clone();
        std::thread::spawn(move || {
            if let Err(e) = handle(stream, &studio, ui_dir.as_deref()) {
                eprintln!("studio: {e}");
            }
        });
    }
    Ok(())
}

fn handle(mut stream: TcpStream, studio: &Studio, ui_dir: Option<&Path>) -> Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request_line = String::new();
    if reader.read_line(&mut request_line)? == 0 {
        return Ok(());
    }
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let target = parts.next().unwrap_or("/").to_string();

    let mut length = 0usize;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            break;
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some(v) = line.strip_prefix("Content-Length:") {
            length = v.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0u8; length];
    if length > 0 {
        reader.read_exact(&mut body)?;
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
                Ok((png, size)) => {
                    let headers = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: {}\r\nX-Render-Size: {}x{}\r\nCache-Control: no-store\r\n\r\n",
                        png.len(), size[0], size[1]
                    );
                    stream.write_all(headers.as_bytes())?;
                    stream.write_all(&png)?;
                    Ok(())
                }
                Err(e) => reply_json(
                    &mut stream,
                    500,
                    &json!({ "ok": false, "error": e.detail() }),
                ),
            }
        }
        ("GET", _) => serve_ui(&mut stream, &path, ui_dir),
        _ => reply_json(
            &mut stream,
            405,
            &json!({ "ok": false, "error": "method not allowed" }),
        ),
    }
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

fn parse_query(q: &str) -> std::collections::BTreeMap<String, String> {
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
}
