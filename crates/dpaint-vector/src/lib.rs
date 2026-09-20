//! dpaint-vector: the Bézier document engine.
//!
//! Everything an agent can do to a vector document lands here: shapes and paths resolve to
//! `kurbo::BezPath` through [`path_of`], booleans and path surgery run on that, text shapes
//! through `rustybuzz`, and the document renders to a `tiny_skia::Pixmap` or serializes to
//! SVG. The 3D extruder in `dpaint-model3d` consumes [`path_of`] directly.

pub mod boolean;
pub mod geom;
pub mod ops;
pub mod pathops;
pub mod raster;
pub mod svg;
pub mod text;
pub mod trace;

use dpaint_core::Op;

pub use geom::path_of;


/// Ops contributed by this crate, registered by the CLI and the MCP server.
pub fn ops() -> Vec<Box<dyn Op>> {
    Vec::new()
}
