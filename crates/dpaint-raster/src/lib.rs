//! degen-paint raster engine: a non-destructive layer compositor and the `raster.*` ops.
//!
//! The shape of the crate:
//!
//! - [`canvas::Canvas`] is the working buffer — premultiplied **linear-light** f32 RGBA.
//!   Pixmaps (premultiplied sRGB u8) are the interchange type at the crate boundary only.
//! - [`composite::render_doc`] walks the layer tree: groups get their own buffer, clipping
//!   masks intersect coverage, layer masks multiply alpha, adjustment layers transform the
//!   backdrop beneath them, and `Linked` layers call the supplied resolver.
//! - [`blend`], [`adjust`], [`filters`], [`effects`], [`paint`], [`select`] and [`text`] are
//!   the pixel kernels; [`ops`] wraps them as schema-carrying ops.
//!
//! Every op that changes pixels writes a **new** blob to the asset store and repoints the
//! layer, because undo is a JSON patch and it depends on the old blob still existing.

pub mod adjust;
pub mod blend;
pub mod canvas;
pub mod composite;
pub mod effects;
pub mod filters;
pub mod geom;
pub mod ops;
pub mod paint;
pub mod select;
pub mod text;

pub use canvas::Canvas;
pub use composite::{render_canvas, render_doc, raster_only_link, LinkResolver};
pub use select::SelMask;

/// Ops contributed by this crate, registered by the CLI and the MCP server.
pub fn ops() -> Vec<Box<dyn dpaint_core::Op>> {
    ops::all()
}
