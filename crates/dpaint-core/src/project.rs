//! The project: a set of documents of mixed kind, their assets, and their history.

use crate::asset::{AssetRef, AssetStore};
use crate::color::Color;
use crate::doc::{Document, ModelDoc, RasterDoc, VectorDoc};
use crate::error::{Error, Result};
use crate::ids::DocId;
use crate::journal::Journal;
use crate::vfs::{FsVfs, Vfs};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub const FORMAT_VERSION: u32 = 1;

/// The canonical document set. This is what `project.json` contains, and it is the single
/// source of truth — no pixels, no derived state, nothing that cannot be diffed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Project {
    #[serde(rename = "degenPaint")]
    pub format: u32,
    pub id: String,
    pub name: String,
    pub created: String,
    pub modified: String,
    pub active: DocId,
    #[schemars(with = "BTreeMap<String, Document>")]
    pub documents: IndexMap<DocId, Document>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub palette: BTreeMap<String, Color>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fonts: Vec<FontEntry>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FontEntry {
    pub family: String,
    pub asset: AssetRef,
    #[serde(default)]
    pub weight: u16,
    #[serde(default)]
    pub italic: bool,
}

impl Project {
    pub fn new(name: impl Into<String>, first: Document) -> Self {
        let now = now_iso();
        let id = format!("prj_{}", ulid::Ulid::new().to_string()[10..].to_ascii_lowercase());
        let active = first.id().clone();
        let mut documents = IndexMap::new();
        documents.insert(active.clone(), first);
        Self {
            format: FORMAT_VERSION,
            id,
            name: name.into(),
            created: now.clone(),
            modified: now,
            active,
            documents,
            palette: BTreeMap::new(),
            fonts: Vec::new(),
        }
    }

    pub fn doc(&self, id: &DocId) -> Result<&Document> {
        self.documents
            .get(id)
            .ok_or_else(|| Error::NoSuchDocument(id.to_string()))
    }

    pub fn doc_mut(&mut self, id: &DocId) -> Result<&mut Document> {
        self.documents
            .get_mut(id)
            .ok_or_else(|| Error::NoSuchDocument(id.to_string()))
    }

    /// Resolve a document by id or by name, falling back to the active document.
    pub fn resolve_doc(&self, hint: Option<&str>) -> Result<DocId> {
        let Some(h) = hint else {
            return Ok(self.active.clone());
        };
        let id = DocId::from(h);
        if self.documents.contains_key(&id) {
            return Ok(id);
        }
        if let Some((k, _)) = self.documents.iter().find(|(_, d)| d.name() == h) {
            return Ok(k.clone());
        }
        Err(Error::NoSuchDocument(h.to_string()))
    }

    pub fn add_document(&mut self, doc: Document) -> DocId {
        let id = doc.id().clone();
        self.documents.insert(id.clone(), doc);
        id
    }

    pub fn raster(&self, id: &DocId) -> Result<&RasterDoc> {
        self.doc(id)?.as_raster().ok_or_else(|| Error::WrongDocumentKind {
            op: "<raster>".into(),
            kind: self.doc(id).map(|d| d.kind().to_string()).unwrap_or_default(),
        })
    }

    pub fn raster_mut(&mut self, id: &DocId) -> Result<&mut RasterDoc> {
        let kind = self.doc(id)?.kind();
        self.doc_mut(id)?.as_raster_mut().ok_or(Error::WrongDocumentKind {
            op: "<raster>".into(),
            kind: kind.to_string(),
        })
    }

    pub fn vector(&self, id: &DocId) -> Result<&VectorDoc> {
        let kind = self.doc(id)?.kind();
        self.doc(id)?.as_vector().ok_or(Error::WrongDocumentKind {
            op: "<vector>".into(),
            kind: kind.to_string(),
        })
    }

    pub fn vector_mut(&mut self, id: &DocId) -> Result<&mut VectorDoc> {
        let kind = self.doc(id)?.kind();
        self.doc_mut(id)?.as_vector_mut().ok_or(Error::WrongDocumentKind {
            op: "<vector>".into(),
            kind: kind.to_string(),
        })
    }

    pub fn model(&self, id: &DocId) -> Result<&ModelDoc> {
        let kind = self.doc(id)?.kind();
        self.doc(id)?.as_model().ok_or(Error::WrongDocumentKind {
            op: "<model>".into(),
            kind: kind.to_string(),
        })
    }

    pub fn model_mut(&mut self, id: &DocId) -> Result<&mut ModelDoc> {
        let kind = self.doc(id)?.kind();
        self.doc_mut(id)?.as_model_mut().ok_or(Error::WrongDocumentKind {
            op: "<model>".into(),
            kind: kind.to_string(),
        })
    }

    /// Every asset referenced from any document. The keep-set for `asset.gc`.
    pub fn referenced_assets(&self) -> BTreeSet<AssetRef> {
        let mut out = BTreeSet::new();
        let v = serde_json::to_value(self).unwrap_or(serde_json::Value::Null);
        collect_assets(&v, &mut out);
        out
    }

    /// Depth-first dependency order: a document appears after everything it references,
    /// so a render walk never has to recurse. Errors on a cycle.
    pub fn render_order(&self) -> Result<Vec<DocId>> {
        let mut out = Vec::new();
        let mut state: BTreeMap<DocId, u8> = BTreeMap::new();
        fn visit(
            p: &Project,
            id: &DocId,
            state: &mut BTreeMap<DocId, u8>,
            out: &mut Vec<DocId>,
        ) -> Result<()> {
            match state.get(id) {
                Some(2) => return Ok(()),
                Some(1) => {
                    return Err(Error::CyclicLink {
                        from: id.to_string(),
                        to: id.to_string(),
                    })
                }
                _ => {}
            }
            state.insert(id.clone(), 1);
            if let Some(d) = p.documents.get(id) {
                for dep in d.dependencies() {
                    if p.documents.contains_key(&dep) {
                        visit(p, &dep, state, out)?;
                    }
                }
            }
            state.insert(id.clone(), 2);
            out.push(id.clone());
            Ok(())
        }
        for id in self.documents.keys() {
            visit(self, id, &mut state, &mut out)?;
        }
        Ok(out)
    }

    /// Would linking `from` to `to` create a cycle?
    pub fn would_cycle(&self, from: &DocId, to: &DocId) -> bool {
        if from == to {
            return true;
        }
        let mut stack = vec![to.clone()];
        let mut seen = BTreeSet::new();
        while let Some(cur) = stack.pop() {
            if &cur == from {
                return true;
            }
            if !seen.insert(cur.clone()) {
                continue;
            }
            if let Some(d) = self.documents.get(&cur) {
                stack.extend(d.dependencies());
            }
        }
        false
    }

    pub fn touch(&mut self) {
        self.modified = now_iso();
    }
}

fn collect_assets(v: &serde_json::Value, out: &mut BTreeSet<AssetRef>) {
    match v {
        serde_json::Value::String(s) if s.starts_with("blake3:") => {
            out.insert(AssetRef(s.clone()));
        }
        serde_json::Value::Array(a) => a.iter().for_each(|x| collect_assets(x, out)),
        serde_json::Value::Object(m) => m.values().for_each(|x| collect_assets(x, out)),
        _ => {}
    }
}

pub fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// A project in storage: the JSON, its asset store, and its journal. Which storage is the
/// [`Vfs`]'s business — a directory natively, an in-memory tree in the browser.
pub struct Workspace {
    pub project: Project,
    pub assets: AssetStore,
    pub journal: Journal,
    root: PathBuf,
    vfs: Arc<dyn Vfs>,
}

impl std::fmt::Debug for Workspace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Workspace")
            .field("root", &self.root)
            .field("project", &self.project.name)
            .finish_non_exhaustive()
    }
}

impl Workspace {
    pub fn create(root: impl AsRef<Path>, project: Project) -> Result<Self> {
        Self::create_with_vfs(root, project, FsVfs::shared())
    }

    pub fn create_with_vfs(
        root: impl AsRef<Path>,
        project: Project,
        vfs: Arc<dyn Vfs>,
    ) -> Result<Self> {
        let root = root.as_ref().to_path_buf();
        vfs.create_dir_all(&root.join("assets"))?;
        let ws = Self {
            assets: AssetStore::with_vfs(&root, Arc::clone(&vfs)),
            journal: Journal::with_vfs(root.join("history.jsonl"), Arc::clone(&vfs)),
            project,
            root,
            vfs,
        };
        ws.save()?;
        Ok(ws)
    }

    pub fn open(root: impl AsRef<Path>) -> Result<Self> {
        Self::open_with_vfs(root, FsVfs::shared())
    }

    pub fn open_with_vfs(root: impl AsRef<Path>, vfs: Arc<dyn Vfs>) -> Result<Self> {
        let root = root.as_ref().to_path_buf();
        let path = root.join("project.json");
        if !vfs.exists(&path) {
            return Err(Error::Invalid(format!(
                "no degen-paint project at {} (expected project.json)",
                root.display()
            )));
        }
        let bytes = vfs.read(&path)?;
        let project: Project = serde_json::from_slice(&bytes)?;
        if project.format > FORMAT_VERSION {
            return Err(Error::MigrationRequired {
                found: project.format,
                supported: FORMAT_VERSION,
            });
        }
        Ok(Self {
            assets: AssetStore::with_vfs(&root, Arc::clone(&vfs)),
            journal: Journal::with_vfs(root.join("history.jsonl"), Arc::clone(&vfs)),
            project,
            root,
            vfs,
        })
    }

    /// Find the nearest project directory from `start` upwards. Native only by nature:
    /// there is no directory to walk up from in a browser tab.
    pub fn discover(start: impl AsRef<Path>) -> Result<Self> {
        Self::open(FsVfs::discover_project_root(start.as_ref())?)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn vfs(&self) -> &Arc<dyn Vfs> {
        &self.vfs
    }

    /// Atomic write: the backend writes a temp file then renames, so a crash can never
    /// truncate a project.
    pub fn save(&self) -> Result<()> {
        let text = serde_json::to_string_pretty(&self.project)?;
        self.vfs.write(&self.root.join("project.json"), text.as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::{raster, Rect};
    use crate::ids::LayerId;

    fn project_with_link() -> Project {
        let logo = VectorDoc::new(DocId::from("doc_logo"), "logo", 100.0, 100.0);
        let mut main = RasterDoc::new(DocId::from("doc_main"), "main", 200, 200);
        main.layers.push(raster::Layer::new(
            LayerId::from("lyr_badge"),
            "badge",
            raster::LayerKind::Linked {
                document: DocId::from("doc_logo"),
                fit: raster::Fit::Contain,
                r#box: Rect::new(0.0, 0.0, 50.0, 50.0),
            },
        ));
        let mut p = Project::new("demo", Document::Raster(main));
        p.add_document(Document::Vector(logo));
        p
    }

    #[test]
    fn render_order_places_dependencies_first() {
        let p = project_with_link();
        let order = p.render_order().unwrap();
        let logo = order.iter().position(|d| d.as_str() == "doc_logo").unwrap();
        let main = order.iter().position(|d| d.as_str() == "doc_main").unwrap();
        assert!(logo < main, "a linked document must render before its consumer");
    }

    #[test]
    fn cycles_are_detected_before_they_are_created() {
        let p = project_with_link();
        assert!(p.would_cycle(&DocId::from("doc_logo"), &DocId::from("doc_main")));
        assert!(p.would_cycle(&DocId::from("doc_main"), &DocId::from("doc_main")));
        assert!(!p.would_cycle(&DocId::from("doc_main"), &DocId::from("doc_logo")));
    }

    #[test]
    fn documents_resolve_by_id_or_by_name_and_default_to_active() {
        let p = project_with_link();
        assert_eq!(p.resolve_doc(None).unwrap().as_str(), "doc_main");
        assert_eq!(p.resolve_doc(Some("logo")).unwrap().as_str(), "doc_logo");
        assert_eq!(p.resolve_doc(Some("doc_logo")).unwrap().as_str(), "doc_logo");
        assert_eq!(p.resolve_doc(Some("nope")).unwrap_err().code(), "no_such_document");
    }

    #[test]
    fn referenced_assets_are_discovered_anywhere_in_the_tree() {
        let mut p = project_with_link();
        let a = AssetRef::new("abc123", "png");
        p.raster_mut(&DocId::from("doc_main"))
            .unwrap()
            .layers
            .push(raster::Layer::new(
                LayerId::from("lyr_px"),
                "px",
                raster::LayerKind::Pixel { asset: a.clone(), offset: [0, 0] },
            ));
        assert!(p.referenced_assets().contains(&a));
    }

    #[test]
    fn workspace_saves_and_reopens_identically() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("demo.dpaint");
        let ws = Workspace::create(&root, project_with_link()).unwrap();
        let reopened = Workspace::open(&root).unwrap();
        assert_eq!(ws.project, reopened.project);
        assert_eq!(Workspace::discover(tmp.path()).unwrap().project, ws.project);
    }

    #[test]
    fn a_newer_format_demands_migration_rather_than_guessing() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("p.dpaint");
        let mut p = project_with_link();
        p.format = FORMAT_VERSION + 1;
        Workspace::create(&root, p).unwrap();
        assert_eq!(Workspace::open(&root).unwrap_err().code(), "migration_required");
    }
}
