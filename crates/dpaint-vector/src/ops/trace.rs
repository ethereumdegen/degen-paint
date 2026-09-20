//! `vector.trace.image` — local, offline raster to vector conversion.

use super::{doc_of, fresh_id};
use crate::geom;
use crate::trace::{TraceMode, TraceOptions};
use crate::vop;
use dpaint_core::asset::AssetRef;
use dpaint_core::doc::common::Paint;
use dpaint_core::doc::vector::{VKind, VObject};
use dpaint_core::error::{Error, Result};
use dpaint_core::kurbo::Affine;
use dpaint_core::project::Project;
use dpaint_core::{Op, OpCx, OpEffect};
use serde::Deserialize;

pub fn ops() -> Vec<Box<dyn Op>> {
    vec![Box::new(TraceImage)]
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct TraceArgs {
    /// Image file to trace. Either this or `asset` is required.
    #[serde(default)]
    pub path: Option<String>,
    /// Asset already in the store, as `blake3:<hash>.<ext>`.
    #[serde(default)]
    pub asset: Option<String>,
    /// binary | color
    #[serde(default)]
    pub mode: TraceMode,
    /// Palette size for `color` mode, 2..=64.
    #[serde(default = "eight")]
    pub colors: u32,
    /// Luminance cut for `binary` mode, 0..1.
    #[serde(default = "half")]
    pub threshold: f64,
    /// Discard regions smaller than this many source pixels.
    #[serde(default = "four")]
    pub speckle: f64,
    /// Turn angle in degrees above which a vertex stays a hard corner.
    #[serde(default = "sixty")]
    pub corner_threshold: f64,
    /// Curve-fitting tolerance in source pixels.
    #[serde(default = "one")]
    pub tolerance: f64,
    /// Where to place the traced result; defaults to the image's pixel size at the origin.
    #[serde(default)]
    pub x: f64,
    #[serde(default)]
    pub y: f64,
    #[serde(default)]
    pub width: Option<f64>,
    #[serde(default)]
    pub height: Option<f64>,
    /// Name for the generated group.
    #[serde(default)]
    pub name: Option<String>,
}

fn eight() -> u32 {
    8
}
fn half() -> f64 {
    0.5
}
fn four() -> f64 {
    4.0
}
fn sixty() -> f64 {
    60.0
}
fn one() -> f64 {
    1.0
}

vop!(TraceImage, TraceArgs, "vector.trace.image", "Trace a raster image into editable paths, entirely offline");

impl TraceImage {
    fn run(project: &mut Project, a: TraceArgs, cx: &mut OpCx) -> Result<OpEffect> {
        let doc = doc_of(project, cx)?;
        let bytes = match (&a.path, &a.asset) {
            (Some(p), _) => std::fs::read(p)?,
            (None, Some(r)) => cx.assets.get(&AssetRef(r.clone()))?,
            (None, None) => {
                return Err(Error::Invalid("pass either --path or --asset".into()));
            }
        };
        let img = image::load_from_memory(&bytes)
            .map_err(|e| Error::AssetDecode(format!("cannot decode the image: {e}")))?
            .to_rgba8();
        let (iw, ih) = img.dimensions();
        let mut pm = tiny_skia::Pixmap::new(iw.max(1), ih.max(1))
            .ok_or_else(|| Error::AssetDecode("zero-sized image".into()))?;
        for (px, dst) in img.pixels().zip(pm.pixels_mut()) {
            let [r, g, b, al] = px.0;
            *dst = tiny_skia::ColorU8::from_rgba(r, g, b, al).premultiply();
        }
        let opts = TraceOptions {
            mode: a.mode,
            colors: a.colors,
            threshold: a.threshold,
            speckle: a.speckle,
            corner_threshold: a.corner_threshold,
            tolerance: a.tolerance,
        };
        let shapes = crate::trace::trace(&pm, &opts)?;
        let sx = a.width.map(|w| w / iw as f64).unwrap_or(1.0);
        let sy = a.height.map(|h| h / ih as f64).unwrap_or(sx);
        let place = Affine::translate((a.x, a.y)) * Affine::scale_non_uniform(sx, sy);

        let v = project.vector_mut(&doc)?;
        let gname = a.name.unwrap_or_else(|| "traced".to_string());
        let gid = fresh_id(v, &gname);
        let mut children = Vec::new();
        for (i, s) in shapes.iter().enumerate() {
            let name = format!("{gname}-{}", i + 1);
            let cid = fresh_id(v, &name);
            let mut o = VObject::new(
                cid,
                name,
                VKind::Path {
                    d: geom::to_d(&(place * s.path.clone())),
                },
            );
            o.fill = Paint::solid(s.color);
            children.push(o);
        }
        let ids: Vec<String> = children.iter().map(|c| c.id.to_string()).collect();
        v.objects.push(VObject::new(
            gid.clone(),
            gname,
            VKind::Group { objects: children },
        ));
        let mut eff = OpEffect::changed(&doc).with_created(gid.to_string());
        for id in ids {
            eff = eff.with_created(id);
        }
        Ok(eff)
    }
}
