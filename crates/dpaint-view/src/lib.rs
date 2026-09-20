//! The native interactive viewport: a window onto a degen-paint project, drawn by
//! `dpaint-gpu`.
//!
//! This is a *viewport*, not a second renderer. `dpaint_render` stays authoritative for
//! `dpaint render`, turntables, goldens and diffs; what lives here is the part the CPU
//! cannot do — redrawing a scene sixty times a second while somebody drags the mouse.
//!
//! The crate is split so that everything with a right answer is a pure function in this
//! library and the binary is only plumbing:
//!
//! - [`camera::OrbitCamera`] — orbit/pan/zoom reduction, producing a
//!   [`dpaint_render::preview3d::Camera`] the GPU and the CPU renderer both understand.
//! - [`canvas::CanvasView`] — the 2D equivalent: fit, cursor-anchored zoom, pan.
//! - [`input`] — window events mapped to intent, with no `winit` types in the signatures.
//! - [`reload`] — the journal poll that makes an agent's edit show up in the window.
//! - [`status`] — the text of the on-window readout.
//! - [`subject`] — a document turned into something drawable.
//! - [`render`] — one draw path, used by both the window and `--frames`.

pub mod camera;
pub mod canvas;
pub mod headless;
pub mod input;
pub mod overlay;
pub mod reload;
pub mod render;
pub mod status;
pub mod subject;
pub mod target;
pub mod text;
pub mod viewport;

pub mod app;

pub use camera::OrbitCamera;
pub use canvas::CanvasView;
pub use input::{Action, Button, Drag, Mode, Mods};
pub use reload::JournalWatch;
pub use render::ViewRenderer;
pub use subject::Subject;
pub use viewport::Viewport;

/// What the binary prints, and what every entry point degrades to, when the machine has no
/// usable GPU adapter. The CPU renderer can still produce the same image — just not at
/// sixty frames a second.
pub const NO_ADAPTER_MESSAGE: &str = "\
no GPU adapter is available, so the interactive viewport cannot open.

degen-paint does not require a GPU: the CPU renderer produces the same picture, and it is
the authoritative one. Render this project from the command line instead:

    dpaint render preview.png --project <project.dpaint> --doc <id|name>
    dpaint op render.turntable --project <project.dpaint> --document <id|name> --frames 8 --dir turn/

`dpaint doctor` reports what this machine can do.";

/// Exit status when there is no adapter. Distinct from 1 so a script can tell "this
/// machine cannot run the viewport" from "that project is broken".
pub const NO_ADAPTER_EXIT: i32 = 2;

/// Turn the optional device into either a usable GPU or the message the user needs.
///
/// The whole no-GPU policy lives here: a missing adapter is reported, in words, with the
/// CPU command that does the same job — never a panic, never a black window.
pub fn require_gpu(gpu: Option<dpaint_gpu::Gpu>) -> std::result::Result<dpaint_gpu::Gpu, String> {
    gpu.ok_or_else(|| NO_ADAPTER_MESSAGE.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_machine_with_no_adapter_is_told_which_cpu_command_to_run() {
        let Err(err) = require_gpu(None) else {
            panic!("no adapter must not yield a device");
        };
        assert!(
            err.contains("dpaint render"),
            "the fallback must name the CPU renderer: {err}"
        );
        assert!(
            err.contains("does not require a GPU"),
            "the message must say a GPU is optional: {err}"
        );
        assert!(
            !err.to_lowercase().contains("panic"),
            "it is a supported configuration, not a crash: {err}"
        );
    }
}
