//! The HTTP boundary.
//!
//! Every provider call goes through [`Transport`], so the ops are testable without a network,
//! without a key, and without a mock HTTP server. Tests inject [`RecordedTransport`], which
//! serves fixtures and records the exact requests that were made so a test can assert the
//! wire shape a provider actually sees.

use dpaint_core::{Error, Result};
use parking_lot::Mutex;
use std::collections::VecDeque;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
}

impl Method {
    pub fn as_str(self) -> &'static str {
        match self {
            Method::Get => "GET",
            Method::Post => "POST",
        }
    }
}

/// An outgoing request. `Debug` redacts credential headers: a request must be printable in a
/// log or an error message without leaking a key.
#[derive(Clone)]
pub struct HttpRequest {
    pub method: Method,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
}

impl HttpRequest {
    pub fn get(url: impl Into<String>) -> Self {
        Self {
            method: Method::Get,
            url: url.into(),
            headers: Vec::new(),
            body: None,
        }
    }

    pub fn post(url: impl Into<String>) -> Self {
        Self {
            method: Method::Post,
            url: url.into(),
            headers: Vec::new(),
            body: None,
        }
    }

    pub fn post_json(url: impl Into<String>, body: &serde_json::Value) -> Self {
        Self::post(url)
            .header("content-type", "application/json")
            .body(serde_json::to_vec(body).unwrap_or_default())
    }

    pub fn header(mut self, name: &str, value: impl Into<String>) -> Self {
        self.headers.push((name.to_ascii_lowercase(), value.into()));
        self
    }

    pub fn body(mut self, bytes: Vec<u8>) -> Self {
        self.body = Some(bytes);
        self
    }

    /// Case-insensitive header lookup.
    pub fn header_value(&self, name: &str) -> Option<&str> {
        let name = name.to_ascii_lowercase();
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(&name))
            .map(|(_, v)| v.as_str())
    }

    /// The request body parsed as JSON, for assertions and for provider clients that echo.
    pub fn json(&self) -> Option<serde_json::Value> {
        serde_json::from_slice(self.body.as_deref()?).ok()
    }
}

fn is_secret_header(name: &str) -> bool {
    name.eq_ignore_ascii_case("authorization") || name.eq_ignore_ascii_case("x-api-key")
}

impl std::fmt::Debug for HttpRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let headers: Vec<(&str, &str)> = self
            .headers
            .iter()
            .map(|(k, v)| {
                (
                    k.as_str(),
                    if is_secret_header(k) {
                        "<redacted>"
                    } else {
                        v.as_str()
                    },
                )
            })
            .collect();
        f.debug_struct("HttpRequest")
            .field("method", &self.method)
            .field("url", &self.url)
            .field("headers", &headers)
            .field(
                "body_bytes",
                &self.body.as_ref().map(|b| b.len()).unwrap_or(0),
            )
            .finish()
    }
}

#[derive(Debug, Clone)]
pub struct HttpResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl HttpResponse {
    pub fn json(status: u16, body: &serde_json::Value) -> Self {
        Self {
            status,
            headers: vec![("content-type".into(), "application/json".into())],
            body: serde_json::to_vec(body).unwrap_or_default(),
        }
    }

    pub fn bytes(status: u16, content_type: &str, body: Vec<u8>) -> Self {
        Self {
            status,
            headers: vec![("content-type".into(), content_type.into())],
            body,
        }
    }

    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    pub fn header_value(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    pub fn parse_json(&self) -> Result<serde_json::Value> {
        serde_json::from_slice(&self.body).map_err(|e| Error::ProviderError {
            provider: "http".into(),
            detail: format!("response was not JSON: {e}"),
        })
    }

    /// Provider error text, truncated so an error message stays readable.
    pub fn error_text(&self) -> String {
        let text = String::from_utf8_lossy(&self.body);
        let trimmed = text.trim();
        if trimmed.len() > 400 {
            format!("{}…", &trimmed[..400])
        } else {
            trimmed.to_string()
        }
    }
}

pub trait Transport: Send + Sync {
    fn request(&self, req: HttpRequest) -> Result<HttpResponse>;
}

/// A transport that serves canned responses and remembers every request it was given.
///
/// Routes match on method plus a URL substring, in registration order. A route with several
/// queued responses serves them in order and then repeats the last one, which is exactly what
/// polling a job queue needs.
pub struct RecordedTransport {
    routes: Mutex<Vec<Route>>,
    calls: Mutex<Vec<HttpRequest>>,
}

struct Route {
    method: Method,
    contains: String,
    responses: VecDeque<HttpResponse>,
}

impl Default for RecordedTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl RecordedTransport {
    pub fn new() -> Self {
        Self {
            routes: Mutex::new(Vec::new()),
            calls: Mutex::new(Vec::new()),
        }
    }

    pub fn on(self, method: Method, url_contains: &str, response: HttpResponse) -> Self {
        {
            let mut routes = self.routes.lock();
            match routes
                .iter_mut()
                .find(|r| r.method == method && r.contains == url_contains)
            {
                Some(r) => r.responses.push_back(response),
                None => routes.push(Route {
                    method,
                    contains: url_contains.to_string(),
                    responses: VecDeque::from(vec![response]),
                }),
            }
        }
        self
    }

    pub fn on_json(self, method: Method, url_contains: &str, body: serde_json::Value) -> Self {
        self.on(method, url_contains, HttpResponse::json(200, &body))
    }

    pub fn on_bytes(self, url_contains: &str, content_type: &str, body: Vec<u8>) -> Self {
        self.on(
            Method::Get,
            url_contains,
            HttpResponse::bytes(200, content_type, body),
        )
    }

    pub fn calls(&self) -> Vec<HttpRequest> {
        self.calls.lock().clone()
    }

    pub fn call_count(&self) -> usize {
        self.calls.lock().len()
    }

    /// Requests whose URL contains `needle`, for targeted assertions.
    pub fn calls_matching(&self, needle: &str) -> Vec<HttpRequest> {
        self.calls
            .lock()
            .iter()
            .filter(|c| c.url.contains(needle))
            .cloned()
            .collect()
    }
}

impl Transport for RecordedTransport {
    fn request(&self, req: HttpRequest) -> Result<HttpResponse> {
        self.calls.lock().push(req.clone());
        let mut routes = self.routes.lock();
        for r in routes.iter_mut() {
            if r.method == req.method && req.url.contains(&r.contains) {
                let resp = if r.responses.len() > 1 {
                    r.responses.pop_front().expect("len > 1")
                } else {
                    r.responses
                        .front()
                        .cloned()
                        .ok_or_else(|| Error::ProviderError {
                            provider: "recorded".into(),
                            detail: format!("route '{}' has no responses left", r.contains),
                        })?
                };
                return Ok(resp);
            }
        }
        Err(Error::ProviderError {
            provider: "recorded".into(),
            detail: format!("no fixture for {} {}", req.method.as_str(), req.url),
        })
    }
}

/// Used when the crate is built without the `net` feature: everything local still works,
/// every network op fails loudly instead of silently pretending.
pub struct OfflineTransport;

impl Transport for OfflineTransport {
    fn request(&self, req: HttpRequest) -> Result<HttpResponse> {
        Err(Error::ProviderError {
            provider: "offline".into(),
            detail: format!(
                "this build has no network support (feature `net` is off); refused {} {}",
                req.method.as_str(),
                req.url
            ),
        })
    }
}

#[cfg(feature = "net")]
pub use net::ReqwestTransport;

#[cfg(feature = "net")]
mod net {
    use super::*;
    use std::time::Duration;

    pub struct ReqwestTransport {
        client: std::result::Result<reqwest::blocking::Client, String>,
    }

    impl ReqwestTransport {
        pub fn new(timeout: Duration) -> Self {
            let client = reqwest::blocking::Client::builder()
                .timeout(timeout)
                .user_agent(concat!("degen-paint/", env!("CARGO_PKG_VERSION")))
                .build()
                .map_err(|e| e.to_string());
            Self { client }
        }
    }

    impl Transport for ReqwestTransport {
        fn request(&self, req: HttpRequest) -> Result<HttpResponse> {
            let client = self.client.as_ref().map_err(|e| Error::ProviderError {
                provider: "http".into(),
                detail: format!("could not build HTTP client: {e}"),
            })?;
            let mut builder = match req.method {
                Method::Get => client.get(&req.url),
                Method::Post => client.post(&req.url),
            };
            for (k, v) in &req.headers {
                builder = builder.header(k.as_str(), v.as_str());
            }
            if let Some(body) = req.body.clone() {
                builder = builder.body(body);
            }
            let resp = builder.send().map_err(|e| Error::ProviderError {
                provider: "http".into(),
                // `e` carries the URL and the kind, never the headers, so no key can leak.
                detail: e.to_string(),
            })?;
            let status = resp.status().as_u16();
            let headers = resp
                .headers()
                .iter()
                .map(|(k, v)| {
                    (
                        k.as_str().to_string(),
                        v.to_str().unwrap_or_default().to_string(),
                    )
                })
                .collect();
            let body = resp
                .bytes()
                .map_err(|e| Error::ProviderError {
                    provider: "http".into(),
                    detail: e.to_string(),
                })?
                .to_vec();
            Ok(HttpResponse {
                status,
                headers,
                body,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_never_prints_the_key() {
        let req = HttpRequest::post("https://queue.fal.run/fal-ai/flux/dev")
            .header("authorization", "Key sk-super-secret");
        let printed = format!("{req:?}");
        assert!(!printed.contains("sk-super-secret"), "{printed}");
        assert!(printed.contains("<redacted>"), "{printed}");
        // The value itself is still reachable for the transport that must send it.
        assert_eq!(
            req.header_value("Authorization"),
            Some("Key sk-super-secret")
        );
    }

    #[test]
    fn recorded_transport_repeats_its_last_response_and_records_calls() {
        let t = RecordedTransport::new()
            .on_json(
                Method::Get,
                "/status",
                serde_json::json!({"status": "IN_PROGRESS"}),
            )
            .on_json(
                Method::Get,
                "/status",
                serde_json::json!({"status": "COMPLETED"}),
            );

        let first = t
            .request(HttpRequest::get("https://x/requests/1/status"))
            .unwrap();
        let second = t
            .request(HttpRequest::get("https://x/requests/1/status"))
            .unwrap();
        let third = t
            .request(HttpRequest::get("https://x/requests/1/status"))
            .unwrap();

        assert_eq!(first.parse_json().unwrap()["status"], "IN_PROGRESS");
        assert_eq!(second.parse_json().unwrap()["status"], "COMPLETED");
        assert_eq!(third.parse_json().unwrap()["status"], "COMPLETED");
        assert_eq!(t.call_count(), 3);
    }

    #[cfg(feature = "net")]
    #[test]
    fn the_real_transport_reports_a_refused_connection_as_a_provider_error() {
        // Port 1 refuses immediately: this exercises the reqwest path without a network.
        let t = ReqwestTransport::new(std::time::Duration::from_secs(2));
        let err = t
            .request(HttpRequest::post_json(
                "http://127.0.0.1:1/fal-ai/flux/dev",
                &serde_json::json!({"prompt": "x"}),
            ))
            .unwrap_err();
        assert_eq!(err.code(), "provider_error");
        assert_eq!(err.exit_code(), 5);
    }

    #[test]
    fn unrouted_request_is_a_provider_error_not_a_panic() {
        let t = RecordedTransport::new();
        let err = t.request(HttpRequest::get("https://nowhere/")).unwrap_err();
        assert_eq!(err.code(), "provider_error");
    }
}
