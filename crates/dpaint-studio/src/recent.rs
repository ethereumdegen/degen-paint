//! The recent-projects list behind the Welcome screen.
//!
//! A shell launched with no project has nowhere project-local to keep this, so the list
//! lives beside the user's other configuration. It is advisory: a failed read or write
//! degrades the Welcome screen, it never fails opening or creating a project, which is
//! why nothing here returns an error.

use dpaint_core::now_iso;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Enough to fill the Welcome screen without turning it into a file browser.
pub const MAX: usize = 10;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub path: PathBuf,
    pub name: String,
    /// RFC-3339 timestamp of the last open.
    pub at: String,
}

/// Newest first, with entries whose directory no longer holds a project dropped.
pub fn list() -> Vec<Entry> {
    match store() {
        Some(p) => read(&p),
        None => Vec::new(),
    }
}

/// Move `root` to the front of the list.
pub fn record(root: &Path, name: &str) {
    if let Some(p) = store() {
        write(&p, push_front(read(&p), root, name));
    }
}

/// `$XDG_CONFIG_HOME`, the macOS equivalent, or `~/.config`. Also where the desktop
/// keeps `user-dirs.dirs`, which is why this is not private to the recents list.
pub fn config_dir() -> Option<PathBuf> {
    if cfg!(target_os = "macos") {
        return Some(PathBuf::from(std::env::var_os("HOME")?).join("Library/Application Support"));
    }
    match std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        Some(v) => Some(PathBuf::from(v)),
        None => Some(PathBuf::from(std::env::var_os("HOME")?).join(".config")),
    }
}

/// `$XDG_CONFIG_HOME/degen-paint/recent.json`, or the platform's equivalent.
pub fn store() -> Option<PathBuf> {
    Some(config_dir()?.join("degen-paint").join("recent.json"))
}

fn read(store: &Path) -> Vec<Entry> {
    let Ok(bytes) = std::fs::read(store) else {
        return Vec::new();
    };
    // A hand-edited or half-written file loses the list, never the session.
    let entries: Vec<Entry> = serde_json::from_slice(&bytes).unwrap_or_default();
    entries
        .into_iter()
        .filter(|e| e.path.join("project.json").is_file())
        .take(MAX)
        .collect()
}

fn write(store: &Path, entries: Vec<Entry>) {
    let Some(dir) = store.parent() else { return };
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    if let Ok(text) = serde_json::to_string_pretty(&entries) {
        let _ = std::fs::write(store, text);
    }
}

fn push_front(mut entries: Vec<Entry>, root: &Path, name: &str) -> Vec<Entry> {
    // Canonicalize so the same project reached through a relative path and an absolute
    // one is one entry rather than two.
    let path = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    entries.retain(|e| e.path != path);
    entries.insert(
        0,
        Entry {
            path,
            name: name.to_string(),
            at: now_iso(),
        },
    );
    entries.truncate(MAX);
    entries
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(dir: &Path, name: &str) -> PathBuf {
        let root = dir.join(format!("{name}.dpaint"));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("project.json"), "{}").unwrap();
        root
    }

    #[test]
    fn the_list_is_newest_first_capped_and_free_of_deleted_projects() {
        let tmp = tempfile::tempdir().unwrap();
        let store = tmp.path().join("recent.json");

        let mut roots = Vec::new();
        for i in 0..(MAX + 2) {
            let root = project(tmp.path(), &format!("p{i}"));
            write(&store, push_front(read(&store), &root, &format!("p{i}")));
            roots.push(root);
        }

        let list = read(&store);
        assert_eq!(list.len(), MAX, "the list is capped");
        assert_eq!(list[0].name, format!("p{}", MAX + 1), "newest first");

        // Re-opening an older project moves it to the front instead of duplicating it.
        write(&store, push_front(read(&store), &roots[5], "p5"));
        let list = read(&store);
        assert_eq!(list[0].name, "p5");
        assert_eq!(list.iter().filter(|e| e.name == "p5").count(), 1);

        std::fs::remove_file(roots[5].join("project.json")).unwrap();
        assert!(
            !read(&store).iter().any(|e| e.name == "p5"),
            "a project that no longer exists must not be offered"
        );
    }
}
