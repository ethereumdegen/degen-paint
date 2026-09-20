//! Content-addressed asset store.
//!
//! Every blob — imported photo, baked pixel layer, font, AI generation, cached cross-mode
//! render — is stored under its blake3 hash. Consequences: `project.json` never contains
//! pixels, undo snapshots copy JSON rather than images, identical imports deduplicate for
//! free, and an identical AI request is served from disk instead of re-billing.

use crate::error::{Error, Result};
use crate::vfs::{FsVfs, Vfs};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// `blake3:<64 hex>.<ext>` — the hash identifies the bytes, the extension records the format.
#[derive(
    Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, schemars::JsonSchema,
)]
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
        PathBuf::from("assets")
            .join(shard)
            .join(format!("{h}.{}", self.ext()))
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

#[derive(Clone)]
pub struct AssetStore {
    root: PathBuf,
    vfs: Arc<dyn Vfs>,
}

impl std::fmt::Debug for AssetStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AssetStore")
            .field("root", &self.root)
            .finish_non_exhaustive()
    }
}

impl AssetStore {
    /// `root` is the project directory; assets live in `root/assets`. Backed by the host
    /// filesystem — the browser build goes through [`AssetStore::with_vfs`] instead.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self::with_vfs(root, FsVfs::shared())
    }

    pub fn with_vfs(root: impl Into<PathBuf>, vfs: Arc<dyn Vfs>) -> Self {
        Self {
            root: root.into(),
            vfs,
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn vfs(&self) -> &Arc<dyn Vfs> {
        &self.vfs
    }

    pub fn path_of(&self, r: &AssetRef) -> PathBuf {
        self.root.join(r.rel_path())
    }

    pub fn contains(&self, r: &AssetRef) -> bool {
        self.vfs.exists(&self.path_of(r))
    }

    /// Write bytes, returning their reference. Writing identical bytes twice is a no-op,
    /// which is what makes replay and undo/redo free.
    pub fn put(&self, bytes: &[u8], ext: &str) -> Result<AssetRef> {
        let hash = blake3::hash(bytes).to_hex().to_string();
        let r = AssetRef::new(&hash, ext);
        let path = self.path_of(&r);
        if self.vfs.exists(&path) {
            return Ok(r);
        }
        self.vfs.write(&path, bytes)?;
        Ok(r)
    }

    pub fn put_file(&self, src: impl AsRef<Path>) -> Result<AssetRef> {
        let src = src.as_ref();
        let ext = src
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("bin")
            .to_ascii_lowercase();
        let bytes = self.vfs.read(src)?;
        self.put(&bytes, &ext)
    }

    pub fn get(&self, r: &AssetRef) -> Result<Vec<u8>> {
        let path = self.path_of(r);
        if !self.vfs.exists(&path) {
            return Err(Error::AssetMissing(r.0.clone()));
        }
        self.vfs.read(&path)
    }

    pub fn size_of(&self, r: &AssetRef) -> Result<u64> {
        self.vfs.size(&self.path_of(r))
    }

    /// Every blob currently in the store, for garbage collection and project statistics.
    pub fn list(&self) -> Result<Vec<AssetRef>> {
        let dir = self.root.join("assets");
        let mut out = Vec::new();
        if !self.vfs.exists(&dir) {
            return Ok(out);
        }
        for shard in self.vfs.list(&dir)? {
            // Only the two-hex shard directories hold blobs; anything else is not ours.
            let Ok(files) = self.vfs.list(&shard) else {
                continue;
            };
            for f in files {
                let name = f
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string();
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
                self.vfs.remove(&self.path_of(&r))?;
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
        let parts: Vec<_> = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().to_string())
            .collect();
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

    /// The browser build runs this exact store against [`MemVfs`]. Deduplication, sharding
    /// and gc are properties of the store, not of the filesystem, and must hold identically.
    #[test]
    fn the_store_deduplicates_and_collects_garbage_entirely_in_memory() {
        let store = AssetStore::with_vfs("/mem.dpaint", crate::vfs::MemVfs::shared());

        let a = store.put(b"pixels", "png").unwrap();
        let again = store.put(b"pixels", "png").unwrap();
        assert_eq!(a, again, "identical bytes must be one blob");
        assert_eq!(store.list().unwrap(), vec![a.clone()]);

        let b = store.put(b"other pixels", "png").unwrap();
        let font = store.put(b"ttf bytes", "ttf").unwrap();
        assert_ne!(a, b);
        assert_eq!(store.get(&b).unwrap(), b"other pixels");
        assert_eq!(store.size_of(&font).unwrap(), 9);
        assert_eq!(store.list().unwrap().len(), 3);

        // Sharding survives the backend swap: two hex chars, then the blob.
        let shard = store.path_of(&a).parent().unwrap().to_path_buf();
        assert!(shard.ends_with(&a.hash()[..2]));

        let (removed, freed) = store
            .gc(&std::collections::BTreeSet::from([a.clone()]))
            .unwrap();
        assert_eq!(removed.len(), 2, "both unreachable blobs go");
        assert_eq!(freed, 12 + 9);
        assert_eq!(store.list().unwrap(), vec![a.clone()]);
        assert!(store.contains(&a));
        assert_eq!(store.get(&b).unwrap_err().code(), "asset_missing");
    }

    #[test]
    fn two_stores_over_one_memory_tree_see_each_others_blobs() {
        let vfs = crate::vfs::MemVfs::new();
        let writer = AssetStore::with_vfs("/p", std::sync::Arc::new(vfs.clone()));
        let reader = AssetStore::with_vfs("/p", std::sync::Arc::new(vfs));
        let r = writer.put(b"shared", "png").unwrap();
        assert_eq!(reader.get(&r).unwrap(), b"shared");
    }
}
