//! Rendering: one place that turns any document into pixels or a file.
//!
//! The Tauri viewport, `dpaint render`, the MCP `dpaint_render` tool and the golden tests all
//! come through here, so what a human sees and what an agent measures cannot diverge.

pub mod encode;
pub mod preview3d;

use dpaint_core::doc::Document;
use dpaint_core::{
    parse_args, schema_for, AssetStore, Color, DocId, Error, Op, OpCx, OpEffect, Project, Result,
};
use serde::Deserialize;
use tiny_skia::Pixmap;

pub use encode::ImageFormat;

#[derive(Debug, Clone)]
pub struct RenderOptions {
    /// Multiplier on the document's natural size.
    pub scale: f64,
    /// Explicit output size; overrides `scale` when set.
    pub size: Option<(u32, u32)>,
    /// Background painted under the document. `None` keeps transparency.
    pub background: Option<Color>,
    pub camera: preview3d::Camera,
    pub lighting: preview3d::Lighting,
    /// Guard against a typo turning into a 40 GB allocation.
    pub max_pixels: u64,
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self {
            scale: 1.0,
            size: None,
            background: None,
            camera: preview3d::Camera::default(),
            lighting: preview3d::Lighting::default(),
            max_pixels: 256_000_000,
        }
    }
}

/// Natural pixel size of a document at a given scale. Model documents have no intrinsic
/// 2D extent, so they get a square preview.
pub fn natural_size(doc: &Document, opts: &RenderOptions) -> (u32, u32) {
    if let Some((w, h)) = opts.size {
        return (w.max(1), h.max(1));
    }
    match doc.size() {
        Some((w, h)) => (
            ((w * opts.scale).round() as u32).max(1),
            ((h * opts.scale).round() as u32).max(1),
        ),
        None => {
            let side = ((1024.0 * opts.scale).round() as u32).max(1);
            (side, side)
        }
    }
}

/// Render any document to a pixmap, recursing through linked documents.
pub fn render_document(
    project: &Project,
    doc: &DocId,
    assets: &AssetStore,
    opts: &RenderOptions,
) -> Result<Pixmap> {
    render_inner(project, doc, assets, opts, &std::cell::RefCell::new(Vec::new()))
}

fn render_inner(
    project: &Project,
    doc_id: &DocId,
    assets: &AssetStore,
    opts: &RenderOptions,
    stack: &std::cell::RefCell<Vec<DocId>>,
) -> Result<Pixmap> {
    {
        let s = stack.borrow();
        if s.contains(doc_id) {
            return Err(Error::CyclicLink {
                from: s.last().map(|d| d.to_string()).unwrap_or_default(),
                to: doc_id.to_string(),
            });
        }
    }
    let document = project.doc(doc_id)?;
    let (w, h) = natural_size(document, opts);
    if (w as u64) * (h as u64) > opts.max_pixels {
        return Err(Error::Invalid(format!(
            "refusing to render {w}x{h} ({} px) above the {} px ceiling",
            (w as u64) * (h as u64),
            opts.max_pixels
        )));
    }

    stack.borrow_mut().push(doc_id.clone());
    let result = match document {
        Document::Raster(_) => {
            let link = |target: &DocId, tw: u32, th: u32| -> Result<Pixmap> {
                let sub = RenderOptions {
                    scale: 1.0,
                    size: Some((tw.max(1), th.max(1))),
                    background: None,
                    ..opts.clone()
                };
                render_inner(project, target, assets, &sub, stack)
            };
            dpaint_raster::render_doc(project, doc_id, assets, opts.scale, &link)
        }
        Document::Vector(_) => {
            let scale = match opts.size {
                Some((tw, _)) => {
                    let (nw, _) = document.size().unwrap_or((tw as f64, 1.0));
                    if nw > 0.0 { tw as f64 / nw } else { opts.scale }
                }
                None => opts.scale,
            };
            dpaint_vector::render_doc(project, doc_id, assets, scale)
        }
        Document::Model(_) => render_model(project, doc_id, assets, w, h, opts),
    };
    stack.borrow_mut().pop();

    let mut pm = result?;
    if let Some(bg) = opts.background {
        pm = with_background(&pm, bg)?;
    }
    Ok(pm)
}

fn render_model(
    project: &Project,
    doc_id: &DocId,
    assets: &AssetStore,
    w: u32,
    h: u32,
    opts: &RenderOptions,
) -> Result<Pixmap> {
    let doc = project.model(doc_id)?;
    let drawables = dpaint_model3d::scene_meshes(project, doc_id, assets)?;
    let meshes: Vec<preview3d::Mesh> = drawables
        .into_iter()
        .map(|(_node, data, material, world)| {
            let m = material.and_then(|id| doc.material(&id));
            preview3d::Mesh {
                positions: data.positions,
                normals: data.normals,
                indices: data.indices,
                world,
                base_color: m.map(|m| m.base_color).unwrap_or(Color::parse("#cccccc").unwrap()),
                metallic: m.map(|m| m.metallic).unwrap_or(0.0),
                roughness: m.map(|m| m.roughness).unwrap_or(0.5),
            }
        })
        .collect();
    preview3d::render(&meshes, w, h, opts.camera, opts.lighting, None)
}

fn with_background(src: &Pixmap, bg: Color) -> Result<Pixmap> {
    let mut out = Pixmap::new(src.width(), src.height())
        .ok_or_else(|| Error::Invalid("zero-size render".into()))?;
    let c = bg.to_rgba8();
    out.fill(tiny_skia::Color::from_rgba8(c[0], c[1], c[2], c[3]));
    out.draw_pixmap(
        0,
        0,
        src.as_ref(),
        &tiny_skia::PixmapPaint::default(),
        tiny_skia::Transform::identity(),
        None,
    );
    Ok(out)
}

/// A turntable: `frames` evenly spaced yaw steps around the subject.
pub fn turntable(
    project: &Project,
    doc: &DocId,
    assets: &AssetStore,
    frames: u32,
    opts: &RenderOptions,
) -> Result<Vec<Pixmap>> {
    let frames = frames.clamp(1, 360);
    (0..frames)
        .map(|i| {
            let mut o = opts.clone();
            o.camera.yaw = opts.camera.yaw + 360.0 * i as f32 / frames as f32;
            render_document(project, doc, assets, &o)
        })
        .collect()
}

/// What an export produced, so the caller can report bytes written without re-stat-ing.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Export {
    pub path: String,
    pub bytes: usize,
    pub format: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<[u32; 2]>,
}

/// Export a document to a file, choosing the pipeline from the extension:
/// `svg` from the vector engine, `glb`/`gltf` from the model engine, everything else
/// through the raster encoders.
pub fn export_document(
    project: &Project,
    doc: &DocId,
    assets: &AssetStore,
    path: &str,
    opts: &RenderOptions,
    quality: u8,
) -> Result<Export> {
    let ext = path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    if let Some(parent) = std::path::Path::new(path).parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }

    match ext.as_str() {
        "svg" => {
            let svg = dpaint_vector::to_svg(project, doc)?;
            std::fs::write(path, svg.as_bytes())?;
            Ok(Export { path: path.into(), bytes: svg.len(), format: "svg".into(), size: None })
        }
        "glb" | "gltf" => {
            let textures = |d: &DocId| -> Result<Vec<u8>> {
                let pm = render_document(project, d, assets, &RenderOptions::default())?;
                encode::encode(&encode::to_rgba(&pm), ImageFormat::Png, 100)
            };
            let out = dpaint_model3d::export(project, doc, assets, &textures)?;
            let bytes = if ext == "glb" { out.glb } else { out.json.into_bytes() };
            std::fs::write(path, &bytes)?;
            if ext == "gltf" && !out.bin.is_empty() {
                let bin_path = std::path::Path::new(path).with_extension("bin");
                std::fs::write(bin_path, &out.bin)?;
            }
            Ok(Export { path: path.into(), bytes: bytes.len(), format: ext, size: None })
        }
        _ => {
            let format = ImageFormat::from_path(path)?;
            let pm = render_document(project, doc, assets, opts)?;
            let mut img = encode::to_rgba(&pm);
            if !format.has_alpha() {
                img = encode::flatten(&img, opts.background.unwrap_or(Color::WHITE));
            }
            let bytes = encode::encode(&img, format, quality)?;
            std::fs::write(path, &bytes)?;
            Ok(Export {
                path: path.into(),
                bytes: bytes.len(),
                format: format.ext().into(),
                size: Some([pm.width(), pm.height()]),
            })
        }
    }
}

pub fn ops() -> Vec<Box<dyn Op>> {
    vec![Box::new(RenderImage), Box::new(RenderTurntable)]
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct RenderImageArgs {
    /// Output path. The extension selects the format: png, jpg, webp, tiff, svg, glb, gltf.
    pub path: String,
    /// Document to render; defaults to the active one.
    #[serde(default)]
    pub document: Option<String>,
    /// Multiplier on the document's natural size.
    #[serde(default = "one")]
    pub scale: f64,
    /// Explicit width in px; overrides scale.
    #[serde(default)]
    pub width: Option<u32>,
    #[serde(default)]
    pub height: Option<u32>,
    /// Background color; transparent when omitted (JPEG falls back to white).
    #[serde(default)]
    pub background: Option<Color>,
    /// Quality for lossy formats.
    #[serde(default = "ninety")]
    pub quality: u8,
}

fn one() -> f64 {
    1.0
}
fn ninety() -> u8 {
    90
}

pub struct RenderImage;

impl Op for RenderImage {
    fn id(&self) -> &'static str {
        "render.image"
    }
    fn about(&self) -> &'static str {
        "Render a document to a file; format is chosen by the extension"
    }
    fn schema(&self) -> serde_json::Value {
        schema_for::<RenderImageArgs>()
    }
    fn is_query(&self) -> bool {
        true
    }
    fn apply(&self, p: &mut Project, args: serde_json::Value, cx: &mut OpCx) -> Result<OpEffect> {
        let a: RenderImageArgs = parse_args(self.id(), args)?;
        let doc = match &a.document {
            Some(d) => p.resolve_doc(Some(d))?,
            None => cx.target_doc(p)?,
        };
        let opts = RenderOptions {
            scale: a.scale,
            size: match (a.width, a.height) {
                (Some(w), Some(h)) => Some((w, h)),
                (Some(w), None) => {
                    let (nw, nh) = p.doc(&doc)?.size().unwrap_or((w as f64, w as f64));
                    Some((w, ((w as f64) * nh / nw).round() as u32))
                }
                (None, Some(h)) => {
                    let (nw, nh) = p.doc(&doc)?.size().unwrap_or((h as f64, h as f64));
                    Some((((h as f64) * nw / nh).round() as u32, h))
                }
                (None, None) => None,
            },
            background: a.background,
            ..Default::default()
        };
        if cx.dry_run {
            let (w, h) = natural_size(p.doc(&doc)?, &opts);
            return Ok(OpEffect::default()
                .with_data(serde_json::json!({ "path": a.path, "size": [w, h], "written": false })));
        }
        let out = export_document(p, &doc, cx.assets, &a.path, &opts, a.quality)?;
        Ok(OpEffect::default().with_data(serde_json::to_value(out)?))
    }
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct TurntableArgs {
    /// Directory for the frames.
    pub dir: String,
    #[serde(default)]
    pub document: Option<String>,
    #[serde(default = "eight")]
    pub frames: u32,
    #[serde(default = "five_twelve")]
    pub width: u32,
    #[serde(default = "five_twelve")]
    pub height: u32,
    #[serde(default)]
    pub background: Option<Color>,
    /// Camera elevation in degrees.
    #[serde(default = "twenty")]
    pub pitch: f32,
}

fn eight() -> u32 {
    8
}
fn five_twelve() -> u32 {
    512
}
fn twenty() -> f32 {
    20.0
}

pub struct RenderTurntable;

impl Op for RenderTurntable {
    fn id(&self) -> &'static str {
        "render.turntable"
    }
    fn about(&self) -> &'static str {
        "Render a model document as evenly spaced orbit frames"
    }
    fn schema(&self) -> serde_json::Value {
        schema_for::<TurntableArgs>()
    }
    fn modes(&self) -> &'static [dpaint_core::DocKind] {
        &[dpaint_core::DocKind::Model]
    }
    fn is_query(&self) -> bool {
        true
    }
    fn apply(&self, p: &mut Project, args: serde_json::Value, cx: &mut OpCx) -> Result<OpEffect> {
        let a: TurntableArgs = parse_args(self.id(), args)?;
        let doc = match &a.document {
            Some(d) => p.resolve_doc(Some(d))?,
            None => cx.target_doc(p)?,
        };
        let mut opts = RenderOptions {
            size: Some((a.width, a.height)),
            background: a.background,
            ..Default::default()
        };
        opts.camera.pitch = a.pitch;

        if cx.dry_run {
            return Ok(OpEffect::default()
                .with_data(serde_json::json!({ "frames": a.frames, "written": false })));
        }
        std::fs::create_dir_all(&a.dir)?;
        let frames = turntable(p, &doc, cx.assets, a.frames, &opts)?;
        let mut written = Vec::new();
        for (i, pm) in frames.iter().enumerate() {
            let path = format!("{}/frame_{i:03}.png", a.dir.trim_end_matches('/'));
            let bytes = encode::encode(&encode::to_rgba(pm), ImageFormat::Png, 100)?;
            std::fs::write(&path, bytes)?;
            written.push(path);
        }
        Ok(OpEffect::default().with_data(serde_json::json!({ "frames": written })))
    }
}
