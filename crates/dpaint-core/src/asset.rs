//! Content-addressed asset store.
//!
//! Every blob — imported photo, baked pixel layer, font, AI generation, cached cross-mode
//! render — is stored under its blake3 hash. Consequences: `project.json` never contains
//! pixels, undo snapshots copy JSON rather than images, identical imports deduplicate for
//! free, and an identical AI request is served from disk instead of re-billing.

use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// `blake3:<64 hex>.<ext>` — the hash identifies the bytes, the extension records the format.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(transparent)]
pub struct AssetRef(pub String);

impl AssetRef {
    pub fn new(hash: &str, ext: &str) -> Self {
        AssetRef(format!("blake3:{hash}.{ext}"))
    }

    pub fn hash(&self) -> &str {
        self.0
            .trim_start_matches("blake3:")
            .split('.')
            .next()
            .unwrap_or_default()
    }

    pub fn ext(&self) -> &str {
        self.0.rsplit('.').next().unwrap_or("bin")
    }

    /// `assets/3f/3f9a….png` — sharded so a directory never holds a hundred thousand files.
    pub fn rel_path(&self) -> PathBuf {
        let h = self.hash();
        let shard = &h[..2.min(h.len())];
        PathBuf::from("assets").join(shard).join(format!("{h}.{}", self.ext()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for AssetRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone)]
pub struct AssetStore {
    root: PathBuf,
}

impl AssetStore {
    /// `root` is the project directory; assets live in `root/assets`.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn path_of(&self, r: &AssetRef) -> PathBuf {
        self.root.join(r.rel_path())
    }

    pub fn contains(&self, r: &AssetRef) -> bool {
        self.path_of(r).exists()
    }

    /// Write bytes, returning their reference. Writing identical bytes twice is a no-op,
    /// which is what makes replay and undo/redo free.
    pub fn put(&self, bytes: &[u8], ext: &str) -> Result<AssetRef> {
        let hash = blake3::hash(bytes).to_hex().to_string();
        let r = AssetRef::new(&hash, ext);
        let path = self.path_of(&r);
        if path.exists() {
            return Ok(r);
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension(format!("{ext}.tmp"));
        std::fs::write(&tmp, bytes)?;
        std::fs::rename(&tmp, &path)?;
        Ok(r)
    }

    pub fn put_file(&self, src: impl AsRef<Path>) -> Result<AssetRef> {
        let src = src.as_ref();
        let ext = src
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("bin")
            .to_ascii_lowercase();
        let bytes = std::fs::read(src)?;
        self.put(&bytes, &ext)
    }

    pub fn get(&self, r: &AssetRef) -> Result<Vec<u8>> {
        let path = self.path_of(r);
        if !path.exists() {
            return Err(Error::AssetMissing(r.0.clone()));
        }
        Ok(std::fs::read(path)?)
    }

    pub fn size_of(&self, r: &AssetRef) -> Result<u64> {
        Ok(std::fs::metadata(self.path_of(r))?.len())
    }

    /// Every blob currently on disk, for garbage collection and project statistics.
    pub fn list(&self) -> Result<Vec<AssetRef>> {
        let dir = self.root.join("assets");
        let mut out = Vec::new();
        if !dir.exists() {
            return Ok(out);
        }
        for shard in std::fs::read_dir(&dir)? {
            let shard = shard?;
            if !shard.file_type()?.is_dir() {
                continue;
            }
            for f in std::fs::read_dir(shard.path())? {
                let f = f?;
                let name = f.file_name().to_string_lossy().to_string();
                if name.ends_with(".tmp") {
                    continue;
                }
                if let Some((h, ext)) = name.rsplit_once('.') {
                    out.push(AssetRef::new(h, ext));
                }
            }
        }
        out.sort();
        Ok(out)
    }

    /// Delete blobs not in `keep`. Returns what was removed and how many bytes came back.
    pub fn gc(&self, keep: &std::collections::BTreeSet<AssetRef>) -> Result<(Vec<AssetRef>, u64)> {
        let mut removed = Vec::new();
        let mut freed = 0u64;
        for r in self.list()? {
            if !keep.contains(&r) {
                freed += self.size_of(&r).unwrap_or(0);
                std::fs::remove_file(self.path_of(&r))?;
                removed.push(r);
            }
        }
        Ok((removed, freed))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_bytes_deduplicate_to_one_blob() {
        let tmp = tempfile::tempdir().unwrap();
        let store = AssetStore::new(tmp.path());
        let a = store.put(b"pixels", "png").unwrap();
        let b = store.put(b"pixels", "png").unwrap();
        assert_eq!(a, b);
        assert_eq!(store.list().unwrap().len(), 1);
        assert_eq!(store.get(&a).unwrap(), b"pixels");
    }

    #[test]
    fn different_bytes_get_different_refs_and_shard_by_prefix() {
        let tmp = tempfile::tempdir().unwrap();
        let store = AssetStore::new(tmp.path());
        let a = store.put(b"one", "png").unwrap();
        let b = store.put(b"two", "png").unwrap();
        assert_ne!(a, b);
        let rel = a.rel_path();
        let parts: Vec<_> = rel.components().map(|c| c.as_os_str().to_string_lossy().to_string()).collect();
        assert_eq!(parts[0], "assets");
        assert_eq!(parts[1], a.hash()[..2].to_string());
    }

    #[test]
    fn gc_removes_only_unreachable_blobs() {
        let tmp = tempfile::tempdir().unwrap();
        let store = AssetStore::new(tmp.path());
        let keep = store.put(b"keep", "png").unwrap();
        let _drop = store.put(b"drop", "png").unwrap();
        let set = std::collections::BTreeSet::from([keep.clone()]);
        let (removed, freed) = store.gc(&set).unwrap();
        assert_eq!(removed.len(), 1);
        assert_eq!(freed, 4);
        assert!(store.contains(&keep));
        assert_eq!(store.list().unwrap(), vec![keep]);
    }

    #[test]
    fn missing_assets_report_a_machine_readable_code() {
        let tmp = tempfile::tempdir().unwrap();
        let store = AssetStore::new(tmp.path());
        let err = store.get(&AssetRef::new("deadbeef", "png")).unwrap_err();
        assert_eq!(err.code(), "asset_missing");
    }
}
