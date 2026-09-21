//! degen-paint Studio: the GUI's access to the engine.
//!
//! [`api::Studio`] is the whole surface, and both shells call it — the Tauri desktop app
//! in-process, and a browser tab through [`server::serve`]. Because it goes through the same
//! [`dpaint_core::Engine`] an agent uses, a human's edits and an agent's edits land on one
//! journal with one undo stack, and either can undo the other.

pub mod api;
pub mod contract;
pub mod io;
pub mod jobs;
pub mod providers;
pub mod recent;
pub mod server;
pub mod skill;

pub use api::{create_project, registry, Studio};
pub use contract::{Shortcut, CONFIRM_LABEL_PREFIXES, SHORTCUTS};
pub use server::{serve, ServerConfig};
