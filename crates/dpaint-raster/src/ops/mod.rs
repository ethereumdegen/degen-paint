//! The `raster.*` op catalog. One struct per op, ids exactly as in `docs/op-registry.md`.

pub mod adjust_ops;
pub mod canvas_ops;
pub mod effect_ops;
pub mod filter_ops;
pub mod layer_ops;
pub mod mask_ops;
pub mod paint_ops;
pub mod select_ops;
pub mod support;
pub mod text_ops;

pub fn all() -> Vec<Box<dyn dpaint_core::Op>> {
    let mut out = Vec::new();
    out.extend(canvas_ops::ops());
    out.extend(layer_ops::ops());
    out.extend(mask_ops::ops());
    out.extend(adjust_ops::ops());
    out.extend(filter_ops::ops());
    out.extend(select_ops::ops());
    out.extend(paint_ops::ops());
    out.extend(text_ops::ops());
    out.extend(effect_ops::ops());
    out
}
