//! dpaint-raster

use dpaint_core::Op;

/// Ops contributed by this crate, registered by the CLI and the MCP server.
pub fn ops() -> Vec<Box<dyn Op>> {
    Vec::new()
}
