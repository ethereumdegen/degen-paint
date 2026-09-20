//! Project-level ops: documents, assets, palette, fonts, history.
//!
//! These need no engine, so they live in core. Mode-specific ops come from the engine crates.

use crate::asset::AssetRef;
use crate::color::Color;
use crate::doc::{DocKind, Document, ModelDoc, RasterDoc, VectorDoc};
use crate::error::{Error, Result};
use crate::ids::DocId;
use crate::op::{parse_args, schema_for, Op, OpCx, OpEffect};
use crate::project::{FontEntry, Project};
use serde::Deserialize;

pub fn ops() -> Vec<Box<dyn Op>> {
    vec![
        Box::new(DocAdd),
        Box::new(DocRemove),
        Box::new(DocRename),
        Box::new(DocDuplicate),
        Box::new(DocSetActive),
        Box::new(DocResize),
        Box::new(AssetImport),
        Box::new(PaletteSet),
        Box::new(PaletteRemove),
        Box::new(FontRegister),
        Box::new(ProjectInfo),
    ]
}

/// Unique id derived from a name, with a numeric suffix if the name is taken.
fn unique_doc_id(p: &Project, name: &str) -> DocId {
    let base = DocId::from_name(name);
    if !p.documents.contains_key(&base) {
        return base;
    }
    (2..)
        .map(|n| DocId::from(format!("{}-{n}", base.as_str())))
        .find(|c| !p.documents.contains_key(c))
        .expect("an unbounded search always terminates")
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct DocAddArgs {
    /// Document name, also the basis for its id.
    pub name: String,
    /// raster | vector | model
    pub kind: String,
    /// Width in px. Required for raster and vector documents.
    #[serde(default)]
    pub width: Option<f64>,
    /// Height in px.
    #[serde(default)]
    pub height: Option<f64>,
    /// Output resolution for raster documents.
    #[serde(default)]
    pub dpi: Option<f32>,
    /// Make this the active document.
    #[serde(default)]
    pub activate: bool,
}

pub struct DocAdd;

impl Op for DocAdd {
    fn id(&self) -> &'static str {
        "doc.add"
    }
    fn about(&self) -> &'static str {
        "Add a document of any kind to the project"
    }
    fn schema(&self) -> serde_json::Value {
        schema_for::<DocAddArgs>()
    }
    fn apply(&self, p: &mut Project, args: serde_json::Value, _cx: &mut OpCx) -> Result<OpEffect> {
        let a: DocAddArgs = parse_args(self.id(), args)?;
        let kind: DocKind = a.kind.parse()?;
        let id = unique_doc_id(p, &a.name);
        let (w, h) = (a.width.unwrap_or(1024.0), a.height.unwrap_or(1024.0));
        if w <= 0.0 || h <= 0.0 {
            return Err(Error::Invalid(format!(
                "size must be positive, got {w}x{h}"
            )));
        }
        let doc = match kind {
            DocKind::Raster => {
                let mut d = RasterDoc::new(id.clone(), &a.name, w as u32, h as u32);
                if let Some(dpi) = a.dpi {
                    d.dpi = dpi;
                }
                Document::Raster(d)
            }
            DocKind::Vector => Document::Vector(VectorDoc::new(id.clone(), &a.name, w, h)),
            DocKind::Model => Document::Model(ModelDoc::new(id.clone(), &a.name)),
        };
        p.add_document(doc);
        if a.activate {
            p.active = id.clone();
        }
        Ok(OpEffect::changed(&id).with_created(id.to_string()))
    }
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct DocRefArgs {
    /// Document id or name.
    pub document: String,
}

pub struct DocRemove;

impl Op for DocRemove {
    fn id(&self) -> &'static str {
        "doc.remove"
    }
    fn about(&self) -> &'static str {
        "Remove a document; refuses while another document links to it"
    }
    fn schema(&self) -> serde_json::Value {
        schema_for::<DocRefArgs>()
    }
    fn apply(&self, p: &mut Project, args: serde_json::Value, _cx: &mut OpCx) -> Result<OpEffect> {
        let a: DocRefArgs = parse_args(self.id(), args)?;
        let id = p.resolve_doc(Some(&a.document))?;
        if p.documents.len() == 1 {
            return Err(Error::Invalid(
                "a project must keep at least one document".into(),
            ));
        }
        let dependents: Vec<String> = p
            .documents
            .iter()
            .filter(|(k, d)| **k != id && d.dependencies().contains(&id))
            .map(|(k, _)| k.to_string())
            .collect();
        if !dependents.is_empty() {
            return Err(Error::Invalid(format!(
                "{id} is still linked from {}; unlink those first",
                dependents.join(", ")
            )));
        }
        p.documents.shift_remove(&id);
        if p.active == id {
            p.active = p
                .documents
                .keys()
                .next()
                .cloned()
                .expect("at least one document remains");
        }
        Ok(OpEffect::default().with_removed(id.to_string()))
    }
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct DocRenameArgs {
    pub document: String,
    pub name: String,
}

pub struct DocRename;

impl Op for DocRename {
    fn id(&self) -> &'static str {
        "doc.rename"
    }
    fn about(&self) -> &'static str {
        "Rename a document (its id is stable and does not change)"
    }
    fn schema(&self) -> serde_json::Value {
        schema_for::<DocRenameArgs>()
    }
    fn apply(&self, p: &mut Project, args: serde_json::Value, _cx: &mut OpCx) -> Result<OpEffect> {
        let a: DocRenameArgs = parse_args(self.id(), args)?;
        let id = p.resolve_doc(Some(&a.document))?;
        p.doc_mut(&id)?.set_name(a.name);
        Ok(OpEffect::changed(&id))
    }
}

pub struct DocDuplicate;

impl Op for DocDuplicate {
    fn id(&self) -> &'static str {
        "doc.duplicate"
    }
    fn about(&self) -> &'static str {
        "Copy a document, giving every object in it a fresh id"
    }
    fn schema(&self) -> serde_json::Value {
        schema_for::<DocRefArgs>()
    }
    fn apply(&self, p: &mut Project, args: serde_json::Value, _cx: &mut OpCx) -> Result<OpEffect> {
        let a: DocRefArgs = parse_args(self.id(), args)?;
        let src = p.resolve_doc(Some(&a.document))?;
        let name = format!("{} copy", p.doc(&src)?.name());
        let new_id = unique_doc_id(p, &name);

        // Re-id through JSON so every nested object gets a fresh id without a bespoke
        // deep-clone per document kind.
        let mut v = serde_json::to_value(p.doc(&src)?)?;
        remap_ids(&mut v, &src, &new_id);
        let mut doc: Document = serde_json::from_value(v)?;
        doc.set_name(name);
        p.add_document(doc);
        Ok(OpEffect::changed(&new_id).with_created(new_id.to_string()))
    }
}

/// Give every `id` field in a document subtree a fresh suffix, keeping internal references
/// consistent by rewriting both sides with the same mapping.
fn remap_ids(v: &mut serde_json::Value, old_doc: &DocId, new_doc: &DocId) {
    let suffix = format!(
        "-{}",
        &ulid::Ulid::new().to_string()[20..].to_ascii_lowercase()
    );
    fn walk(v: &mut serde_json::Value, suffix: &str, old_doc: &str, new_doc: &str) {
        match v {
            serde_json::Value::Object(m) => {
                let keys: Vec<String> = m.keys().cloned().collect();
                for k in keys {
                    let is_id_field = k == "id";
                    if let Some(val) = m.get_mut(&k) {
                        if is_id_field {
                            if let Some(s) = val.as_str() {
                                let next = if s == old_doc {
                                    new_doc.to_string()
                                } else {
                                    format!("{s}{suffix}")
                                };
                                *val = serde_json::Value::String(next);
                                continue;
                            }
                        }
                        walk(val, suffix, old_doc, new_doc);
                    }
                }
            }
            serde_json::Value::Array(a) => {
                a.iter_mut().for_each(|x| walk(x, suffix, old_doc, new_doc))
            }
            _ => {}
        }
    }
    walk(v, &suffix, old_doc.as_str(), new_doc.as_str());
}

pub struct DocSetActive;

impl Op for DocSetActive {
    fn id(&self) -> &'static str {
        "doc.set-active"
    }
    fn about(&self) -> &'static str {
        "Choose the document subsequent ops target by default"
    }
    fn schema(&self) -> serde_json::Value {
        schema_for::<DocRefArgs>()
    }
    fn apply(&self, p: &mut Project, args: serde_json::Value, _cx: &mut OpCx) -> Result<OpEffect> {
        let a: DocRefArgs = parse_args(self.id(), args)?;
        let id = p.resolve_doc(Some(&a.document))?;
        p.active = id.clone();
        Ok(OpEffect::changed(&id))
    }
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct DocResizeArgs {
    #[serde(default)]
    pub document: Option<String>,
    pub width: f64,
    pub height: f64,
}

pub struct DocResize;

impl Op for DocResize {
    fn id(&self) -> &'static str {
        "doc.resize"
    }
    fn about(&self) -> &'static str {
        "Resize the canvas or artboard without scaling its contents"
    }
    fn schema(&self) -> serde_json::Value {
        schema_for::<DocResizeArgs>()
    }
    fn modes(&self) -> &'static [DocKind] {
        &[DocKind::Raster, DocKind::Vector]
    }
    fn apply(&self, p: &mut Project, args: serde_json::Value, cx: &mut OpCx) -> Result<OpEffect> {
        let a: DocResizeArgs = parse_args(self.id(), args)?;
        if a.width <= 0.0 || a.height <= 0.0 {
            return Err(Error::Invalid("width and height must be positive".into()));
        }
        let id = match &a.document {
            Some(d) => p.resolve_doc(Some(d))?,
            None => cx.target_doc(p)?,
        };
        match p.doc_mut(&id)? {
            Document::Raster(d) => d.size = [a.width as u32, a.height as u32],
            Document::Vector(d) => {
                if let Some(ab) = d.artboards.first_mut() {
                    ab.rect = crate::doc::Rect::new(ab.rect.x(), ab.rect.y(), a.width, a.height);
                }
            }
            Document::Model(_) => {
                return Err(Error::WrongDocumentKind {
                    op: self.id().into(),
                    kind: "model".into(),
                })
            }
        }
        Ok(OpEffect::changed(&id))
    }
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct AssetImportArgs {
    /// Path to a file on disk.
    pub path: String,
}

pub struct AssetImport;

impl Op for AssetImport {
    fn id(&self) -> &'static str {
        "asset.import"
    }
    fn about(&self) -> &'static str {
        "Import a file into the content-addressed asset store"
    }
    fn schema(&self) -> serde_json::Value {
        schema_for::<AssetImportArgs>()
    }
    fn apply(&self, _p: &mut Project, args: serde_json::Value, cx: &mut OpCx) -> Result<OpEffect> {
        let a: AssetImportArgs = parse_args(self.id(), args)?;
        let r = cx.assets.put_file(&a.path)?;
        let size = cx.assets.size_of(&r).unwrap_or(0);
        Ok(OpEffect::default()
            .with_created(r.to_string())
            .with_data(serde_json::json!({ "asset": r.as_str(), "bytes": size })))
    }
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct PaletteSetArgs {
    /// Palette entry name, usable anywhere a color is accepted.
    pub name: String,
    pub color: Color,
}

pub struct PaletteSet;

impl Op for PaletteSet {
    fn id(&self) -> &'static str {
        "palette.set"
    }
    fn about(&self) -> &'static str {
        "Define a project-level named color"
    }
    fn schema(&self) -> serde_json::Value {
        schema_for::<PaletteSetArgs>()
    }
    fn apply(&self, p: &mut Project, args: serde_json::Value, _cx: &mut OpCx) -> Result<OpEffect> {
        let a: PaletteSetArgs = parse_args(self.id(), args)?;
        p.palette.insert(a.name.clone(), a.color);
        Ok(OpEffect::default().with_created(a.name))
    }
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct PaletteRemoveArgs {
    pub name: String,
}

pub struct PaletteRemove;

impl Op for PaletteRemove {
    fn id(&self) -> &'static str {
        "palette.remove"
    }
    fn about(&self) -> &'static str {
        "Remove a named color"
    }
    fn schema(&self) -> serde_json::Value {
        schema_for::<PaletteRemoveArgs>()
    }
    fn apply(&self, p: &mut Project, args: serde_json::Value, _cx: &mut OpCx) -> Result<OpEffect> {
        let a: PaletteRemoveArgs = parse_args(self.id(), args)?;
        if p.palette.remove(&a.name).is_none() {
            return Err(Error::Invalid(format!(
                "no palette entry named '{}'",
                a.name
            )));
        }
        Ok(OpEffect::default().with_removed(a.name))
    }
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct FontRegisterArgs {
    /// Path to a .ttf or .otf file.
    pub path: String,
    /// Family name ops will refer to. Defaults to the file stem.
    #[serde(default)]
    pub family: Option<String>,
    #[serde(default)]
    pub weight: Option<u16>,
    #[serde(default)]
    pub italic: bool,
}

pub struct FontRegister;

impl Op for FontRegister {
    fn id(&self) -> &'static str {
        "font.register"
    }
    fn about(&self) -> &'static str {
        "Embed a font in the project so renders are reproducible anywhere"
    }
    fn schema(&self) -> serde_json::Value {
        schema_for::<FontRegisterArgs>()
    }
    fn apply(&self, p: &mut Project, args: serde_json::Value, cx: &mut OpCx) -> Result<OpEffect> {
        let a: FontRegisterArgs = parse_args(self.id(), args)?;
        let path = std::path::Path::new(&a.path);
        let family = a.family.unwrap_or_else(|| {
            path.file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "embedded".into())
        });
        let asset = cx.assets.put_file(path)?;
        p.fonts.retain(|f| {
            !(f.family == family && f.weight == a.weight.unwrap_or(400) && f.italic == a.italic)
        });
        p.fonts.push(FontEntry {
            family: family.clone(),
            asset: asset.clone(),
            weight: a.weight.unwrap_or(400),
            italic: a.italic,
        });
        Ok(OpEffect::default()
            .with_created(family)
            .with_data(serde_json::json!({ "asset": asset.as_str() })))
    }
}

pub struct ProjectInfo;

impl Op for ProjectInfo {
    fn id(&self) -> &'static str {
        "project.info"
    }
    fn about(&self) -> &'static str {
        "Summarize the project: documents, sizes, assets, palette"
    }
    fn schema(&self) -> serde_json::Value {
        serde_json::json!({ "type": "object", "properties": {}, "additionalProperties": false })
    }
    fn is_query(&self) -> bool {
        true
    }
    fn apply(&self, p: &mut Project, _args: serde_json::Value, cx: &mut OpCx) -> Result<OpEffect> {
        let docs: Vec<serde_json::Value> = p
            .documents
            .values()
            .map(|d| {
                serde_json::json!({
                    "id": d.id().as_str(),
                    "name": d.name(),
                    "kind": d.kind().as_str(),
                    "size": d.size().map(|(w, h)| vec![w, h]),
                    "dependsOn": d.dependencies().iter().map(|x| x.to_string()).collect::<Vec<_>>(),
                })
            })
            .collect();
        let referenced: Vec<AssetRef> = p.referenced_assets().into_iter().collect();
        Ok(OpEffect::default().with_data(serde_json::json!({
            "name": p.name,
            "active": p.active.as_str(),
            "documents": docs,
            "assets": { "referenced": referenced.len(), "onDisk": cx.assets.list().map(|l| l.len()).unwrap_or(0) },
            "palette": p.palette.iter().map(|(k, v)| (k.clone(), v.to_hex())).collect::<std::collections::BTreeMap<_, _>>(),
            "fonts": p.fonts.iter().map(|f| f.family.clone()).collect::<Vec<_>>(),
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asset::AssetStore;
    use crate::op::Registry;

    fn setup() -> (tempfile::TempDir, Registry, Project) {
        let tmp = tempfile::tempdir().unwrap();
        let mut reg = Registry::new();
        reg.extend(ops());
        let p = Project::new(
            "demo",
            Document::Raster(RasterDoc::new(DocId::from("doc_main"), "main", 100, 100)),
        );
        (tmp, reg, p)
    }

    fn run(
        reg: &Registry,
        p: &mut Project,
        assets: &AssetStore,
        id: &str,
        args: serde_json::Value,
    ) -> Result<OpEffect> {
        reg.get(id).unwrap().apply(p, args, &mut OpCx::new(assets))
    }

    #[test]
    fn documents_of_every_kind_can_be_added_and_activated() {
        let (tmp, reg, mut p) = setup();
        let assets = AssetStore::new(tmp.path());
        for (name, kind) in [("logo", "vector"), ("badge", "model"), ("poster", "raster")] {
            run(
                &reg,
                &mut p,
                &assets,
                "doc.add",
                serde_json::json!({ "name": name, "kind": kind, "width": 64, "height": 64 }),
            )
            .unwrap();
        }
        assert_eq!(p.documents.len(), 4);
        assert_eq!(p.resolve_doc(Some("logo")).unwrap().as_str(), "doc_logo");

        run(
            &reg,
            &mut p,
            &assets,
            "doc.set-active",
            serde_json::json!({ "document": "logo" }),
        )
        .unwrap();
        assert_eq!(p.active.as_str(), "doc_logo");
    }

    #[test]
    fn adding_a_duplicate_name_does_not_collide_ids() {
        let (tmp, reg, mut p) = setup();
        let assets = AssetStore::new(tmp.path());
        for _ in 0..2 {
            run(
                &reg,
                &mut p,
                &assets,
                "doc.add",
                serde_json::json!({ "name": "logo", "kind": "vector" }),
            )
            .unwrap();
        }
        let ids: Vec<&str> = p.documents.keys().map(|k| k.as_str()).collect();
        assert!(
            ids.contains(&"doc_logo") && ids.contains(&"doc_logo-2"),
            "got {ids:?}"
        );
    }

    #[test]
    fn removing_a_linked_document_is_refused_with_a_reason() {
        use crate::doc::{raster::*, Rect};
        use crate::ids::LayerId;
        let (tmp, reg, mut p) = setup();
        let assets = AssetStore::new(tmp.path());
        run(
            &reg,
            &mut p,
            &assets,
            "doc.add",
            serde_json::json!({ "name": "logo", "kind": "vector" }),
        )
        .unwrap();
        p.raster_mut(&DocId::from("doc_main"))
            .unwrap()
            .layers
            .push(Layer::new(
                LayerId::from("lyr_link"),
                "link",
                LayerKind::Linked {
                    document: DocId::from("doc_logo"),
                    fit: Fit::Contain,
                    r#box: Rect::new(0.0, 0.0, 10.0, 10.0),
                },
            ));
        let err = run(
            &reg,
            &mut p,
            &assets,
            "doc.remove",
            serde_json::json!({ "document": "logo" }),
        )
        .unwrap_err();
        assert!(err.to_string().contains("still linked"), "got: {err}");
        assert_eq!(p.documents.len(), 2);
    }

    #[test]
    fn duplicating_a_document_gives_every_object_a_fresh_id() {
        use crate::doc::raster::*;
        use crate::ids::LayerId;
        let (tmp, reg, mut p) = setup();
        let assets = AssetStore::new(tmp.path());
        p.raster_mut(&DocId::from("doc_main"))
            .unwrap()
            .layers
            .push(Layer::new(
                LayerId::from("lyr_bg"),
                "bg",
                LayerKind::Fill {
                    color: Color::WHITE,
                },
            ));
        run(
            &reg,
            &mut p,
            &assets,
            "doc.duplicate",
            serde_json::json!({ "document": "doc_main" }),
        )
        .unwrap();
        assert_eq!(p.documents.len(), 2);
        let copy = p
            .documents
            .values()
            .find(|d| d.name() == "main copy")
            .unwrap();
        let copy_layer = &copy.as_raster().unwrap().layers[0];
        assert_ne!(
            copy_layer.id.as_str(),
            "lyr_bg",
            "a copy must not share ids with its source"
        );
        assert_eq!(copy_layer.name, "bg");
    }

    #[test]
    fn importing_an_asset_deduplicates_and_reports_its_ref() {
        let (tmp, reg, mut p) = setup();
        let assets = AssetStore::new(tmp.path());
        let f = tmp.path().join("in.png");
        std::fs::write(&f, b"not really a png").unwrap();
        let e1 = run(
            &reg,
            &mut p,
            &assets,
            "asset.import",
            serde_json::json!({ "path": f.to_str().unwrap() }),
        )
        .unwrap();
        let e2 = run(
            &reg,
            &mut p,
            &assets,
            "asset.import",
            serde_json::json!({ "path": f.to_str().unwrap() }),
        )
        .unwrap();
        assert_eq!(e1.created, e2.created);
        assert_eq!(assets.list().unwrap().len(), 1);
        assert_eq!(e1.data.unwrap()["bytes"], 16);
    }

    #[test]
    fn project_info_is_a_query_and_reports_dependencies() {
        let (tmp, reg, mut p) = setup();
        let assets = AssetStore::new(tmp.path());
        assert!(reg.get("project.info").unwrap().is_query());
        let e = run(&reg, &mut p, &assets, "project.info", serde_json::json!({})).unwrap();
        let d = e.data.unwrap();
        assert_eq!(d["documents"][0]["kind"], "raster");
        assert_eq!(d["active"], "doc_main");
    }

    #[test]
    fn resizing_rejects_nonsense_dimensions() {
        let (tmp, reg, mut p) = setup();
        let assets = AssetStore::new(tmp.path());
        let err = run(
            &reg,
            &mut p,
            &assets,
            "doc.resize",
            serde_json::json!({ "width": 0, "height": 10 }),
        )
        .unwrap_err();
        assert_eq!(err.code(), "invalid");
        assert_eq!(p.raster(&DocId::from("doc_main")).unwrap().size, [100, 100]);
    }
}
