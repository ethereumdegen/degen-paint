//! `--frames N --out <dir>`: the viewport, rendered offscreen.
//!
//! Not a second code path — [`crate::render::ViewRenderer`] draws these frames exactly as
//! it draws the window, into a texture instead of a surface. That makes it two useful
//! things at once: a way to see what the viewport looks like on a machine with no
//! display, and a measurement of what a frame costs, since the timing excludes only the
//! readback and the PNG encode.

use dpaint_core::{DocId, Error, Result, Workspace};
use dpaint_gpu::{Camera, Gpu, Lighting};
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::camera::OrbitCamera;
use crate::canvas::CanvasView;
use crate::input::Mode;
use crate::render::ViewRenderer;
use crate::status::{self, Status};
use crate::subject;
use crate::target::OffscreenTarget;

#[derive(Debug, Clone)]
pub struct Options {
    pub frames: u32,
    pub out: PathBuf,
    pub size: [u32; 2],
    pub samples: u32,
    /// Physical pixels of simulated left-drag per frame. Zero renders the same view
    /// repeatedly, which is how you measure a still frame.
    pub drag_px: f32,
    pub status: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            frames: 1,
            out: PathBuf::from("frames"),
            size: [1280, 800],
            samples: 4,
            drag_px: 24.0,
            status: true,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Report {
    pub paths: Vec<PathBuf>,
    /// Wall time for each frame's draw, with the GPU waited to completion. Excludes
    /// readback and PNG encoding, which a window never pays.
    pub draw_ms: Vec<f32>,
    pub mode: Mode,
    pub samples: u32,
    pub size: [u32; 2],
    /// Mesh uploads over the whole run. A drag re-shades; it does not re-upload.
    pub geometry_uploads: u64,
    /// Pixmap uploads over the whole run. Panning and zooming never touch the texture.
    pub canvas_uploads: u64,
}

impl Report {
    pub fn mean_draw_ms(&self) -> f32 {
        if self.draw_ms.is_empty() {
            return 0.0;
        }
        self.draw_ms.iter().sum::<f32>() / self.draw_ms.len() as f32
    }

    /// The typical frame. More useful than the mean on a first run, where frame zero
    /// also pays for pipeline compilation and the first buffer allocations.
    pub fn median_draw_ms(&self) -> f32 {
        if self.draw_ms.is_empty() {
            return 0.0;
        }
        let mut v = self.draw_ms.clone();
        v.sort_by(f32::total_cmp);
        v[v.len() / 2]
    }

    pub fn worst_draw_ms(&self) -> f32 {
        self.draw_ms.iter().copied().fold(0.0, f32::max)
    }

    /// Frames per second the typical measured draw cost implies.
    pub fn fps(&self) -> f32 {
        let m = self.median_draw_ms();
        if m > 0.0 {
            1000.0 / m
        } else {
            0.0
        }
    }
}

/// Frame file name for index `i`, zero-padded so a directory listing sorts correctly.
pub fn frame_name(i: u32) -> String {
    format!("frame_{i:03}.png")
}

/// Status text scale for an offscreen frame.
///
/// A window gets this from the display, but a headless render has no display to ask, so
/// it is derived from the frame height against the default 800 pt window. Without it a
/// 2560x1600 capture carries 13 px text nobody can read.
pub fn status_scale(size: [u32; 2]) -> f32 {
    (size[1] as f32 / 800.0).clamp(1.0, 3.0)
}

pub fn run(gpu: &Gpu, ws: &Workspace, doc: &DocId, opts: &Options) -> Result<Report> {
    let max_texture = gpu.device().limits().max_texture_dimension_2d;
    let mut orbit = OrbitCamera::default();
    let mut canvas = CanvasView::default();

    let mut subject = subject::load(ws, doc, orbit.camera(), Lighting::default(), max_texture)?;
    let mode = subject.mode();
    if let Some(content) = subject.content_size() {
        canvas.fit(content, opts.size);
    }

    let mut renderer = ViewRenderer::new(gpu, OffscreenTarget::FORMAT, opts.samples);
    renderer.set_subject(gpu, &subject);
    let target = OffscreenTarget::new(gpu.device(), opts.size);

    std::fs::create_dir_all(&opts.out)?;

    let document = ws.project.doc(doc)?;
    let info = gpu.info();
    let mut report = Report {
        paths: Vec::with_capacity(opts.frames as usize),
        draw_ms: Vec::with_capacity(opts.frames as usize),
        mode,
        samples: renderer.samples(),
        size: opts.size,
        geometry_uploads: 0,
        canvas_uploads: 0,
    };

    for i in 0..opts.frames {
        if i > 0 {
            // The same reduction the window runs, fed a synthetic drag: the frames are
            // a real interaction, not N copies of one picture.
            match mode {
                Mode::Model => orbit.orbit(opts.drag_px, opts.drag_px * 0.25),
                Mode::Canvas => {
                    canvas.pan_by(opts.drag_px, 0.0);
                    canvas.zoom_at(
                        [opts.size[0] as f32 * 0.5, opts.size[1] as f32 * 0.5],
                        opts.size,
                        0.25,
                    );
                }
            }
        }
        if let Some(scene) = subject.scene_mut() {
            scene.camera = orbit.camera();
            scene.pan = orbit.pan;
        }

        if opts.status {
            let last = report.draw_ms.last().copied().unwrap_or(0.0);
            let lines = status::lines(&Status {
                document: document.name().to_string(),
                kind: document.kind().to_string(),
                mode,
                zoom: match mode {
                    Mode::Model => orbit.zoom,
                    Mode::Canvas => canvas.zoom,
                },
                yaw: orbit.yaw,
                pitch: orbit.pitch,
                pan: match mode {
                    Mode::Model => orbit.pan,
                    Mode::Canvas => canvas.pan,
                },
                content: subject.content_size(),
                frame_ms: last,
                backend: info.backend.clone(),
                adapter: info.name.clone(),
                samples: renderer.samples(),
                reloads: 0,
            });
            renderer.set_status(gpu, &lines, status_scale(opts.size));
        }

        let started = Instant::now();
        renderer.draw(
            gpu,
            target.view(),
            opts.size,
            &subject,
            canvas.view_state(),
            status_scale(opts.size),
        );
        gpu.device()
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(|e| Error::Invalid(format!("waiting for the GPU failed: {e}")))?;
        report
            .draw_ms
            .push(started.elapsed().as_secs_f32() * 1000.0);

        let pm = target
            .read_pixmap(gpu.device(), gpu.queue())
            .ok_or_else(|| Error::Invalid("reading the frame back from the GPU failed".into()))?;
        let path = opts.out.join(frame_name(i));
        save_png(&pm, &path)?;
        report.paths.push(path);
    }

    report.geometry_uploads = renderer.geometry_uploads();
    report.canvas_uploads = renderer.canvas_uploads();
    Ok(report)
}

fn save_png(pm: &tiny_skia::Pixmap, path: &Path) -> Result<()> {
    pm.save_png(path)
        .map_err(|e| Error::Invalid(format!("writing {}: {e}", path.display())))
}

/// Resolve a `--doc` hint against the project, falling back to the active document.
pub fn resolve_doc(ws: &Workspace, hint: Option<&str>) -> Result<DocId> {
    ws.project.resolve_doc(hint)
}

/// The camera a fresh viewport starts from — the same three-quarter view `dpaint render`
/// uses, so a frame from the window and a frame from the CLI are comparable.
pub fn initial_camera() -> Camera {
    OrbitCamera::default().camera()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_names_sort_in_render_order() {
        let mut names: Vec<String> = (0..12).map(frame_name).collect();
        let sorted = {
            let mut c = names.clone();
            c.sort();
            c
        };
        assert_eq!(names, sorted, "zero padding must keep listing order");
        names.dedup();
        assert_eq!(names.len(), 12);
    }

    #[test]
    fn the_headless_status_scales_with_the_frame_so_it_stays_readable() {
        assert_eq!(status_scale([1280, 800]), 1.0);
        assert_eq!(status_scale([2560, 1600]), 2.0);
        assert_eq!(status_scale([320, 200]), 1.0, "never shrink below legible");
        assert_eq!(status_scale([16000, 10000]), 3.0, "and never run away");
    }

    #[test]
    fn a_report_summarises_its_frame_times() {
        let r = Report {
            paths: Vec::new(),
            draw_ms: vec![10.0, 20.0, 30.0],
            mode: Mode::Model,
            samples: 4,
            size: [800, 600],
            geometry_uploads: 1,
            canvas_uploads: 0,
        };
        assert!((r.mean_draw_ms() - 20.0).abs() < 1e-5);
        assert_eq!(r.median_draw_ms(), 20.0);
        assert_eq!(r.worst_draw_ms(), 30.0);
        assert!((r.fps() - 50.0).abs() < 1e-3);
    }

    #[test]
    fn an_empty_report_reports_no_rate_rather_than_dividing_by_zero() {
        let r = Report {
            paths: Vec::new(),
            draw_ms: Vec::new(),
            mode: Mode::Canvas,
            samples: 1,
            size: [1, 1],
            geometry_uploads: 0,
            canvas_uploads: 0,
        };
        assert_eq!(r.mean_draw_ms(), 0.0);
        assert_eq!(r.median_draw_ms(), 0.0);
        assert_eq!(r.fps(), 0.0);
    }
}
