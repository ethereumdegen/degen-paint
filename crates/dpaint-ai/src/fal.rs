//! fal.ai queue client.
//!
//! ```text
//! POST https://queue.fal.run/{model}                       -> { request_id, status_url, response_url }
//! GET  https://queue.fal.run/{model}/requests/{id}/status  -> IN_QUEUE | IN_PROGRESS | COMPLETED
//! GET  https://queue.fal.run/{model}/requests/{id}         -> model-specific result
//! ```
//!
//! The model id is never a constant in this file: it is passed in from configuration.

use crate::config::AiConfig;
use crate::image;
use crate::keys::Provider;
use crate::transport::{HttpRequest, Transport};
use crate::Blob;
use dpaint_core::{Error, Result};
use serde_json::Value;

pub struct FalClient<'a> {
    transport: &'a dyn Transport,
    cfg: &'a AiConfig,
    key: String,
}

#[derive(Debug, Clone)]
pub struct FalRun {
    pub request_id: String,
    pub payload: Value,
    /// Price reported by the provider, when it reports one.
    pub reported_cost: Option<f64>,
}

impl<'a> FalClient<'a> {
    pub fn new(transport: &'a dyn Transport, cfg: &'a AiConfig, key: &str) -> Self {
        Self {
            transport,
            cfg,
            key: key.to_string(),
        }
    }

    fn auth(&self, req: HttpRequest) -> HttpRequest {
        req.header("authorization", Provider::Fal.auth_header(&self.key))
    }

    fn err(detail: impl Into<String>) -> Error {
        Error::ProviderError {
            provider: "fal".into(),
            detail: detail.into(),
        }
    }

    /// Submit, poll until `COMPLETED`, fetch the result.
    pub fn run(&self, model: &str, params: &Value) -> Result<FalRun> {
        let submit_url = format!("{}/{}", self.cfg.fal.base_url.trim_end_matches('/'), model);
        let resp = self
            .transport
            .request(self.auth(HttpRequest::post_json(&submit_url, params)))?;
        if !resp.is_success() {
            return Err(Self::err(format!(
                "submit failed with HTTP {}: {}",
                resp.status,
                resp.error_text()
            )));
        }
        let queued = resp.parse_json()?;
        let request_id = queued
            .get("request_id")
            .or_else(|| queued.get("requestId"))
            .and_then(Value::as_str)
            .ok_or_else(|| Self::err("queue response had no request_id"))?
            .to_string();

        let base = format!("{submit_url}/requests/{request_id}");
        let status_url = queued
            .get("status_url")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| format!("{base}/status"));
        let response_url = queued
            .get("response_url")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| base.clone());

        let mut reported_cost = value_cost(&queued);
        let mut attempt = 0;
        loop {
            let st = self
                .transport
                .request(self.auth(HttpRequest::get(&status_url)))?;
            if !st.is_success() {
                return Err(Self::err(format!(
                    "status check failed with HTTP {}: {}",
                    st.status,
                    st.error_text()
                )));
            }
            let body = st.parse_json()?;
            reported_cost = reported_cost.or_else(|| value_cost(&body));
            match body
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or_default()
            {
                "COMPLETED" => break,
                "IN_QUEUE" | "IN_PROGRESS" => {}
                other => {
                    let detail = body
                        .get("error")
                        .map(|e| e.to_string())
                        .unwrap_or_else(|| other.to_string());
                    return Err(Self::err(format!(
                        "job {request_id} reported '{other}': {detail}"
                    )));
                }
            }
            attempt += 1;
            if attempt >= self.cfg.poll_max_attempts {
                return Err(Self::err(format!(
                    "job {request_id} did not complete after {attempt} status checks"
                )));
            }
            if self.cfg.poll_interval_ms > 0 {
                std::thread::sleep(std::time::Duration::from_millis(self.cfg.poll_interval_ms));
            }
        }

        let result = self
            .transport
            .request(self.auth(HttpRequest::get(&response_url)))?;
        if !result.is_success() {
            return Err(Self::err(format!(
                "result fetch failed with HTTP {}: {}",
                result.status,
                result.error_text()
            )));
        }
        let payload = result.parse_json()?;
        Ok(FalRun {
            request_id,
            reported_cost: reported_cost.or_else(|| value_cost(&payload)),
            payload,
        })
    }

    /// Fetch every image a result refers to, in order.
    pub fn download_images(&self, payload: &Value) -> Result<Vec<Blob>> {
        let urls = image_urls(payload);
        if urls.is_empty() {
            return Err(Self::err(format!(
                "result carried no image: {}",
                truncate(&payload.to_string())
            )));
        }
        urls.iter().map(|u| self.download(u)).collect()
    }

    pub fn download(&self, url: &str) -> Result<Blob> {
        if let Some(rest) = url.strip_prefix("data:") {
            return decode_data_uri(rest);
        }
        let resp = self.transport.request(HttpRequest::get(url))?;
        if !resp.is_success() {
            return Err(Self::err(format!(
                "download of {url} failed with HTTP {}",
                resp.status
            )));
        }
        let ext = image::ext_for(resp.header_value("content-type"), url);
        Ok(Blob {
            bytes: resp.body,
            ext,
        })
    }
}

fn decode_data_uri(rest: &str) -> Result<Blob> {
    use base64::Engine;
    let (meta, data) = rest
        .split_once(',')
        .ok_or_else(|| FalClient::err("malformed data URI"))?;
    let mime = meta.split(';').next().unwrap_or("image/png");
    let bytes = if meta.contains("base64") {
        base64::engine::general_purpose::STANDARD
            .decode(data.trim())
            .map_err(|e| FalClient::err(format!("bad base64 in data URI: {e}")))?
    } else {
        data.as_bytes().to_vec()
    };
    let ext = match mime {
        "image/jpeg" => "jpg",
        "image/webp" => "webp",
        "image/tiff" => "tiff",
        _ => "png",
    };
    Ok(Blob {
        bytes,
        ext: ext.to_string(),
    })
}

/// Image URLs in a fal result, covering the `images[]`, `image{}` and bare-`url` shapes the
/// different endpoints use.
pub fn image_urls(payload: &Value) -> Vec<String> {
    fn push(v: &Value, out: &mut Vec<String>) {
        if let Some(u) = v.as_str() {
            out.push(u.to_string());
        } else if let Some(u) = v.get("url").and_then(Value::as_str) {
            out.push(u.to_string());
        }
    }
    let mut out = Vec::new();
    if let Some(arr) = payload.get("images").and_then(Value::as_array) {
        arr.iter().for_each(|v| push(v, &mut out));
    }
    for key in ["image", "output", "url"] {
        if out.is_empty() {
            if let Some(v) = payload.get(key) {
                push(v, &mut out);
            }
        }
    }
    out
}

fn value_cost(v: &Value) -> Option<f64> {
    for key in ["cost_usd", "costUsd", "cost"] {
        if let Some(n) = v.get(key).and_then(Value::as_f64) {
            return Some(n);
        }
    }
    None
}

fn truncate(s: &str) -> String {
    if s.len() > 200 {
        format!("{}…", &s[..200])
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::{HttpResponse, Method, RecordedTransport};
    use serde_json::json;

    fn cfg() -> AiConfig {
        AiConfig {
            poll_interval_ms: 0,
            ..AiConfig::default()
        }
    }

    #[test]
    fn a_job_is_submitted_polled_until_completed_then_fetched() {
        let t = RecordedTransport::new()
            .on_json(
                Method::Post,
                "https://queue.fal.run/fal-ai/flux/dev",
                json!({
                    "request_id": "req-1",
                    "status_url": "https://queue.fal.run/fal-ai/flux/dev/requests/req-1/status",
                    "response_url": "https://queue.fal.run/fal-ai/flux/dev/requests/req-1"
                }),
            )
            .on_json(
                Method::Get,
                "/requests/req-1/status",
                json!({"status": "IN_QUEUE"}),
            )
            .on_json(
                Method::Get,
                "/requests/req-1/status",
                json!({"status": "IN_PROGRESS"}),
            )
            .on_json(
                Method::Get,
                "/requests/req-1/status",
                json!({"status": "COMPLETED"}),
            )
            .on(
                Method::Get,
                "https://queue.fal.run/fal-ai/flux/dev/requests/req-1",
                HttpResponse::json(200, &json!({"images": [{"url": "https://cdn/x.png"}]})),
            );

        let cfg = cfg();
        let client = FalClient::new(&t, &cfg, "sk-test");
        let run = client
            .run("fal-ai/flux/dev", &json!({"prompt": "a barn"}))
            .unwrap();

        assert_eq!(run.request_id, "req-1");
        assert_eq!(
            image_urls(&run.payload),
            vec!["https://cdn/x.png".to_string()]
        );

        let calls = t.calls();
        assert_eq!(calls[0].url, "https://queue.fal.run/fal-ai/flux/dev");
        assert_eq!(calls[0].header_value("authorization"), Some("Key sk-test"));
        assert_eq!(calls[0].json().unwrap()["prompt"], "a barn");
        // Three status checks, and polling stopped at COMPLETED rather than looping on.
        assert_eq!(t.calls_matching("/status").len(), 3);
        assert_eq!(t.calls_matching("/requests/req-1").len(), 4);
    }

    #[test]
    fn a_failed_job_surfaces_the_provider_message() {
        let t = RecordedTransport::new()
            .on_json(
                Method::Post,
                "queue.fal.run",
                json!({"request_id": "req-2"}),
            )
            .on_json(
                Method::Get,
                "/status",
                json!({"status": "FAILED", "error": "content filter"}),
            );
        let cfg = cfg();
        let err = FalClient::new(&t, &cfg, "k")
            .run("fal-ai/flux/dev", &json!({}))
            .unwrap_err();
        assert_eq!(err.code(), "provider_error");
        assert!(err.to_string().contains("content filter"), "{err}");
        assert_eq!(err.exit_code(), 5);
    }

    #[test]
    fn data_uri_results_are_decoded_without_a_second_request() {
        let t = RecordedTransport::new();
        let cfg = cfg();
        let client = FalClient::new(&t, &cfg, "k");
        let blob = client
            .download("data:image/jpeg;base64,/9j/4AAQSkZJRg==")
            .unwrap();
        assert_eq!(blob.ext, "jpg");
        assert_eq!(&blob.bytes[..3], &[0xff, 0xd8, 0xff]);
        assert_eq!(t.call_count(), 0);
    }
}
