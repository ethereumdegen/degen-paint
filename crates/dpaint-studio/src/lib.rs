//! degen-paint Studio: the GUI's access to the engine.
//!
//! [`api::Studio`] is the whole surface, and both shells call it — the Tauri desktop app
//! in-process, and a browser tab through [`server::serve`]. Because it goes through the same
//! [`dpaint_core::Engine`] an agent uses, a human's edits and an agent's edits land on one
//! journal with one undo stack, and either can undo the other.

pub mod api;
pub mod server;

pub use api::{registry, Studio};
pub use server::{serve, ServerConfig};
