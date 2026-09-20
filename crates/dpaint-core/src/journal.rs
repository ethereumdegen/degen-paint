//! Append-only op journal with RFC-6902 patches.
//!
//! Undo is the inverse patch, so correct undo costs one mechanism instead of a hand-written
//! `undo()` for every op in the catalog. The journal is also the audit trail and the replay
//! format: a golden test fixture is a journal, not a mystery binary.

use crate::error::Result;
use crate::ids::DocId;
use crate::op::OpEffect;
use crate::project::Project;
use crate::vfs::{FsVfs, Vfs};
use json_patch::Patch;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    pub seq: u64,
    pub ts: String,
    #[serde(default)]
    pub actor: Actor,
    pub op: String,
    pub args: serde_json::Value,
    /// before -> after
    pub patch: Patch,
    /// after -> before; applying this is undo.
    pub inverse: Patch,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effect: Option<OpEffect>,
    /// Undone entries stay in the file; redo re-applies them.
    #[serde(default)]
    pub undone: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Actor {
    #[default]
    Agent,
    Human,
    Replay,
}

pub struct Journal {
    path: PathBuf,
    vfs: Arc<dyn Vfs>,
    entries: Vec<Entry>,
    loaded: bool,
}

impl std::fmt::Debug for Journal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Journal")
            .field("path", &self.path)
            .field("entries", &self.entries.len())
            .field("loaded", &self.loaded)
            .finish()
    }
}

impl Journal {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self::with_vfs(path, FsVfs::shared())
    }

    pub fn with_vfs(path: impl Into<PathBuf>, vfs: Arc<dyn Vfs>) -> Self {
        Self {
            path: path.into(),
            vfs,
            entries: Vec::new(),
            loaded: false,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&mut self) -> Result<&[Entry]> {
        if !self.loaded {
            self.entries.clear();
            if self.vfs.exists(&self.path) {
                let bytes = self.vfs.read(&self.path)?;
                for line in String::from_utf8_lossy(&bytes).lines() {
                    if line.trim().is_empty() {
                        continue;
                    }
                    self.entries.push(serde_json::from_str(line)?);
                }
            }
            self.loaded = true;
        }
        Ok(&self.entries)
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Record an applied op. `before`/`after` are the serialized project on either side.
    pub fn record(
        &mut self,
        op: &str,
        args: serde_json::Value,
        before: &serde_json::Value,
        after: &serde_json::Value,
        effect: Option<OpEffect>,
        actor: Actor,
    ) -> Result<u64> {
        self.load()?;
        let seq = self.entries.last().map(|e| e.seq + 1).unwrap_or(1);
        let entry = Entry {
            seq,
            ts: crate::project::now_iso(),
            actor,
            op: op.to_string(),
            args,
            patch: json_patch::diff(before, after),
            inverse: json_patch::diff(after, before),
            effect,
            undone: false,
        };
        self.append(&entry)?;
        self.entries.push(entry);
        Ok(seq)
    }

    fn append(&self, e: &Entry) -> Result<()> {
        let mut line = serde_json::to_string(e)?;
        line.push('\n');
        self.vfs.append(&self.path, line.as_bytes())
    }

    /// Rewrite the file after undo/redo flips an entry's state. `Vfs::write` is atomic where
    /// the backend allows, so a crash mid-flip cannot shred the history.
    fn rewrite(&self) -> Result<()> {
        let mut buf = String::new();
        for e in &self.entries {
            buf.push_str(&serde_json::to_string(e)?);
            buf.push('\n');
        }
        self.vfs.write(&self.path, buf.as_bytes())
    }

    /// Undo the newest applied entry. Returns the op id that was undone.
    pub fn undo(&mut self, project: &mut Project) -> Result<Option<String>> {
        self.load()?;
        let Some(idx) = self.entries.iter().rposition(|e| !e.undone) else {
            return Ok(None);
        };
        let mut value = serde_json::to_value(&*project)?;
        json_patch::patch(&mut value, &self.entries[idx].inverse)
            .map_err(|e| crate::error::Error::Invalid(format!("undo patch failed: {e}")))?;
        *project = serde_json::from_value(value)?;
        self.entries[idx].undone = true;
        self.rewrite()?;
        Ok(Some(self.entries[idx].op.clone()))
    }

    /// Redo the oldest undone entry.
    pub fn redo(&mut self, project: &mut Project) -> Result<Option<String>> {
        self.load()?;
        let Some(idx) = self.entries.iter().position(|e| e.undone) else {
            return Ok(None);
        };
        let mut value = serde_json::to_value(&*project)?;
        json_patch::patch(&mut value, &self.entries[idx].patch)
            .map_err(|e| crate::error::Error::Invalid(format!("redo patch failed: {e}")))?;
        *project = serde_json::from_value(value)?;
        self.entries[idx].undone = false;
        self.rewrite()?;
        Ok(Some(self.entries[idx].op.clone()))
    }

    /// Documents touched by the last `n` applied entries — what an agent polls to see
    /// what a human just changed in the GUI.
    pub fn recently_changed(&self, n: usize) -> Vec<DocId> {
        let mut out = Vec::new();
        for e in self.entries.iter().rev().filter(|e| !e.undone).take(n) {
            if let Some(eff) = &e.effect {
                for d in &eff.changed {
                    if !out.contains(d) {
                        out.push(d.clone());
                    }
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::{Document, RasterDoc};
    use crate::ids::DocId;

    fn project() -> Project {
        Project::new(
            "t",
            Document::Raster(RasterDoc::new(DocId::from("doc_main"), "main", 10, 10)),
        )
    }

    fn mutate(p: &mut Project, dpi: f32) -> (serde_json::Value, serde_json::Value) {
        let before = serde_json::to_value(&*p).unwrap();
        p.raster_mut(&DocId::from("doc_main")).unwrap().dpi = dpi;
        let after = serde_json::to_value(&*p).unwrap();
        (before, after)
    }

    #[test]
    fn undo_and_redo_restore_exact_json_at_every_step() {
        let tmp = tempfile::tempdir().unwrap();
        let mut j = Journal::new(tmp.path().join("history.jsonl"));
        let mut p = project();
        let states = vec![serde_json::to_value(&p).unwrap()];

        let mut states = states;
        for dpi in [150.0f32, 300.0, 600.0] {
            let (before, after) = mutate(&mut p, dpi);
            j.record(
                "raster.canvas.set-dpi",
                serde_json::json!({ "dpi": dpi }),
                &before,
                &after,
                None,
                Actor::Agent,
            )
            .unwrap();
            states.push(after);
        }

        for expect in states.iter().rev().skip(1) {
            j.undo(&mut p).unwrap();
            assert_eq!(&serde_json::to_value(&p).unwrap(), expect);
        }
        assert!(
            j.undo(&mut p).unwrap().is_none(),
            "undo past the start must be a no-op"
        );

        for expect in states.iter().skip(1) {
            j.redo(&mut p).unwrap();
            assert_eq!(&serde_json::to_value(&p).unwrap(), expect);
        }
        assert!(j.redo(&mut p).unwrap().is_none());
    }

    #[test]
    fn the_journal_survives_a_reload_with_its_undo_state() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("history.jsonl");
        let mut p = project();
        {
            let mut j = Journal::new(&path);
            let (b, a) = mutate(&mut p, 300.0);
            j.record(
                "raster.canvas.set-dpi",
                serde_json::json!({}),
                &b,
                &a,
                None,
                Actor::Agent,
            )
            .unwrap();
            j.undo(&mut p).unwrap();
        }
        let mut j2 = Journal::new(&path);
        let entries = j2.load().unwrap().to_vec();
        assert_eq!(entries.len(), 1);
        assert!(
            entries[0].undone,
            "undone state must persist across processes"
        );
        assert_eq!(
            j2.redo(&mut p).unwrap().as_deref(),
            Some("raster.canvas.set-dpi")
        );
        assert_eq!(p.raster(&DocId::from("doc_main")).unwrap().dpi, 300.0);
    }
}
