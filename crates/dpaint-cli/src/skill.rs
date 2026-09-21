//! Putting the pack contribution on disk.
//!
//! The bytes themselves come from [`dpaint_studio::skill`], which is also what
//! `GET /api/v1/skill` serves — one generator, so a vendored pack and a live probe cannot
//! describe different apps. All that is left here is rendering values to files.

use dpaint_core::{Error, Result};
use serde_json::Value;
use std::path::Path;

/// Each file as its path and its text: Markdown verbatim, JSON pretty-printed with a trailing
/// newline so the output is diffable once starkbot-neo vendors it into a git tree.
pub fn files(pack: &Value) -> Result<Vec<(String, String)>> {
    let map = pack
        .get("files")
        .and_then(Value::as_object)
        .ok_or_else(|| Error::Invalid("the skill pack has no files".into()))?;
    let mut out = Vec::with_capacity(map.len());
    for (path, value) in map {
        let text = match value {
            Value::String(s) => s.clone(),
            other => format!("{}\n", serde_json::to_string_pretty(other)?),
        };
        out.push((path.clone(), text));
    }
    Ok(out)
}

pub fn write(dir: &Path, pack: &Value) -> Result<Vec<String>> {
    let mut written = Vec::new();
    for (rel, text) in files(pack)? {
        let path = dir.join(&rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, text)?;
        written.push(path.display().to_string());
    }
    written.sort();
    Ok(written)
}
