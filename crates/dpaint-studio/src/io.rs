//! Files in and files out: Import, Export, Send to Editor and Export Preview.
//!
//! Starkbot never moves a file, so every hand-off is one of these four actions. Import is
//! an op like any other — it is journalled and a single undo takes it back — and export
//! goes through `dpaint_render`, the same path `dpaint render` takes, so what the Studio
//! writes and what the CLI writes are the same bytes.

use dpaint_core::doc::common::Provenance;
use dpaint_core::doc::vector::{VKind, VObject};
use dpaint_core::doc::Document;
use dpaint_core::{
    now_iso, parse_args, schema_for, AssetStore, DocId, Engine, Error, LayerId, ObjectId, Op, OpCx,
    OpEffect, Project, Registry, Result, Workspace,
};
use dpaint_inspect::DigestOptions;
use dpaint_render::{ImageFormat, RenderOptions};
use image::RgbaImage;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

/// Ops this crate contributes. Import is here rather than in a mode crate because it
/// spans all three: one file, one journal entry, whichever document kind it lands in.
pub fn ops() -> Vec<Box<dyn Op>> {
    vec![Box::new(ImportFile)]
}

/// The registry without the ops defined here, so [`ImportFile`] can delegate to
/// `asset.import`, `doc.add` and friends instead of reimplementing them.
static BASE: LazyLock<Registry> = LazyLock::new(crate::api::base_registry);

fn run(
    op: &str,
    project: &mut Project,
    args: Value,
    cx: &OpCx,
    doc: Option<String>,
) -> Result<OpEffect> {
    let mut sub = OpCx::new(cx.assets).with_doc(doc);
    sub.dry_run = cx.dry_run;
    BASE.get(op)?.apply(project, args, &mut sub)
}

// ------------------------------------------------------------------------------- import

#[derive(Debug, Clone, Copy, Default, PartialEq, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum ImportMode {
    /// Add to a document that is already open.
    #[default]
    Layer,
    /// Create a document of the kind the file implies.
    Document,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ImportArgs {
    /// Path to a png, jpg, webp, tiff, svg, gltf or glb file.
    pub path: String,
    #[serde(default)]
    pub mode: ImportMode,
    /// Name for what is created. Defaults to the file's stem.
    #[serde(default)]
    pub name: Option<String>,
}

pub struct ImportFile;

impl Op for ImportFile {
    fn id(&self) -> &'static str {
        "io.import"
    }
    fn about(&self) -> &'static str {
        "Import an image, SVG or glTF file as a layer, objects or a new document"
    }
    fn schema(&self) -> Value {
        schema_for::<ImportArgs>()
    }
    fn apply(&self, p: &mut Project, args: Value, cx: &mut OpCx) -> Result<OpEffect> {
        let a: ImportArgs = parse_args(self.id(), args)?;
        let path = PathBuf::from(&a.path);
        if !path.is_file() {
            return Err(Error::Invalid(format!(
                "nothing to import at {}",
                path.display()
            )));
        }
        let name = a.name.clone().unwrap_or_else(|| stem(&path));
        let side = sidecar(&path);
        let prov = side.as_ref().and_then(provenance_of);

        let effect = match classify(&path)? {
            Incoming::Image => image_in(p, cx, &path, &name, a.mode, prov.as_ref())?,
            Incoming::Svg => svg_in(p, cx, &path, &name, a.mode, prov.as_ref())?,
            Incoming::Model => model_in(p, cx, &path, &name, a.mode)?,
        };
        let doc = effect.changed.first().map(|d| d.to_string());
        Ok(effect.with_data(json!({ "sidecar": side, "doc": doc })))
    }
}

enum Incoming {
    Image,
    Svg,
    Model,
}

fn classify(path: &Path) -> Result<Incoming> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match ext.as_str() {
        "png" | "jpg" | "jpeg" | "webp" | "tif" | "tiff" => Ok(Incoming::Image),
        "svg" => Ok(Incoming::Svg),
        "gltf" | "glb" => Ok(Incoming::Model),
        other => Err(Error::UnsupportedFormat(format!(
            "'{other}' cannot be imported; degen-paint reads png, jpg, webp, tiff, svg, gltf and glb"
        ))),
    }
}

fn image_in(
    p: &mut Project,
    cx: &OpCx,
    path: &Path,
    name: &str,
    mode: ImportMode,
    prov: Option<&Provenance>,
) -> Result<OpEffect> {
    if mode == ImportMode::Document {
        // Header-only read: a 300 DPI poster does not need decoding to be measured.
        let (w, h) = image::image_dimensions(path)
            .map_err(|e| Error::AssetDecode(format!("{}: {e}", path.display())))?;
        let doc = one_doc(run(
            "doc.add",
            p,
            json!({ "name": name, "kind": "raster", "width": w, "height": h, "activate": true }),
            cx,
            None,
        )?)?;
        let mut effect = pixel_layer(p, cx, path, name, &doc, prov)?;
        effect.created.insert(0, doc.to_string());
        return Ok(effect);
    }

    let doc = cx.target_doc(p)?;
    match p.doc(&doc)? {
        Document::Raster(_) => pixel_layer(p, cx, path, name, &doc, prov),
        Document::Vector(_) => {
            let effect = run(
                "vector.object.add-image",
                p,
                json!({ "path": path_str(path)?, "name": name }),
                cx,
                Some(doc.to_string()),
            )?;
            if let (Some(prov), Some(id)) = (prov, effect.created.first()) {
                if let Some(o) = p.vector_mut(&doc)?.object_mut(&ObjectId::from(id.as_str())) {
                    o.provenance = Some(prov.clone());
                }
            }
            Ok(effect)
        }
        Document::Model(d) => Err(Error::Invalid(format!(
            "'{}' is a model document; an image becomes a pixel layer in a raster document or \
             an image object in a vector document — import it as a new document instead",
            d.name
        ))),
    }
}

fn pixel_layer(
    p: &mut Project,
    cx: &OpCx,
    path: &Path,
    name: &str,
    doc: &DocId,
    prov: Option<&Provenance>,
) -> Result<OpEffect> {
    let stored = run(
        "asset.import",
        p,
        json!({ "path": path_str(path)? }),
        cx,
        None,
    )?;
    let asset = stored
        .data
        .as_ref()
        .and_then(|d| d.get("asset"))
        .and_then(|a| a.as_str())
        .ok_or_else(|| Error::Invalid("asset.import reported no blob".into()))?
        .to_string();

    let effect = run(
        "raster.layer.add",
        p,
        json!({ "type": "pixel", "asset": asset, "name": name }),
        cx,
        Some(doc.to_string()),
    )?;
    if let (Some(prov), Some(id)) = (prov, effect.created.first()) {
        if let Some(l) = p.raster_mut(doc)?.layer_mut(&LayerId::from(id.as_str())) {
            l.provenance = Some(prov.clone());
        }
    }
    Ok(effect)
}

fn svg_in(
    p: &mut Project,
    cx: &OpCx,
    path: &Path,
    name: &str,
    mode: ImportMode,
    prov: Option<&Provenance>,
) -> Result<OpEffect> {
    let text = std::fs::read_to_string(path)?;
    let imported = dpaint_vector::import_svg(&text, DocId::from("doc_import"), name)?;

    if mode == ImportMode::Document {
        let (w, h) = imported.size();
        let doc = one_doc(run(
            "doc.add",
            p,
            json!({ "name": name, "kind": "vector", "width": w, "height": h, "activate": true }),
            cx,
            None,
        )?)?;
        // A whole document of its own needs no wrapper group: the document is the group.
        let mut created = merge(p.vector_mut(&doc)?, imported.objects, name, prov, false);
        created.insert(0, doc.to_string());
        return Ok(OpEffect {
            changed: vec![doc],
            created,
            ..Default::default()
        });
    }

    let doc = cx.target_doc(p)?;
    let target = p.doc(&doc)?;
    if target.as_vector().is_none() {
        return Err(Error::Invalid(format!(
            "'{}' is a {} document; an SVG imports as vector objects — target a vector \
             document, or import it as a new document",
            target.name(),
            target.kind().as_str()
        )));
    }
    let created = merge(p.vector_mut(&doc)?, imported.objects, name, prov, true);
    Ok(OpEffect {
        changed: vec![doc],
        created,
        ..Default::default()
    })
}

fn model_in(
    p: &mut Project,
    cx: &OpCx,
    path: &Path,
    name: &str,
    mode: ImportMode,
) -> Result<OpEffect> {
    if mode == ImportMode::Document {
        let doc = one_doc(run(
            "doc.add",
            p,
            json!({ "name": name, "kind": "model", "activate": true }),
            cx,
            None,
        )?)?;
        let mut effect = run(
            "model.mesh.import",
            p,
            json!({ "file": path_str(path)?, "name": name }),
            cx,
            Some(doc.to_string()),
        )?;
        effect.created.insert(0, doc.to_string());
        return Ok(effect);
    }

    let doc = cx.target_doc(p)?;
    let target = p.doc(&doc)?;
    if target.as_model().is_none() {
        return Err(Error::Invalid(format!(
            "'{}' is a {} document; geometry needs a model document — import it as a new \
             document instead",
            target.name(),
            target.kind().as_str()
        )));
    }
    run(
        "model.mesh.import",
        p,
        json!({ "file": path_str(path)?, "name": name }),
        cx,
        Some(doc.to_string()),
    )
}

/// The document `doc.add` just created.
fn one_doc(effect: OpEffect) -> Result<DocId> {
    effect
        .changed
        .into_iter()
        .next()
        .ok_or_else(|| Error::Invalid("doc.add created no document".into()))
}

/// Merge imported objects into a vector document, giving colliding ids a fresh name and
/// following the renames through clip and mask references so the geometry still draws.
fn merge(
    target: &mut dpaint_core::VectorDoc,
    mut objects: Vec<VObject>,
    label: &str,
    prov: Option<&Provenance>,
    group: bool,
) -> Vec<String> {
    let mut used: BTreeSet<String> = target.walk().iter().map(|o| o.id.to_string()).collect();
    let mut renames: BTreeMap<String, String> = BTreeMap::new();
    uniquify(&mut objects, &mut used, &mut renames);

    let mut created = Vec::new();
    settle(&mut objects, &renames, prov, &mut created);

    if group {
        let id = unique_id(&used, &ObjectId::from_name(label).to_string());
        created.insert(0, id.clone());
        let mut wrapper = VObject::new(ObjectId::from(id), label, VKind::Group { objects });
        wrapper.provenance = prov.cloned();
        target.objects.push(wrapper);
    } else {
        target.objects.extend(objects);
    }
    created
}

fn uniquify(
    objects: &mut [VObject],
    used: &mut BTreeSet<String>,
    renames: &mut BTreeMap<String, String>,
) {
    for o in objects.iter_mut() {
        let original = o.id.to_string();
        if used.contains(&original) {
            let fresh = unique_id(used, &original);
            renames.insert(original, fresh.clone());
            o.id = ObjectId::from(fresh);
        }
        used.insert(o.id.to_string());
        if let VKind::Group { objects } = &mut o.kind {
            uniquify(objects, used, renames);
        }
    }
}

fn settle(
    objects: &mut [VObject],
    renames: &BTreeMap<String, String>,
    prov: Option<&Provenance>,
    out: &mut Vec<String>,
) {
    for o in objects.iter_mut() {
        if let Some(new) = o.clip.as_ref().and_then(|c| renames.get(c.as_str())) {
            o.clip = Some(ObjectId::from(new.clone()));
        }
        if let Some(new) = o.mask.as_ref().and_then(|m| renames.get(m.as_str())) {
            o.mask = Some(ObjectId::from(new.clone()));
        }
        if prov.is_some() {
            o.provenance = prov.cloned();
        }
        out.push(o.id.to_string());
        if let VKind::Group { objects } = &mut o.kind {
            settle(objects, renames, prov, out);
        }
    }
}

fn unique_id(used: &BTreeSet<String>, base: &str) -> String {
    if !used.contains(base) {
        return base.to_string();
    }
    (2..)
        .map(|n| format!("{base}-{n}"))
        .find(|c| !used.contains(c))
        .unwrap_or_else(|| ObjectId::generate().to_string())
}

/// DMS writes `<take>.json` beside the file it hands off. Read it leniently: the fields
/// we understand become provenance, the rest is echoed to the caller and ignored.
fn sidecar(path: &Path) -> Option<Value> {
    let side = path.with_extension("json");
    if side == path {
        return None;
    }
    serde_json::from_slice(&std::fs::read(side).ok()?).ok()
}

fn provenance_of(v: &Value) -> Option<Provenance> {
    let first = |keys: &[&str]| {
        keys.iter()
            .find_map(|k| v.get(*k).and_then(|x| x.as_str()).map(String::from))
    };
    let number = |keys: &[&str]| keys.iter().find_map(|k| v.get(*k).and_then(|x| x.as_f64()));

    let prompt = first(&["prompt", "text"]);
    let model = first(&["model", "modelId", "model_id"]);
    let cost = number(&["cost", "costUsd", "cost_usd"]);
    let parents: Vec<String> = v
        .get("parents")
        .and_then(|p| p.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    if prompt.is_none() && model.is_none() && cost.is_none() && parents.is_empty() {
        return None;
    }
    Some(Provenance {
        provider: first(&["provider"]).unwrap_or_else(|| "import".into()),
        model: model.unwrap_or_default(),
        prompt,
        seed: v.get("seed").and_then(|s| s.as_i64()),
        request_id: first(&["id", "requestId", "request_id", "take"]),
        at: first(&["at", "createdAt", "created_at"]).unwrap_or_else(now_iso),
        cost_usd: cost,
        parents,
    })
}

// ------------------------------------------------------------------------------- export

/// Write a document to a file. `frames` turns a model document into a turntable sequence
/// plus a contact sheet; everything else is one render through `dpaint_render`.
pub fn export(engine: &mut Engine, params: &Value) -> Result<Value> {
    let path = PathBuf::from(crate::api::str_param(params, "path")?);
    let overwrite = flag(params, "overwrite");
    let doc = engine
        .workspace
        .project
        .resolve_doc(params.get("doc").and_then(|d| d.as_str()))?;
    refuse_existing(&path, overwrite)?;

    let scale = scale_of(&engine.workspace.project, &doc, params)?;
    if let Some(frames) = params
        .get("frames")
        .and_then(|f| f.as_u64())
        .filter(|f| *f > 0)
    {
        return turntable(
            &engine.workspace,
            &doc,
            &path,
            frames as u32,
            scale,
            overwrite,
        );
    }

    let applied = engine.apply(
        "render.image",
        json!({ "path": path_str(&path)?, "document": doc.as_str(), "scale": scale }),
        None,
        false,
    )?;
    let written = applied
        .effect
        .data
        .ok_or_else(|| Error::Invalid("render.image reported nothing".into()))?;
    let size = match written.get("size").cloned() {
        Some(Value::Array(wh)) => Value::Array(wh),
        // SVG and glTF have no pixel grid; their size is the document's own extent.
        _ => {
            let opts = RenderOptions {
                scale,
                ..Default::default()
            };
            let (w, h) = dpaint_render::natural_size(engine.workspace.project.doc(&doc)?, &opts);
            json!([w, h])
        }
    };
    Ok(json!({
        "path": written.get("path").cloned().unwrap_or(json!(path_str(&path)?)),
        "bytes": written.get("bytes").cloned().unwrap_or(json!(0)),
        "size": size,
    }))
}

/// Frames land in `<stem>_frames/` beside the contact sheet, which is what the returned
/// path names: one file the operator can look at, with the sequence next to it.
fn turntable(
    ws: &Workspace,
    doc: &DocId,
    path: &Path,
    frames: u32,
    scale: f64,
    overwrite: bool,
) -> Result<Value> {
    let side = ((512.0 * scale).round() as u32).max(1);
    let opts = RenderOptions {
        size: Some((side, side)),
        ..Default::default()
    };
    let rendered =
        dpaint_render::turntable(&ws.project, doc, &AssetStore::new(ws.root()), frames, &opts)?;

    let dir = path
        .parent()
        .unwrap_or(Path::new("."))
        .join(format!("{}_frames", stem(path)));
    let paths: Vec<PathBuf> = (0..rendered.len())
        .map(|i| dir.join(format!("frame_{i:03}.png")))
        .collect();
    // Refuse the whole sequence before writing any of it: half a turntable on disk is
    // worse than none.
    for frame in &paths {
        refuse_existing(frame, overwrite)?;
    }
    std::fs::create_dir_all(&dir)?;
    for (frame, pm) in paths.iter().zip(&rendered) {
        let bytes = dpaint_render::encode::encode(
            &dpaint_render::encode::to_rgba(pm),
            ImageFormat::Png,
            100,
        )?;
        std::fs::write(frame, bytes)?;
    }

    let sheet = contact_sheet(&rendered);
    let format = ImageFormat::from_path(path_str(path)?)?;
    let bytes = dpaint_render::encode::encode(&sheet, format, 100)?;
    std::fs::write(path, &bytes)?;
    Ok(json!({
        "path": path_str(path)?,
        "bytes": bytes.len(),
        "size": [sheet.width(), sheet.height()],
        "frames": rendered.len(),
    }))
}

/// A square-ish grid of every frame, so one `look` covers the whole turntable.
fn contact_sheet(frames: &[tiny_skia::Pixmap]) -> RgbaImage {
    let (fw, fh) = frames
        .first()
        .map(|p| (p.width(), p.height()))
        .unwrap_or((1, 1));
    let cols = (frames.len() as f64).sqrt().ceil().max(1.0) as u32;
    let rows = (frames.len() as u32).div_ceil(cols).max(1);
    let mut sheet = RgbaImage::new(cols * fw, rows * fh);
    for (i, pm) in frames.iter().enumerate() {
        let img = dpaint_render::encode::to_rgba(pm);
        let (ox, oy) = ((i as u32 % cols) * fw, (i as u32 / cols) * fh);
        for y in 0..img.height().min(fh) {
            for x in 0..img.width().min(fw) {
                sheet.put_pixel(ox + x, oy + y, *img.get_pixel(x, y));
            }
        }
    }
    sheet
}

/// An explicit DPI is a scale request: 300 DPI out of a 72 DPI document is 300/72×.
fn scale_of(p: &Project, doc: &DocId, params: &Value) -> Result<f64> {
    let mut scale = params.get("scale").and_then(|s| s.as_f64()).unwrap_or(1.0);
    if scale <= 0.0 {
        return Err(Error::Invalid(format!(
            "scale must be positive, got {scale}"
        )));
    }
    if let Some(dpi) = params.get("dpi").and_then(|d| d.as_f64()) {
        let native = p
            .doc(doc)?
            .as_raster()
            .map(|r| r.dpi as f64)
            .unwrap_or(72.0);
        if dpi <= 0.0 || native <= 0.0 {
            return Err(Error::Invalid(format!("dpi must be positive, got {dpi}")));
        }
        scale *= dpi / native;
    }
    Ok(scale)
}

fn refuse_existing(path: &Path, overwrite: bool) -> Result<()> {
    if !overwrite && path.exists() {
        return Err(Error::Exists(format!(
            "{} already exists; pass overwrite to replace it",
            path.display()
        )));
    }
    Ok(())
}

// ----------------------------------------------------------------------- send to editor

/// Export documents to the folder Diffusion Studio and Powermove import from, each with
/// the sidecar DMS's own hand-off has, so the lineage survives the trip.
pub fn send_to_editor(ws: &mut Workspace, params: &Value) -> Result<Value> {
    let docs: Vec<String> = params
        .get("docs")
        .and_then(|d| d.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    if docs.is_empty() {
        return Err(Error::Invalid(
            "send to editor needs at least one document".into(),
        ));
    }
    let overwrite = flag(params, "overwrite");
    let dir = match params.get("dir").and_then(|d| d.as_str()) {
        Some(d) => PathBuf::from(d),
        None => videos_dir()
            .join("degen-paint")
            .join(slug(&ws.project.name)),
    };
    std::fs::create_dir_all(&dir)?;

    let revision = revision(ws)?;
    let assets = AssetStore::new(ws.root());
    let opts = RenderOptions::default();
    let mut files = Vec::new();

    for hint in &docs {
        let id = ws.project.resolve_doc(Some(hint))?;
        let doc = ws.project.doc(&id)?;
        let base = slug(doc.name());
        let side = dir.join(format!("{base}.json"));

        // PNG for every kind, plus the document's own native format where it has one.
        let mut exts = vec!["png"];
        match doc {
            Document::Vector(_) => exts.push("svg"),
            Document::Model(_) => exts.push("glb"),
            Document::Raster(_) => {}
        }

        // Every target for this document is checked before the first byte is written, so
        // a refusal leaves the folder as the operator last saw it.
        refuse_existing(&side, overwrite)?;
        for ext in &exts {
            refuse_existing(&dir.join(format!("{base}.{ext}")), overwrite)?;
        }

        let mut size = json!(null);
        for ext in &exts {
            let out = dir.join(format!("{base}.{ext}"));
            let written = dpaint_render::export_document(
                &ws.project,
                &id,
                &assets,
                path_str(&out)?,
                &opts,
                100,
            )?;
            if let Some(wh) = written.size {
                size = json!(wh);
            }
            files.push(json!({ "path": written.path, "sidecar": path_str(&side)? }));
        }
        if size.is_null() {
            let (w, h) = dpaint_render::natural_size(doc, &opts);
            size = json!([w, h]);
        }

        let digest = dpaint_inspect::digest::digest(
            &ws.project,
            &id,
            &assets,
            &DigestOptions {
                per_object: false,
                ..Default::default()
            },
        )?;
        let sidecar = json!({
            "project": ws.project.name,
            "document": doc.name(),
            "kind": doc.kind().as_str(),
            "revision": revision,
            "size": size,
            "digestSummary": {
                "objects": digest.tree.len(),
                "size": digest.size,
                "alphaCoverage": digest.alpha_coverage,
                "meanColor": digest.mean_color,
                "dominantColors": digest.dominant_colors.iter().take(3)
                    .map(|c| json!({ "color": c.color, "fraction": c.fraction }))
                    .collect::<Vec<_>>(),
            },
            "provenance": provenances(doc),
        });
        std::fs::write(&side, serde_json::to_string_pretty(&sidecar)?)?;
    }
    Ok(json!({ "files": files }))
}

/// Every provenance record in the document, deduplicated: where this artwork came from.
fn provenances(doc: &Document) -> Vec<Value> {
    let found: Vec<&Provenance> = match doc {
        Document::Raster(r) => r
            .walk()
            .iter()
            .filter_map(|l| l.provenance.as_ref())
            .collect(),
        Document::Vector(v) => v
            .walk()
            .iter()
            .filter_map(|o| o.provenance.as_ref())
            .collect(),
        Document::Model(_) => Vec::new(),
    };
    let mut unique: Vec<&Provenance> = Vec::new();
    for p in found {
        if !unique.contains(&p) {
            unique.push(p);
        }
    }
    unique
        .into_iter()
        .filter_map(|p| serde_json::to_value(p).ok())
        .collect()
}

// ----------------------------------------------------------------------- export preview

/// The render Sol is allowed to look at, and its annotated twin, at a path that states
/// the revision it belongs to.
pub fn export_preview(ws: &mut Workspace, params: &Value) -> Result<Value> {
    let id = ws
        .project
        .resolve_doc(params.get("doc").and_then(|d| d.as_str()))?;
    let revision = revision(ws)?;
    let dir = pictures_dir().join("degen-paint").join("previews");
    std::fs::create_dir_all(&dir)?;

    let stem = format!(
        "{}-{}-r{revision}",
        slug(&ws.project.name),
        slug(ws.project.doc(&id)?.name())
    );
    let png = dir.join(format!("{stem}.png"));
    let annotated = dir.join(format!("{stem}-annotated.png"));

    let assets = AssetStore::new(ws.root());
    let opts = DigestOptions::default();
    let base = dpaint_render::render_document(&ws.project, &id, &assets, &opts.render)?;
    std::fs::write(&png, encode_png(&base)?)?;

    let digest = dpaint_inspect::digest::digest(&ws.project, &id, &assets, &opts)?;
    let (marked, _legend) = dpaint_inspect::annotate::annotate(&base, &digest)?;
    std::fs::write(&annotated, encode_png(&marked)?)?;

    Ok(json!({ "png": path_str(&png)?, "annotated": path_str(&annotated)? }))
}

fn encode_png(pm: &tiny_skia::Pixmap) -> Result<Vec<u8>> {
    dpaint_render::encode::encode(&dpaint_render::encode::to_rgba(pm), ImageFormat::Png, 100)
}

// --------------------------------------------------------------------------- user dirs

/// `~/Movies/degen-paint/<project>` mirrors DMS's hand-off folder, which is where the
/// other three apps look. A user who has moved their videos elsewhere is followed there.
pub fn videos_dir() -> PathBuf {
    user_dir("XDG_VIDEOS_DIR", "Movies")
}

pub fn pictures_dir() -> PathBuf {
    user_dir("XDG_PICTURES_DIR", "Pictures")
}

fn user_dir(key: &str, fallback: &str) -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    if let Some(v) = std::env::var_os(key).filter(|v| !v.is_empty()) {
        return expand(&v.to_string_lossy(), &home);
    }
    match user_dirs_entry(key) {
        Some(v) => expand(&v, &home),
        None => home.join(fallback),
    }
}

fn user_dirs_entry(key: &str) -> Option<String> {
    let text = std::fs::read_to_string(crate::recent::config_dir()?.join("user-dirs.dirs")).ok()?;
    parse_user_dirs(&text, key)
}

/// `user-dirs.dirs` is shell assignments: `XDG_VIDEOS_DIR="$HOME/Videos"`.
fn parse_user_dirs(text: &str, key: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| l.split_once('='))
        .find(|(k, _)| k.trim() == key)
        .map(|(_, v)| v.trim().trim_matches('"').to_string())
}

fn expand(value: &str, home: &Path) -> PathBuf {
    match value
        .strip_prefix("$HOME/")
        .or_else(|| value.strip_prefix("~/"))
    {
        Some(rest) => home.join(rest),
        None => PathBuf::from(value),
    }
}

// ----------------------------------------------------------------------------- plumbing

fn revision(ws: &mut Workspace) -> Result<u64> {
    Ok(ws.journal.load()?.last().map(|e| e.seq).unwrap_or(0))
}

fn flag(params: &Value, key: &str) -> bool {
    params.get(key).and_then(|v| v.as_bool()).unwrap_or(false)
}

fn stem(path: &Path) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("imported")
        .to_string()
}

/// Ops and the renderer take paths as strings; a path that is not UTF-8 cannot round-trip
/// through JSON either, so it is refused here rather than mangled.
fn path_str(path: &Path) -> Result<&str> {
    path.to_str().ok_or_else(|| {
        Error::Invalid(format!(
            "path {} is not valid UTF-8",
            path.to_string_lossy()
        ))
    })
}

fn slug(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let s = s.trim_matches('-').to_string();
    if s.is_empty() {
        "untitled".into()
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sidecar_becomes_provenance_and_an_unrelated_json_does_not() {
        let side = json!({
            "prompt": "a loud microphone",
            "model": "flux/dev",
            "parents": ["take_7", "take_2"],
            "cost": 0.031,
            "unknown": { "whatever": true }
        });
        let p = provenance_of(&side).expect("a DMS sidecar carries provenance");
        assert_eq!(p.prompt.as_deref(), Some("a loud microphone"));
        assert_eq!(p.model, "flux/dev");
        assert_eq!(p.parents, vec!["take_7", "take_2"]);
        assert_eq!(p.cost_usd, Some(0.031));
        assert_eq!(
            p.provider, "import",
            "an unattributed file is still an import"
        );

        assert!(
            provenance_of(&json!({ "width": 512, "height": 512 })).is_none(),
            "a sidecar with nothing to say must not stamp an empty record"
        );
    }

    #[test]
    fn user_dirs_are_followed_when_the_desktop_defines_them() {
        let dirs = "# generated by xdg-user-dirs-update\n\
                    XDG_VIDEOS_DIR=\"$HOME/Media/Video\"\n\
                    #XDG_PICTURES_DIR=\"$HOME/Commented Out\"\n";

        assert_eq!(
            parse_user_dirs(dirs, "XDG_VIDEOS_DIR").as_deref(),
            Some("$HOME/Media/Video")
        );
        assert_eq!(
            parse_user_dirs(dirs, "XDG_PICTURES_DIR"),
            None,
            "a commented-out entry is not a definition, so the default stands"
        );
        assert_eq!(
            expand("$HOME/Media/Video", Path::new("/home/x")),
            PathBuf::from("/home/x/Media/Video")
        );
        assert_eq!(
            expand("/mnt/media", Path::new("/home/x")),
            PathBuf::from("/mnt/media"),
            "an absolute entry is taken as it stands"
        );
    }
}
