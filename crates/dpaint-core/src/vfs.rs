//! The one place in the engine that touches storage.
//!
//! Everything above this module — the asset store, the project file, the journal — speaks
//! [`Vfs`] and never `std::fs`. That is what lets the identical engine run against a real
//! directory in the CLI and the Tauri app, and against an in-memory tree in a browser tab
//! where there is no filesystem at all. A `std::fs` call anywhere else in `dpaint-core`
//! compiles for `wasm32` and then fails at run time, which is the worst kind of bug; the
//! [`tests::no_direct_filesystem_calls_outside_this_module`] guard makes that a build
//! failure instead.

use crate::error::Result;
use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, RwLock};

/// Byte storage, addressed by path. Implementations are shared across threads and cloned
/// freely, so every method takes `&self`.
pub trait Vfs: Send + Sync {
    fn read(&self, path: &Path) -> Result<Vec<u8>>;
    /// Replace the contents of `path`, atomically where the backend allows it. Missing
    /// parent directories are created.
    fn write(&self, path: &Path, bytes: &[u8]) -> Result<()>;
    fn exists(&self, path: &Path) -> bool;
    fn remove(&self, path: &Path) -> Result<()>;
    /// Immediate children of `dir`, as full paths, sorted. Errors if `dir` is not a directory.
    fn list(&self, dir: &Path) -> Result<Vec<PathBuf>>;
    fn create_dir_all(&self, path: &Path) -> Result<()>;
    /// Append to `path`, creating it if absent. This is the journal's hot path.
    fn append(&self, path: &Path, bytes: &[u8]) -> Result<()>;

    /// Size in bytes. Overridden by backends that can answer without reading the blob.
    fn size(&self, path: &Path) -> Result<u64> {
        Ok(self.read(path)?.len() as u64)
    }
}

/// The native backend. Writes go through a sibling `.tmp` file and a rename, so a crash can
/// never leave a half-written `project.json` or a truncated asset.
#[derive(Debug, Clone, Copy, Default)]
pub struct FsVfs;

impl FsVfs {
    pub fn new() -> Self {
        Self
    }

    /// An `Arc<dyn Vfs>` over the native filesystem — the default for every constructor that
    /// does not take one explicitly.
    pub fn shared() -> Arc<dyn Vfs> {
        Arc::new(Self)
    }

    /// Walk upwards from `start` looking for a project. Directory traversal is a filesystem
    /// concept, so it lives with the filesystem backend rather than in `project.rs`.
    ///
    /// A directory containing `project.json` is the project; otherwise the lexicographically
    /// first `*.dpaint` child that contains one counts.
    pub fn discover_project_root(start: &Path) -> Result<PathBuf> {
        let mut cur = std::fs::canonicalize(start)?;
        loop {
            if cur.join("project.json").exists() {
                return Ok(cur);
            }
            if let Ok(entries) = std::fs::read_dir(&cur) {
                let mut candidates: Vec<PathBuf> = entries
                    .flatten()
                    .map(|e| e.path())
                    .filter(|p| {
                        p.extension().and_then(|e| e.to_str()) == Some("dpaint")
                            && p.join("project.json").exists()
                    })
                    .collect();
                candidates.sort();
                if let Some(p) = candidates.into_iter().next() {
                    return Ok(p);
                }
            }
            if !cur.pop() {
                return Err(crate::error::Error::Invalid(
                    "no degen-paint project found in this directory or any parent".into(),
                ));
            }
        }
    }
}

fn tmp_sibling(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_os_string();
    s.push(".tmp");
    PathBuf::from(s)
}

fn ensure_parent(path: &Path) -> Result<()> {
    if let Some(p) = path.parent() {
        if !p.as_os_str().is_empty() {
            std::fs::create_dir_all(p)?;
        }
    }
    Ok(())
}

impl Vfs for FsVfs {
    fn read(&self, path: &Path) -> Result<Vec<u8>> {
        Ok(std::fs::read(path)?)
    }

    fn write(&self, path: &Path, bytes: &[u8]) -> Result<()> {
        ensure_parent(path)?;
        let tmp = tmp_sibling(path);
        std::fs::write(&tmp, bytes)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    fn exists(&self, path: &Path) -> bool {
        path.exists()
    }

    fn remove(&self, path: &Path) -> Result<()> {
        Ok(std::fs::remove_file(path)?)
    }

    fn list(&self, dir: &Path) -> Result<Vec<PathBuf>> {
        let mut out = Vec::new();
        for e in std::fs::read_dir(dir)? {
            out.push(e?.path());
        }
        out.sort();
        Ok(out)
    }

    fn create_dir_all(&self, path: &Path) -> Result<()> {
        Ok(std::fs::create_dir_all(path)?)
    }

    fn append(&self, path: &Path, bytes: &[u8]) -> Result<()> {
        use std::io::Write;
        ensure_parent(path)?;
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
        f.write_all(bytes)?;
        Ok(())
    }

    fn size(&self, path: &Path) -> Result<u64> {
        Ok(std::fs::metadata(path)?.len())
    }
}

/// An in-memory tree. This is the browser backend — the page persists the bytes to OPFS or
/// IndexedDB and hands them back on reload — and it is also what makes the core tests run
/// without a `tempfile` round trip.
#[derive(Debug, Clone, Default)]
pub struct MemVfs {
    inner: Arc<RwLock<Tree>>,
}

#[derive(Debug, Default)]
struct Tree {
    files: BTreeMap<PathBuf, Vec<u8>>,
    dirs: std::collections::BTreeSet<PathBuf>,
}

/// Lexical normalisation: `.` is dropped, `..` pops. There are no symlinks in memory, so
/// this is exact rather than a best guess.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn missing(path: &Path) -> crate::error::Error {
    crate::error::Error::Io(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        format!("{} not found", path.display()),
    ))
}

impl MemVfs {
    pub fn new() -> Self {
        Self::default()
    }

    /// A fresh in-memory filesystem as an `Arc<dyn Vfs>`.
    pub fn shared() -> Arc<dyn Vfs> {
        Arc::new(Self::new())
    }

    /// Every file currently held, as `(path, bytes)` sorted by path. The browser shell uses
    /// this to flush the whole tree to OPFS, and tests use it to assert on the exact shape.
    pub fn snapshot(&self) -> Vec<(PathBuf, Vec<u8>)> {
        let g = self.inner.read().expect("mem vfs poisoned");
        g.files.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
    }

    /// Total bytes held, for the browser shell's storage budget reporting.
    pub fn bytes(&self) -> u64 {
        let g = self.inner.read().expect("mem vfs poisoned");
        g.files.values().map(|v| v.len() as u64).sum()
    }

    fn touch_dirs(tree: &mut Tree, path: &Path) {
        let mut cur = path.to_path_buf();
        while cur.pop() && !cur.as_os_str().is_empty() {
            tree.dirs.insert(cur.clone());
        }
    }
}

impl Vfs for MemVfs {
    fn read(&self, path: &Path) -> Result<Vec<u8>> {
        let p = normalize(path);
        let g = self.inner.read().expect("mem vfs poisoned");
        g.files.get(&p).cloned().ok_or_else(|| missing(path))
    }

    fn write(&self, path: &Path, bytes: &[u8]) -> Result<()> {
        let p = normalize(path);
        let mut g = self.inner.write().expect("mem vfs poisoned");
        Self::touch_dirs(&mut g, &p);
        g.files.insert(p, bytes.to_vec());
        Ok(())
    }

    fn exists(&self, path: &Path) -> bool {
        let p = normalize(path);
        let g = self.inner.read().expect("mem vfs poisoned");
        g.files.contains_key(&p) || g.dirs.contains(&p)
    }

    fn remove(&self, path: &Path) -> Result<()> {
        let p = normalize(path);
        let mut g = self.inner.write().expect("mem vfs poisoned");
        g.files.remove(&p).map(|_| ()).ok_or_else(|| missing(path))
    }

    fn list(&self, dir: &Path) -> Result<Vec<PathBuf>> {
        let d = normalize(dir);
        let g = self.inner.read().expect("mem vfs poisoned");
        if !g.dirs.contains(&d) {
            return Err(if g.files.contains_key(&d) {
                crate::error::Error::Io(std::io::Error::new(
                    // `ErrorKind::NotADirectory` is newer than this crate's MSRV.
                    std::io::ErrorKind::InvalidInput,
                    format!("{} is a file", dir.display()),
                ))
            } else {
                missing(dir)
            });
        }
        let mut out: std::collections::BTreeSet<PathBuf> = std::collections::BTreeSet::new();
        for k in g.files.keys().chain(g.dirs.iter()) {
            if let Ok(rel) = k.strip_prefix(&d) {
                if let Some(first) = rel.components().next() {
                    out.insert(d.join(first.as_os_str()));
                }
            }
        }
        Ok(out.into_iter().collect())
    }

    fn create_dir_all(&self, path: &Path) -> Result<()> {
        let p = normalize(path);
        let mut g = self.inner.write().expect("mem vfs poisoned");
        let mut cur = PathBuf::new();
        for c in p.components() {
            cur.push(c.as_os_str());
            if !cur.as_os_str().is_empty() {
                g.dirs.insert(cur.clone());
            }
        }
        Ok(())
    }

    fn append(&self, path: &Path, bytes: &[u8]) -> Result<()> {
        let p = normalize(path);
        let mut g = self.inner.write().expect("mem vfs poisoned");
        Self::touch_dirs(&mut g, &p);
        g.files.entry(p).or_default().extend_from_slice(bytes);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Source lines that reach `std::fs` directly, either spelled out or through a
    /// `use std::fs;` alias. Anything at or after the `#[cfg(test)]` marker is test
    /// scaffolding and never ships in a wasm build.
    fn direct_fs_calls(src: &str) -> Vec<(usize, String)> {
        // `fs::` only counts when it starts a path segment — `vfs::` and `FsVfs::` do not.
        fn aliased_fs_path(code: &str) -> bool {
            let b = code.as_bytes();
            code.match_indices("fs::").any(|(i, _)| {
                i == 0 || !(b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_' || b[i - 1] == b':')
            })
        }
        let mut out = Vec::new();
        for (i, line) in src.lines().enumerate() {
            if line.trim_start().starts_with("#[cfg(test)]") {
                break;
            }
            let code = line.split("//").next().unwrap_or("");
            if code.contains("std::fs") || aliased_fs_path(code) {
                out.push((i + 1, line.trim().to_string()));
            }
        }
        out
    }

    #[test]
    fn the_filesystem_guard_actually_detects_a_reintroduced_call() {
        let bad = "fn save(p: &Path) {\n    std::fs::write(p, b\"x\").unwrap();\n}\n";
        assert_eq!(direct_fs_calls(bad).len(), 1, "a direct std::fs call must be caught");

        let aliased = "use std::fs;\nfn load() { let _ = fs::read(\"a\"); }\n";
        assert_eq!(direct_fs_calls(aliased).len(), 2, "`use std::fs` and `fs::read` are both hits");

        let in_tests = "fn ok() {}\n#[cfg(test)]\nmod tests {\n    std::fs::read(\"x\");\n}\n";
        assert!(direct_fs_calls(in_tests).is_empty(), "test code may use the real filesystem");

        let commented = "// std::fs::read is what this replaces\nfn ok() {}\n";
        assert!(direct_fs_calls(commented).is_empty(), "a doc reference is not a call");
    }

    /// The hard requirement of the WASM build: the only route from engine code to the host
    /// filesystem is `FsVfs`, which a browser build never constructs.
    #[test]
    fn no_direct_filesystem_calls_outside_this_module() {
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut offenders = Vec::new();
        let mut stack = vec![src.clone()];
        let mut scanned = 0usize;
        while let Some(dir) = stack.pop() {
            for e in std::fs::read_dir(&dir).unwrap() {
                let p = e.unwrap().path();
                if p.is_dir() {
                    stack.push(p);
                    continue;
                }
                if p.extension().and_then(|x| x.to_str()) != Some("rs") {
                    continue;
                }
                if p.file_name().and_then(|x| x.to_str()) == Some("vfs.rs") {
                    continue;
                }
                scanned += 1;
                let text = std::fs::read_to_string(&p).unwrap();
                for (line, code) in direct_fs_calls(&text) {
                    offenders.push(format!("{}:{line}: {code}", p.display()));
                }
            }
        }
        assert!(scanned > 5, "the scanner found almost no sources: {scanned}");
        assert!(
            offenders.is_empty(),
            "dpaint-core must reach storage only through vfs::Vfs, but found:\n{}",
            offenders.join("\n")
        );
    }

    #[test]
    fn a_memory_tree_round_trips_files_directories_and_appends() {
        let vfs = MemVfs::new();
        let root = Path::new("/p");
        vfs.create_dir_all(&root.join("assets/ab")).unwrap();
        assert!(vfs.exists(&root.join("assets")));
        assert!(!vfs.exists(&root.join("assets/cd")));

        vfs.write(&root.join("project.json"), b"{}").unwrap();
        assert_eq!(vfs.read(&root.join("project.json")).unwrap(), b"{}");
        assert_eq!(vfs.size(&root.join("project.json")).unwrap(), 2);

        vfs.append(&root.join("history.jsonl"), b"one\n").unwrap();
        vfs.append(&root.join("history.jsonl"), b"two\n").unwrap();
        assert_eq!(vfs.read(&root.join("history.jsonl")).unwrap(), b"one\ntwo\n");

        vfs.write(&root.join("assets/ab/x.png"), b"px").unwrap();
        assert_eq!(vfs.list(&root.join("assets")).unwrap(), vec![root.join("assets/ab")]);
        assert_eq!(vfs.list(&root.join("assets/ab")).unwrap(), vec![root.join("assets/ab/x.png")]);

        vfs.remove(&root.join("assets/ab/x.png")).unwrap();
        assert!(!vfs.exists(&root.join("assets/ab/x.png")));
        assert_eq!(vfs.remove(&root.join("nope")).unwrap_err().code(), "io_error");
        assert!(vfs.list(&root.join("project.json")).is_err(), "a file is not a directory");
    }

    #[test]
    fn the_two_backends_agree_on_the_operations_the_engine_uses() {
        let tmp = tempfile::tempdir().unwrap();
        let mem = MemVfs::new();
        let backends: Vec<(&str, PathBuf, Arc<dyn Vfs>)> = vec![
            ("fs", tmp.path().to_path_buf(), Arc::new(FsVfs)),
            ("mem", PathBuf::from("/p"), Arc::new(mem)),
        ];
        for (name, root, vfs) in backends {
            vfs.create_dir_all(&root.join("assets")).unwrap();
            assert!(vfs.exists(&root.join("assets")), "{name}: created dir must exist");
            assert!(!vfs.exists(&root.join("assets/none.png")), "{name}");

            // write is a replace, not an append, and is visible immediately.
            vfs.write(&root.join("assets/a.bin"), b"first").unwrap();
            vfs.write(&root.join("assets/a.bin"), b"second").unwrap();
            assert_eq!(vfs.read(&root.join("assets/a.bin")).unwrap(), b"second", "{name}");

            // write creates missing parents, which is what the sharded asset store relies on.
            vfs.write(&root.join("assets/de/ep/b.bin"), b"x").unwrap();
            assert_eq!(vfs.read(&root.join("assets/de/ep/b.bin")).unwrap(), b"x", "{name}");

            assert_eq!(vfs.size(&root.join("assets/a.bin")).unwrap(), 6, "{name}");
            assert_eq!(vfs.read(&root.join("missing")).unwrap_err().code(), "io_error", "{name}");

            let listed = vfs.list(&root.join("assets")).unwrap();
            assert_eq!(listed, vec![root.join("assets/a.bin"), root.join("assets/de")], "{name}");
        }
    }

    #[test]
    fn a_memory_tree_is_shared_by_every_clone() {
        let a = MemVfs::new();
        let b = a.clone();
        a.write(Path::new("/x"), b"1").unwrap();
        assert_eq!(b.read(Path::new("/x")).unwrap(), b"1");
        assert_eq!(b.snapshot(), vec![(PathBuf::from("/x"), b"1".to_vec())]);
        assert_eq!(b.bytes(), 1);
    }
}
