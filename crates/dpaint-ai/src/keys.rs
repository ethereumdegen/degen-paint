//! Provider credentials.
//!
//! Resolution order, first hit wins: explicit key, environment, OS keychain, then
//! `~/.config/degen-paint/config.toml`. A key never reaches `project.json`, `history.jsonl`,
//! an asset, a log line or an error message — which is why keys are supplied to the crate
//! here rather than as op arguments (op arguments are journaled verbatim).

use crate::config::ConfigFile;
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Provider {
    Fal,
    Quiver,
}

impl Provider {
    pub const ALL: [Provider; 2] = [Provider::Fal, Provider::Quiver];

    pub fn id(self) -> &'static str {
        match self {
            Provider::Fal => "fal",
            Provider::Quiver => "quiver",
        }
    }

    pub fn env_var(self) -> &'static str {
        match self {
            Provider::Fal => "FAL_KEY",
            Provider::Quiver => "QUIVERAI_API_KEY",
        }
    }

    /// `Authorization` header value for this provider's scheme.
    pub fn auth_header(self, key: &str) -> String {
        match self {
            Provider::Fal => format!("Key {key}"),
            Provider::Quiver => format!("Bearer {key}"),
        }
    }
}

impl std::fmt::Display for Provider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.id())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum KeySource {
    /// Passed in by the caller, e.g. `--api-key`.
    Explicit,
    Env,
    Keychain,
    ConfigFile,
}

impl KeySource {
    pub fn as_str(self) -> &'static str {
        match self {
            KeySource::Explicit => "explicit",
            KeySource::Env => "env",
            KeySource::Keychain => "keychain",
            KeySource::ConfigFile => "config-file",
        }
    }
}

/// A resolved credential. The secret is private and `Debug` is redacted, so it cannot be
/// printed into a log or an error by accident.
#[derive(Clone)]
pub struct ApiKey {
    secret: String,
    source: KeySource,
}

impl ApiKey {
    pub fn new(secret: impl Into<String>, source: KeySource) -> Self {
        Self {
            secret: secret.into(),
            source,
        }
    }

    pub fn expose(&self) -> &str {
        &self.secret
    }

    pub fn source(&self) -> KeySource {
        self.source
    }
}

impl std::fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApiKey")
            .field("secret", &"<redacted>")
            .field("source", &self.source)
            .finish()
    }
}

pub trait KeyStore: Send + Sync {
    fn key(&self, provider: Provider) -> Option<ApiKey>;

    /// Why a lookup produced nothing, when there is something worth saying — for example a
    /// config file that was ignored because its permissions are too open.
    fn note(&self, _provider: Provider) -> Option<String> {
        None
    }
}

/// The real resolution chain.
pub struct SystemKeys {
    explicit: BTreeMap<Provider, String>,
    config_path: Option<PathBuf>,
    use_env: bool,
    use_keychain: bool,
}

impl Default for SystemKeys {
    fn default() -> Self {
        Self::new()
    }
}

impl SystemKeys {
    pub fn new() -> Self {
        Self {
            explicit: BTreeMap::new(),
            config_path: crate::config::default_config_path(),
            use_env: true,
            use_keychain: true,
        }
    }

    pub fn with_explicit(mut self, provider: Provider, key: impl Into<String>) -> Self {
        self.explicit.insert(provider, key.into());
        self
    }

    pub fn with_config_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.config_path = Some(path.into());
        self
    }

    pub fn without_env(mut self) -> Self {
        self.use_env = false;
        self
    }

    pub fn without_keychain(mut self) -> Self {
        self.use_keychain = false;
        self
    }
}

impl KeyStore for SystemKeys {
    fn key(&self, provider: Provider) -> Option<ApiKey> {
        if let Some(k) = self.explicit.get(&provider) {
            return Some(ApiKey::new(k.clone(), KeySource::Explicit));
        }
        if self.use_env {
            if let Some(v) = std::env::var(provider.env_var())
                .ok()
                .filter(|v| !v.trim().is_empty())
            {
                return Some(ApiKey::new(v.trim().to_string(), KeySource::Env));
            }
        }
        if self.use_keychain {
            if let Some(v) = keychain_key(provider) {
                return Some(ApiKey::new(v, KeySource::Keychain));
            }
        }
        let path = self.config_path.as_deref()?;
        config_key(path, provider).map(|k| ApiKey::new(k, KeySource::ConfigFile))
    }

    fn note(&self, provider: Provider) -> Option<String> {
        let path = self.config_path.as_deref()?;
        if self.key(provider).is_some() {
            return None;
        }
        if file_is_group_or_world_accessible(path) && raw_config_key(path, provider).is_some() {
            return Some(format!(
                "ignored the key in {} because the file is group- or world-accessible; chmod 600 it",
                path.display()
            ));
        }
        None
    }
}

/// A fixed set of keys. Used by tests and by any caller that wants to guarantee no ambient
/// credential from the machine is consulted.
#[derive(Debug, Default, Clone)]
pub struct StaticKeys {
    keys: BTreeMap<Provider, String>,
}

impl StaticKeys {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with(mut self, provider: Provider, key: impl Into<String>) -> Self {
        self.keys.insert(provider, key.into());
        self
    }
}

impl KeyStore for StaticKeys {
    fn key(&self, provider: Provider) -> Option<ApiKey> {
        self.keys
            .get(&provider)
            .map(|k| ApiKey::new(k.clone(), KeySource::Explicit))
    }
}

pub const KEYCHAIN_SERVICE: &str = "degen-paint";

#[cfg(feature = "keychain")]
fn keychain_key(provider: Provider) -> Option<String> {
    let entry = keyring::Entry::new(KEYCHAIN_SERVICE, provider.id()).ok()?;
    entry.get_password().ok().filter(|v| !v.trim().is_empty())
}

#[cfg(not(feature = "keychain"))]
fn keychain_key(_provider: Provider) -> Option<String> {
    None
}

/// A key from `config.toml`, but only if the file is not readable by anyone else.
fn config_key(path: &Path, provider: Provider) -> Option<String> {
    if file_is_group_or_world_accessible(path) {
        return None;
    }
    raw_config_key(path, provider)
}

fn raw_config_key(path: &Path, provider: Provider) -> Option<String> {
    let ai = ConfigFile::read(path)?.ai?;
    let key = match provider {
        Provider::Fal => ai.fal?.key?,
        Provider::Quiver => ai.quiver?.key?,
    };
    Some(key).filter(|v| !v.trim().is_empty())
}

#[cfg(unix)]
fn file_is_group_or_world_accessible(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    match std::fs::metadata(path) {
        Ok(m) => m.permissions().mode() & 0o077 != 0,
        Err(_) => false,
    }
}

#[cfg(not(unix))]
fn file_is_group_or_world_accessible(_path: &Path) -> bool {
    false
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderStatus {
    pub provider: &'static str,
    pub configured: bool,
    /// Where the key came from. Never the key itself.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<&'static str>,
    pub env_var: &'static str,
    pub models: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Which providers resolved, and from where. This is what `dpaint doctor` and
/// `ai.provider.status` report; it never reveals a credential.
pub fn providers_status(keys: &dyn KeyStore, cfg: &crate::config::AiConfig) -> Vec<ProviderStatus> {
    Provider::ALL
        .iter()
        .map(|&p| {
            let resolved = keys.key(p);
            ProviderStatus {
                provider: p.id(),
                configured: resolved.is_some(),
                source: resolved.as_ref().map(|k| k.source().as_str()),
                env_var: p.env_var(),
                models: match p {
                    Provider::Fal => cfg.fal.models(),
                    Provider::Quiver => cfg.quiver.models(),
                },
                note: keys.note(p),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_config(dir: &Path, body: &str, mode: u32) -> PathBuf {
        let path = dir.join("config.toml");
        std::fs::write(&path, body).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        }
        let _ = mode;
        path
    }

    #[test]
    fn explicit_key_wins_over_the_config_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_config(dir.path(), "[ai.fal]\nkey = \"from-file\"\n", 0o600);
        let store = SystemKeys::new()
            .without_env()
            .without_keychain()
            .with_config_path(&path)
            .with_explicit(Provider::Fal, "from-arg");

        let k = store.key(Provider::Fal).unwrap();
        assert_eq!(k.expose(), "from-arg");
        assert_eq!(k.source(), KeySource::Explicit);
    }

    #[test]
    fn config_file_key_is_used_when_nothing_earlier_matches() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_config(dir.path(), "[ai.quiver]\nkey = \"qv-123\"\n", 0o600);
        let store = SystemKeys::new()
            .without_env()
            .without_keychain()
            .with_config_path(&path);

        let k = store.key(Provider::Quiver).unwrap();
        assert_eq!(k.expose(), "qv-123");
        assert_eq!(k.source(), KeySource::ConfigFile);
        assert!(store.key(Provider::Fal).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn a_world_readable_config_key_is_refused_and_explained() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_config(dir.path(), "[ai.fal]\nkey = \"leaky\"\n", 0o644);
        let store = SystemKeys::new()
            .without_env()
            .without_keychain()
            .with_config_path(&path);

        assert!(store.key(Provider::Fal).is_none());
        let note = store.note(Provider::Fal).expect("a reason is reported");
        assert!(note.contains("chmod 600"), "{note}");
    }

    #[test]
    fn status_reports_the_source_and_never_the_key() {
        let cfg = crate::config::AiConfig::default();
        let keys = StaticKeys::new().with(Provider::Fal, "sk-secret-value");
        let status = providers_status(&keys, &cfg);
        let json = serde_json::to_string(&status).unwrap();

        assert!(!json.contains("sk-secret-value"), "{json}");
        let fal = status.iter().find(|s| s.provider == "fal").unwrap();
        assert!(fal.configured);
        assert_eq!(fal.source, Some("explicit"));
        assert_eq!(fal.models["generate"], "fal-ai/flux/dev");
        let quiver = status.iter().find(|s| s.provider == "quiver").unwrap();
        assert!(!quiver.configured);
        assert_eq!(quiver.env_var, "QUIVERAI_API_KEY");
    }

    #[test]
    fn env_is_consulted_after_an_explicit_key_and_before_the_file() {
        // Uses a provider-specific variable set only for this process.
        std::env::set_var("FAL_KEY", "env-key");
        let store = SystemKeys::new()
            .without_keychain()
            .with_config_path("/nonexistent");
        let k = store.key(Provider::Fal).unwrap();
        assert_eq!(k.source(), KeySource::Env);
        assert_eq!(k.expose(), "env-key");
        std::env::remove_var("FAL_KEY");
    }
}
