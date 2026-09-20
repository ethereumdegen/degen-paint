//! Request cache.
//!
//! An agent loops. A looping agent with an API key is a billing incident. Every provider
//! request is keyed by `blake3(provider ‖ model ‖ canonical_params ‖ input_hashes)` and its
//! result blobs stay in the content-addressed asset store, so replaying a journal, redoing an
//! undone op, or asking the same question twice costs nothing and hits no network.

use dpaint_core::{AssetRef, AssetStore, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheEntry {
    pub provider: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    pub assets: Vec<AssetRef>,
    /// What the original call cost. A cache hit costs zero; this records what was avoided.
    pub cost_usd: f64,
    pub at: String,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Cache {
    #[serde(default)]
    pub entries: BTreeMap<String, CacheEntry>,
    #[serde(skip)]
    path: PathBuf,
}

impl Cache {
    pub fn path_in(root: &Path) -> PathBuf {
        root.join("ai").join("cache.json")
    }

    pub fn load(root: &Path) -> Self {
        let path = Self::path_in(root);
        let mut cache: Cache = std::fs::read_to_string(&path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        cache.path = path;
        cache
    }

    /// A usable hit: recorded *and* every blob is still on disk. A pruned asset store must
    /// not produce a cache hit that points at nothing.
    pub fn get(&self, key: &str, assets: &AssetStore) -> Option<&CacheEntry> {
        let e = self.entries.get(key)?;
        e.assets.iter().all(|a| assets.contains(a)).then_some(e)
    }

    pub fn insert(&mut self, key: String, entry: CacheEntry) -> Result<()> {
        self.entries.insert(key, entry);
        self.save()
    }

    fn save(&self) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }
}

/// `blake3(provider ‖ model ‖ canonical_params ‖ input_hashes)`.
pub fn cache_key(
    provider: &str,
    model: &str,
    params: &serde_json::Value,
    inputs: &[Vec<u8>],
) -> String {
    let mut h = blake3::Hasher::new();
    h.update(provider.as_bytes());
    h.update(b"\0");
    h.update(model.as_bytes());
    h.update(b"\0");
    h.update(canonical_json(params).as_bytes());
    for input in inputs {
        h.update(b"\0");
        h.update(blake3::hash(input).as_bytes());
    }
    h.finalize().to_hex().to_string()
}

/// Object keys sorted, no insignificant whitespace, floats formatted identically every run —
/// so two equivalent requests hash the same regardless of how the arguments were spelled.
pub fn canonical_json(v: &serde_json::Value) -> String {
    let mut out = String::new();
    write_canonical(v, &mut out);
    out
}

fn write_canonical(v: &serde_json::Value, out: &mut String) {
    use serde_json::Value;
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => {
            // Integral floats normalize to their integer spelling: 2 and 2.0 are one request.
            match n.as_f64() {
                Some(f) if f.fract() == 0.0 && f.abs() < 9e15 => {
                    out.push_str(&format!("{}", f as i64))
                }
                Some(f) => out.push_str(&format!("{f:?}")),
                None => out.push_str(&n.to_string()),
            }
        }
        Value::String(s) => out.push_str(&serde_json::Value::String(s.clone()).to_string()),
        Value::Array(a) => {
            out.push('[');
            for (i, x) in a.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_canonical(x, out);
            }
            out.push(']');
        }
        Value::Object(m) => {
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort();
            out.push('{');
            for (i, k) in keys.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&serde_json::Value::String(k.clone()).to_string());
                out.push(':');
                write_canonical(&m[k], out);
            }
            out.push('}');
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn key_ignores_argument_order_and_integer_spelling() {
        let a = cache_key(
            "fal",
            "m",
            &json!({"prompt": "a barn", "seed": 7, "scale": 2.0}),
            &[],
        );
        let b = cache_key(
            "fal",
            "m",
            &json!({"scale": 2, "seed": 7, "prompt": "a barn"}),
            &[],
        );
        assert_eq!(a, b);
    }

    #[test]
    fn key_changes_with_provider_model_params_and_inputs() {
        let base = cache_key("fal", "m", &json!({"prompt": "a"}), &[b"img".to_vec()]);
        assert_ne!(
            base,
            cache_key("quiver", "m", &json!({"prompt": "a"}), &[b"img".to_vec()])
        );
        assert_ne!(
            base,
            cache_key("fal", "m2", &json!({"prompt": "a"}), &[b"img".to_vec()])
        );
        assert_ne!(
            base,
            cache_key("fal", "m", &json!({"prompt": "b"}), &[b"img".to_vec()])
        );
        assert_ne!(
            base,
            cache_key("fal", "m", &json!({"prompt": "a"}), &[b"other".to_vec()])
        );
    }

    #[test]
    fn a_hit_requires_the_blobs_to_still_exist() {
        let dir = tempfile::tempdir().unwrap();
        let store = AssetStore::new(dir.path());
        let asset = store.put(b"pixels", "png").unwrap();

        let mut cache = Cache::load(dir.path());
        cache
            .insert(
                "k".into(),
                CacheEntry {
                    provider: "fal".into(),
                    model: "m".into(),
                    request_id: Some("r1".into()),
                    assets: vec![asset.clone()],
                    cost_usd: 0.02,
                    at: dpaint_core::now_iso(),
                },
            )
            .unwrap();

        // Survives a reload from disk.
        let reloaded = Cache::load(dir.path());
        assert!(reloaded.get("k", &store).is_some());

        std::fs::remove_file(store.path_of(&asset)).unwrap();
        assert!(reloaded.get("k", &store).is_none());
    }
}
