//! `dpaint-view` — an interactive GPU viewport onto a degen-paint project.

use anyhow::{bail, Context};
use clap::Parser;
use dpaint_core::Workspace;
use dpaint_gpu::Gpu;
use dpaint_view::viewport::parse_size;
use dpaint_view::{app, headless, require_gpu, NO_ADAPTER_EXIT};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "dpaint-view",
    about = "Interactive GPU viewport for a degen-paint project",
    long_about = "Open a degen-paint project in a GPU-driven window.\n\n\
Model documents orbit: drag to turn, shift-drag or right-drag to pan, scroll to zoom,\n\
`f` frames the subject and `1` resets the view. Raster and vector documents are\n\
rasterized once by the engine and then panned and zoomed on the GPU.\n\n\
The window follows the project's journal, so an op applied by an agent, the CLI or the\n\
Studio shows up here within half a second.\n\n\
The viewport is not authoritative: `dpaint render` is, and it needs no GPU."
)]
struct Args {
    /// Project directory (a `.dpaint` folder holding project.json).
    #[arg(long, value_name = "DIR")]
    project: PathBuf,

    /// Document to show: id or name. Defaults to the project's active document.
    #[arg(long, value_name = "ID|NAME")]
    doc: Option<String>,

    /// Render N frames offscreen and exit, instead of opening a window.
    #[arg(long, value_name = "N", requires = "out")]
    frames: Option<u32>,

    /// Directory for the PNGs written by --frames.
    #[arg(long, value_name = "DIR")]
    out: Option<PathBuf>,

    /// Window or frame size.
    #[arg(long, default_value = "1280x800", value_name = "WxH")]
    size: String,

    /// Multisampling for model documents; 1 disables it.
    #[arg(long, default_value_t = 4, value_name = "N")]
    msaa: u32,

    /// Physical pixels of simulated drag between headless frames.
    #[arg(long, default_value_t = 24.0, value_name = "PX")]
    drag: f32,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let size = parse_size(&args.size).map_err(|e| anyhow::anyhow!("--size {e}"))?;

    // No GPU is not a failure of degen-paint, but it is a failure of *this* binary, and
    // saying so beats opening a black window.
    let gpu = match require_gpu(Gpu::block_new()) {
        Ok(g) => g,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(NO_ADAPTER_EXIT);
        }
    };

    match (args.frames, args.out) {
        (Some(frames), Some(out)) => {
            if frames == 0 {
                bail!("--frames must be at least 1");
            }
            let ws = Workspace::open(&args.project)
                .with_context(|| format!("opening {}", args.project.display()))?;
            let doc = headless::resolve_doc(&ws, args.doc.as_deref())?;
            let opts = headless::Options {
                frames,
                out,
                size,
                samples: args.msaa,
                drag_px: args.drag,
                status: true,
            };
            let report = headless::run(&gpu, &ws, &doc, &opts)?;
            let info = gpu.info();
            println!(
                "{} frames of {} ({:?}) at {}x{}, {}x MSAA, on {} · {}",
                report.paths.len(),
                doc,
                report.mode,
                report.size[0],
                report.size[1],
                report.samples,
                info.backend,
                info.name
            );
            println!(
                "draw {:.2} ms median, {:.2} ms mean, {:.2} ms worst — {:.0} fps",
                report.median_draw_ms(),
                report.mean_draw_ms(),
                report.worst_draw_ms(),
                report.fps()
            );
            println!(
                "uploads: {} geometry, {} pixmap (a drag must not move these)",
                report.geometry_uploads, report.canvas_uploads
            );
            for p in &report.paths {
                println!("{}", p.display());
            }
            Ok(())
        }
        _ => {
            let app = app::ViewApp::new(gpu, args.project, args.doc.as_deref(), args.msaa)?;
            app::run(app)
        }
    }
}
