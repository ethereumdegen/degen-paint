//! Shared op plumbing: selector resolution, the validate-then-mutate pixel edit path, and
//! the `raster_op!` macro that turns one function into a schema-carrying op.

use crate::canvas::Canvas;
use crate::composite;
use crate::select::{self, SelMask};
use dpaint_core::doc::raster::{Layer, LayerKind};
use dpaint_core::{
    resolve, resolve_one, AssetRef, AssetStore, DocId, Error, LayerId, OpCx, RasterDoc, Result,
};

/// Declares one op: a unit struct plus its `Op` impl, delegating to `$body`.
macro_rules! raster_op {
    ($struct:ident, $id:literal, $about:literal, $args:ty, $body:path) => {
        pub struct $struct;

        impl dpaint_core::Op for $struct {
            fn id(&self) -> &'static str {
                $id
            }
            fn about(&self) -> &'static str {
                $about
            }
            fn schema(&self) -> serde_json::Value {
                dpaint_core::schema_for::<$args>()
            }
            fn modes(&self) -> &'static [dpaint_core::DocKind] {
                &[dpaint_core::DocKind::Raster]
            }
            fn apply(
                &self,
                project: &mut dpaint_core::Project,
                args: serde_json::Value,
                cx: &mut dpaint_core::OpCx,
            ) -> dpaint_core::Result<dpaint_core::OpEffect> {
                let parsed: $args = dpaint_core::parse_args($id, args)?;
                $body(project, parsed, cx)
            }
        }
    };
}
pub(crate) use raster_op;

/// Whether an edit honors the document's current selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Scope {
    /// Clip the edit to the current selection (the default; no selection means everything).
    #[default]
    Selection,
    /// Ignore the current selection and edit the whole layer.
    Whole,
}

/// The document an op targets: explicit `--doc`, else the project's active document.
/// Errors if it is not a raster document, which is the whole point of `modes()`.
pub fn doc_id(project: &dpaint_core::Project, cx: &OpCx) -> Result<DocId> {
    let id = cx.target_doc(project)?;
    let kind = project.doc(&id)?.kind();
    if kind != dpaint_core::DocKind::Raster {
        return Err(Error::WrongDocumentKind {
            op: "raster.*".into(),
            kind: kind.to_string(),
        });
    }
    Ok(id)
}

/// Resolve a selector to exactly one layer.
pub fn one_layer(project: &dpaint_core::Project, cx: &OpCx, sel: &str) -> Result<(DocId, LayerId)> {
    let doc = doc_id(project, cx)?;
    let m = resolve_one(project, sel, Some(&doc))?;
    Ok((m.document, LayerId::from(m.id)))
}

/// Resolve a selector to every matching layer, in z-order.
pub fn many_layers(
    project: &dpaint_core::Project,
    cx: &OpCx,
    sel: &str,
) -> Result<(DocId, Vec<LayerId>)> {
    let doc = doc_id(project, cx)?;
    let ms = resolve(project, sel, Some(&doc))?;
    let ids = ms.iter().map(|m| LayerId::from(m.id.clone())).collect();
    Ok((doc, ids))
}

pub fn layer_of<'a>(rd: &'a RasterDoc, id: &LayerId) -> Result<&'a Layer> {
    rd.layer(id).ok_or_else(|| Error::SelectorNoMatch {
        selector: format!("#{id}"),
        doc: rd.id.to_string(),
        candidates: rd.walk().iter().map(|l| l.id.to_string()).collect(),
    })
}

/// The selection mask for an edit, at document resolution.
pub fn selection_mask(
    rd: &RasterDoc,
    assets: &AssetStore,
    scope: Scope,
) -> Result<Option<SelMask>> {
    if scope == Scope::Whole {
        return Ok(None);
    }
    select::resolve(rd, assets, rd.width(), rd.height(), 1.0)
}

/// A pixel layer's blob as a canvas, plus its offset.
pub fn load_pixel(rd: &RasterDoc, id: &LayerId, assets: &AssetStore) -> Result<(Canvas, [i32; 2])> {
    let layer = layer_of(rd, id)?;
    match &layer.kind {
        LayerKind::Pixel { asset, offset } => Ok((Canvas::from_png(&assets.get(asset)?)?, *offset)),
        other => Err(Error::Invalid(format!(
            "'{}' is a {} layer; run raster.layer.rasterize on it first",
            layer.name,
            kind_name(other)
        ))),
    }
}

fn kind_name(k: &LayerKind) -> &'static str {
    match k {
        LayerKind::Pixel { .. } => "pixel",
        LayerKind::Fill { .. } => "fill",
        LayerKind::Gradient { .. } => "gradient",
        LayerKind::Text { .. } => "text",
        LayerKind::Shape { .. } => "shape",
        LayerKind::Adjustment { .. } => "adjustment",
        LayerKind::Group { .. } => "group",
        LayerKind::Linked { .. } => "linked",
    }
}

pub fn store_canvas(assets: &AssetStore, c: &Canvas) -> Result<AssetRef> {
    assets.put(&c.to_png()?, "png")
}

/// The validate-then-mutate pixel path every filter, adjustment and paint op goes through.
///
/// `f` sees the layer's current pixels and returns the candidate result at the same size.
/// The candidate is composited through the selection, written to the asset store as a **new**
/// blob, and the layer is repointed — the old blob is never touched, which is exactly what
/// makes undo a pure JSON patch.
pub fn edit_pixels(
    project: &mut dpaint_core::Project,
    doc: &DocId,
    id: &LayerId,
    cx: &mut OpCx,
    scope: Scope,
    f: impl FnOnce(&Canvas) -> Result<Canvas>,
) -> Result<AssetRef> {
    let rd = project.raster(doc)?;
    let (old, offset) = load_pixel(rd, id, cx.assets)?;
    let sel = selection_mask(rd, cx.assets, scope)?;
    let new = f(&old)?;
    if new.width != old.width || new.height != old.height {
        return Err(Error::Invalid(format!(
            "filter changed the layer size from {}x{} to {}x{}",
            old.width, old.height, new.width, new.height
        )));
    }
    let out = select::composite_through(
        &old,
        &new,
        sel.as_ref(),
        (offset[0] as i64, offset[1] as i64),
    );
    let asset = store_canvas(cx.assets, &out)?;
    if !cx.dry_run {
        let rd = project.raster_mut(doc)?;
        if let Some(LayerKind::Pixel { asset: slot, .. }) = rd.layer_mut(id).map(|l| &mut l.kind) {
            *slot = asset.clone();
        }
    }
    Ok(asset)
}

/// Replace a pixel layer's blob and offset outright (canvas ops, transforms).
pub fn set_pixels(
    rd: &mut RasterDoc,
    id: &LayerId,
    asset: AssetRef,
    offset: [i32; 2],
) -> Result<()> {
    let Some(layer) = rd.layer_mut(id) else {
        return Err(Error::Invalid(format!("layer {id} vanished")));
    };
    layer.kind = LayerKind::Pixel { asset, offset };
    Ok(())
}

/// Path to the sibling list containing `id`: the group indices to descend, plus the index
/// of `id` within that list. Reorder, group and ungroup all need the container, not the
/// layer, and a path keeps that borrow-free.
pub fn locate(rd: &RasterDoc, id: &LayerId) -> Option<(Vec<usize>, usize)> {
    fn rec(ls: &[Layer], id: &LayerId, path: &mut Vec<usize>) -> Option<usize> {
        if let Some(i) = ls.iter().position(|l| &l.id == id) {
            return Some(i);
        }
        for (idx, l) in ls.iter().enumerate() {
            if let LayerKind::Group { layers } = &l.kind {
                path.push(idx);
                if let Some(i) = rec(layers, id, path) {
                    return Some(i);
                }
                path.pop();
            }
        }
        None
    }
    let mut path = Vec::new();
    let i = rec(&rd.layers, id, &mut path)?;
    Some((path, i))
}

/// The sibling list a [`locate`] path points at.
pub fn list_at<'a>(rd: &'a mut RasterDoc, path: &[usize]) -> Option<&'a mut Vec<Layer>> {
    let mut list = &mut rd.layers;
    for step in path {
        let layer = list.get_mut(*step)?;
        match &mut layer.kind {
            LayerKind::Group { layers } => list = layers,
            _ => return None,
        }
    }
    Some(list)
}

/// Insert a layer at the top of a group (or of the root list when `parent` is `None`).
pub fn insert_layer(
    rd: &mut RasterDoc,
    parent: Option<&LayerId>,
    index: Option<usize>,
    layer: Layer,
) -> Result<()> {
    let list = match parent {
        None => &mut rd.layers,
        Some(pid) => {
            let Some(p) = rd.layer_mut(pid) else {
                return Err(Error::Invalid(format!("parent layer {pid} not found")));
            };
            match &mut p.kind {
                LayerKind::Group { layers } => layers,
                _ => {
                    return Err(Error::Invalid(format!(
                        "parent '{}' is not a group layer",
                        p.name
                    )))
                }
            }
        }
    };
    let at = index.unwrap_or(list.len()).min(list.len());
    list.insert(at, layer);
    Ok(())
}

/// Composite the whole document at 1:1, for flatten / merge / select-from-composite.
pub fn flatten_canvas(
    project: &dpaint_core::Project,
    doc: &DocId,
    assets: &AssetStore,
) -> Result<Canvas> {
    let link = composite::raster_only_link(project, assets);
    composite::render_canvas(project, doc, assets, 1.0, &link)
}

/// Render exactly one layer at 1:1 with its mask and effects, but *without* its own opacity
/// or blend mode — those belong to whoever composites it.
pub fn layer_content(
    project: &dpaint_core::Project,
    doc: &DocId,
    id: &LayerId,
    assets: &AssetStore,
) -> Result<Canvas> {
    let rd = project.raster(doc)?;
    let layer = layer_of(rd, id)?;
    let link = composite::raster_only_link(project, assets);
    let (w, h) = composite::device_size(rd, 1.0)?;
    let ctx = composite::Ctx {
        project,
        assets,
        scale: 1.0,
        width: w,
        height: h,
        link: &link,
        fonts: crate::text::FontSet::new(project, assets),
        seed: 0,
    };
    composite::render_layer(layer, &ctx)
}

/// Render one layer at 1:1 including its own opacity — what `layer.rasterize` means.
pub fn layer_canvas(
    project: &dpaint_core::Project,
    doc: &DocId,
    id: &LayerId,
    assets: &AssetStore,
) -> Result<Canvas> {
    let opacity = layer_of(project.raster(doc)?, id)?.opacity.clamp(0.0, 1.0);
    let mut content = layer_content(project, doc, id, assets)?;
    if opacity < 1.0 {
        for px in content.data.chunks_exact_mut(4) {
            for v in px.iter_mut() {
                *v *= opacity;
            }
        }
    }
    Ok(content)
}

/// Unique layer id derived from a name, falling back to a generated one on collision.
pub fn fresh_id(rd: &RasterDoc, name: &str) -> LayerId {
    let candidate = LayerId::from_name(name);
    if rd.layer(&candidate).is_none() {
        candidate
    } else {
        LayerId::generate()
    }
}
