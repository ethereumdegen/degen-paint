//! degen-paint core: the document model, the op registry, and the machinery every other
//! crate derives from.
//!
//! The shape of the system:
//!
//! - [`Project`] is the canonical JSON document set — the single source of truth.
//! - [`Document`] is one of three kinds: raster, vector, model. They live in one project
//!   and reference each other, which is what makes the cross-mode bridges possible.
//! - [`op::Registry`] holds every mutation as a schema-carrying op. The CLI, the MCP server
//!   and the GUI are all derived from it, so the surfaces cannot drift.
//! - [`engine::Engine`] applies ops transactionally, journals them as RFC-6902 patches, and
//!   saves atomically.

pub mod asset;
pub mod color;
pub mod doc;
pub mod engine;
pub mod error;
pub mod ids;
pub mod journal;
pub mod op;
pub mod ops;
pub mod project;
pub mod selector;
pub mod text;

pub use asset::{AssetRef, AssetStore};
pub use color::Color;
pub use doc::{DocKind, Document, ModelDoc, RasterDoc, VectorDoc};
pub use engine::Engine;
pub use error::{Error, Result};
pub use ids::*;
pub use journal::{Actor, Journal};
pub use op::{parse_args, schema_for, Op, OpCx, OpEffect, Registry, Warning};
pub use project::{now_iso, Project, Workspace, FORMAT_VERSION};
pub use selector::{resolve, resolve_one, Match, Selector};
pub use text::{FALLBACK_FAMILY, FALLBACK_FONT};

/// Re-exported so mode crates share one geometry type without a version skew.
pub use kurbo;
