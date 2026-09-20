//! Optional generation providers.
//!
//! degen-paint is a complete editor with no AI configured; everything here is additive. A fal
//! image becomes an ordinary pixel layer with a transform, a mask and effects. A Quiver SVG is
//! parsed into ordinary Bézier objects you can boolean-subtract, offset, recolour or extrude.
//! Generation is a source of material, never a black box that emits a finished file.
//!
//! Three things make that safe to hand to a looping agent:
//!
//! - the network lives behind [`Transport`], so nothing here needs a key or a socket to test;
//! - every request is content-addressed and cached, so replay, undo/redo and retries never
//!   re-bill;
//! - a per-project budget refuses a call *before* it is sent.

pub mod budget;
pub mod cache;
pub mod config;
pub mod fal;
pub mod image;
pub mod keys;
pub mod ops;
pub mod quiver;
pub mod transport;

use std::sync::Arc;

pub use config::AiConfig;
pub use keys::{ApiKey, KeySource, KeyStore, Provider, ProviderStatus, StaticKeys, SystemKeys};
pub use transport::{HttpRequest, HttpResponse, Method, RecordedTransport, Transport};

/// One result blob from a provider, and the extension it is stored under.
#[derive(Debug, Clone)]
pub struct Blob {
    pub bytes: Vec<u8>,
    pub ext: String,
}

/// Everything an `ai.*` op needs that is not the document: where to send requests, how to
/// authenticate, and which models to use.
#[derive(Clone)]
pub struct Runtime {
    pub transport: Arc<dyn Transport>,
    pub keys: Arc<dyn KeyStore>,
    pub config: Arc<AiConfig>,
}

impl Runtime {
    pub fn new(
        transport: Arc<dyn Transport>,
        keys: Arc<dyn KeyStore>,
        config: Arc<AiConfig>,
    ) -> Self {
        Self { transport, keys, config }
    }

    /// The real thing: config from `~/.config/degen-paint/config.toml`, keys from the
    /// documented resolution chain, HTTP over reqwest.
    pub fn system() -> Self {
        let config = Arc::new(AiConfig::load());
        Self {
            transport: default_transport(&config),
            keys: Arc::new(SystemKeys::new()),
            config,
        }
    }
}

#[cfg(feature = "net")]
fn default_transport(config: &AiConfig) -> Arc<dyn Transport> {
    Arc::new(transport::ReqwestTransport::new(std::time::Duration::from_secs(
        config.timeout_secs,
    )))
}

#[cfg(not(feature = "net"))]
fn default_transport(_config: &AiConfig) -> Arc<dyn Transport> {
    Arc::new(transport::OfflineTransport)
}

/// The op catalog, wired to the real providers.
pub fn ops() -> Vec<Box<dyn dpaint_core::Op>> {
    ops_with(Runtime::system())
}

/// The op catalog, wired to an injected runtime — how tests, `--api-key` and `--offline` are
/// implemented without any of the ops knowing about them.
pub fn ops_with(rt: Runtime) -> Vec<Box<dyn dpaint_core::Op>> {
    ops::catalog(rt)
}

/// Which providers are configured and from where, for `dpaint doctor`. Never contains key
/// material.
pub fn providers_status() -> serde_json::Value {
    let cfg = AiConfig::load();
    let keys = SystemKeys::new();
    status_value(&keys::providers_status(&keys, &cfg))
}

/// The same report for an injected key store, so a caller that passed `--api-key` sees the
/// truth rather than the ambient environment.
pub fn providers_status_of(rt: &Runtime) -> serde_json::Value {
    status_value(&keys::providers_status(rt.keys.as_ref(), rt.config.as_ref()))
}

fn status_value(statuses: &[ProviderStatus]) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    for s in statuses {
        map.insert(
            s.provider.to_string(),
            serde_json::to_value(s).unwrap_or(serde_json::Value::Null),
        );
    }
    serde_json::Value::Object(map)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_is_keyed_by_provider_and_reports_configuration_without_the_key() {
        let rt = Runtime::new(
            Arc::new(RecordedTransport::new()),
            Arc::new(StaticKeys::new().with(Provider::Fal, "sk-do-not-print")),
            Arc::new(AiConfig::default()),
        );
        let v = providers_status_of(&rt);
        assert_eq!(v["fal"]["configured"], true);
        assert_eq!(v["fal"]["source"], "explicit");
        assert_eq!(v["quiver"]["configured"], false);
        assert!(v["quiver"]["source"].is_null());
        assert!(!v.to_string().contains("sk-do-not-print"));
    }

    #[test]
    fn every_catalog_op_is_registrable_and_carries_an_object_schema() {
        let rt = Runtime::new(
            Arc::new(RecordedTransport::new()),
            Arc::new(StaticKeys::new()),
            Arc::new(AiConfig::default()),
        );
        let mut registry = dpaint_core::Registry::new();
        registry.extend(ops_with(rt));
        for id in registry.ids() {
            assert!(id.starts_with("ai."), "{id} is not in the ai domain");
            let op = registry.get(id).unwrap();
            let schema = op.schema();
            assert_eq!(schema["type"], "object", "{id} schema: {schema}");
            assert!(!op.about().is_empty(), "{id} has no description");
        }
    }
}
