//! QuiverAI client: prompt → SVG, and raster → SVG.
//!
//! ```text
//! POST https://api.quiver.ai/v1/svgs/generations     -> text (+ references) -> SVG
//! POST https://api.quiver.ai/v1/svgs/vectorizations  -> raster image -> SVG
//! ```
//!
//! The SVG that comes back is *material*: the caller parses it into editable objects. This
//! client's only job is to get the markup, never to store it as a blob.

use crate::config::AiConfig;
use crate::keys::Provider;
use crate::transport::{HttpRequest, Transport};
use dpaint_core::{Error, Result};
use serde_json::{json, Value};

pub struct QuiverClient<'a> {
    transport: &'a dyn Transport,
    cfg: &'a AiConfig,
    key: String,
}

#[derive(Debug, Clone)]
pub struct QuiverRun {
    pub request_id: Option<String>,
    pub svgs: Vec<String>,
    pub reported_cost: Option<f64>,
}

pub fn generations_url(cfg: &AiConfig) -> String {
    format!(
        "{}/svgs/generations",
        cfg.quiver.base_url.trim_end_matches('/')
    )
}

pub fn vectorizations_url(cfg: &AiConfig) -> String {
    format!(
        "{}/svgs/vectorizations",
        cfg.quiver.base_url.trim_end_matches('/')
    )
}

impl<'a> QuiverClient<'a> {
    pub fn new(transport: &'a dyn Transport, cfg: &'a AiConfig, key: &str) -> Self {
        Self {
            transport,
            cfg,
            key: key.to_string(),
        }
    }

    fn err(detail: impl Into<String>) -> Error {
        Error::ProviderError {
            provider: "quiver".into(),
            detail: detail.into(),
        }
    }

    pub fn generations_url(&self) -> String {
        generations_url(self.cfg)
    }

    pub fn vectorizations_url(&self) -> String {
        vectorizations_url(self.cfg)
    }

    pub fn post(&self, url: &str, body: &Value) -> Result<QuiverRun> {
        let req = HttpRequest::post_json(url, body)
            .header("authorization", Provider::Quiver.auth_header(&self.key));
        let resp = self.transport.request(req)?;
        if !resp.is_success() {
            return Err(Self::err(format!(
                "HTTP {} from {url}: {}",
                resp.status,
                resp.error_text()
            )));
        }
        let payload = resp.parse_json()?;
        let mut svgs = Vec::new();
        collect_svgs(&payload, &mut svgs);

        if svgs.is_empty() {
            // Some responses hand back a URL instead of inline markup.
            for url in svg_urls(&payload) {
                let r = self.transport.request(HttpRequest::get(&url))?;
                if r.is_success() {
                    let text = String::from_utf8_lossy(&r.body).to_string();
                    if text.contains("<svg") {
                        svgs.push(text);
                    }
                }
            }
        }
        if svgs.is_empty() {
            return Err(Self::err("response contained no SVG"));
        }
        Ok(QuiverRun {
            request_id: payload
                .get("id")
                .or_else(|| payload.get("request_id"))
                .and_then(Value::as_str)
                .map(str::to_string),
            svgs,
            reported_cost: cost_of(&payload),
        })
    }
}

/// Request body for a generation. `stream` is off: the CLI and MCP want one complete answer,
/// and the SSE phases only matter to a live GUI preview.
pub fn generation_body(
    model: &str,
    prompt: &str,
    instructions: Option<&str>,
    n: u32,
    seed: Option<i64>,
) -> Value {
    let mut body = json!({ "model": model, "prompt": prompt, "n": n, "stream": false });
    if let Some(i) = instructions {
        body["instructions"] = json!(i);
    }
    if let Some(s) = seed {
        body["seed"] = json!(s);
    }
    body
}

pub fn vectorization_body(model: &str, image: &str, auto_crop: bool) -> Value {
    json!({ "model": model, "image": image, "auto_crop": auto_crop, "stream": false })
}

/// Every inline SVG in a response, in document order — robust to `svgs[]`, `data[]`,
/// `content`, and single-object shapes alike.
fn collect_svgs(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::String(s) if s.contains("<svg") => out.push(s.clone()),
        Value::Array(a) => a.iter().for_each(|x| collect_svgs(x, out)),
        Value::Object(m) => m.values().for_each(|x| collect_svgs(x, out)),
        _ => {}
    }
}

fn svg_urls(v: &Value) -> Vec<String> {
    let mut out = Vec::new();
    fn rec(v: &Value, out: &mut Vec<String>) {
        match v {
            Value::Object(m) => {
                if let Some(u) = m.get("url").and_then(Value::as_str) {
                    out.push(u.to_string());
                }
                m.values().for_each(|x| rec(x, out));
            }
            Value::Array(a) => a.iter().for_each(|x| rec(x, out)),
            _ => {}
        }
    }
    rec(v, &mut out);
    out
}

fn cost_of(v: &Value) -> Option<f64> {
    for key in ["cost_usd", "costUsd", "cost"] {
        if let Some(n) = v.get(key).and_then(Value::as_f64) {
            return Some(n);
        }
        if let Some(n) = v
            .get("usage")
            .and_then(|u| u.get(key))
            .and_then(Value::as_f64)
        {
            return Some(n);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::{Method, RecordedTransport};

    #[test]
    fn a_generation_posts_bearer_auth_and_reads_every_variant_back() {
        let cfg = AiConfig::default();
        let t = RecordedTransport::new().on_json(
            Method::Post,
            "/svgs/generations",
            json!({
                "id": "gen_42",
                "model": "arrow-2",
                "svgs": [
                    {"content": "<svg viewBox=\"0 0 10 10\"><path d=\"M0 0 H10\"/></svg>"},
                    {"content": "<svg viewBox=\"0 0 10 10\"><path d=\"M0 10 H10\"/></svg>"}
                ],
                "usage": {"cost_usd": 0.07}
            }),
        );

        let client = QuiverClient::new(&t, &cfg, "qv-key");
        let run = client
            .post(
                &client.generations_url(),
                &generation_body(
                    "arrow-2-telos",
                    "a lion crest",
                    Some("clean geometry"),
                    2,
                    Some(3),
                ),
            )
            .unwrap();

        assert_eq!(run.svgs.len(), 2);
        assert_eq!(run.request_id.as_deref(), Some("gen_42"));
        assert_eq!(run.reported_cost, Some(0.07));

        let call = &t.calls()[0];
        assert_eq!(call.url, "https://api.quiver.ai/v1/svgs/generations");
        assert_eq!(call.header_value("authorization"), Some("Bearer qv-key"));
        let body = call.json().unwrap();
        assert_eq!(body["model"], "arrow-2-telos");
        assert_eq!(body["instructions"], "clean geometry");
        assert_eq!(body["n"], 2);
        assert_eq!(body["stream"], false);
    }

    #[test]
    fn a_response_without_svg_is_a_provider_error() {
        let cfg = AiConfig::default();
        let t = RecordedTransport::new().on_json(
            Method::Post,
            "/svgs/vectorizations",
            json!({"id": "v1", "status": "queued"}),
        );
        let client = QuiverClient::new(&t, &cfg, "k");
        let err = client
            .post(
                &client.vectorizations_url(),
                &vectorization_body("arrow-2", "data:image/png;base64,AA", true),
            )
            .unwrap_err();
        assert_eq!(err.code(), "provider_error");
        assert_eq!(err.exit_code(), 5);
    }
}
