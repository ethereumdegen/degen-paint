//! The viewport rendered offscreen, through the same `ViewRenderer` the window uses.
//!
//! These are the tests that look at pixels. Everything they assert is a property a human
//! would check by looking at the window: the subject is actually drawn, the status panel
//! is actually legible, dragging actually changes the picture.
//!
//! A machine with no GPU adapter is not a failure — the viewport is a capability, not a
//! requirement — so these skip rather than fail when `Gpu::block_new()` returns `None`.

mod common;

use dpaint_core::DocId;
use dpaint_gpu::Gpu;
use dpaint_view::headless::{self, Options};
use dpaint_view::input::Mode;
use tiny_skia::Pixmap;

fn gpu() -> Option<Gpu> {
    match Gpu::block_new() {
        Some(g) => Some(g),
        None => {
            eprintln!("no GPU adapter on this machine; skipping the offscreen render tests");
            None
        }
    }
}

fn read(path: &std::path::Path) -> Pixmap {
    let bytes = std::fs::read(path).expect("frame file");
    Pixmap::decode_png(&bytes).expect("frame must be a readable PNG")
}

/// How many visibly different colours a frame contains, quantized so antialiasing does
/// not inflate the count.
fn distinct_colors(pm: &Pixmap) -> usize {
    let mut seen = std::collections::HashSet::new();
    for p in pm.pixels() {
        seen.insert((p.red() >> 3, p.green() >> 3, p.blue() >> 3));
    }
    seen.len()
}

fn differing_pixels(a: &Pixmap, b: &Pixmap) -> usize {
    a.pixels()
        .iter()
        .zip(b.pixels())
        .filter(|(x, y)| {
            x.red().abs_diff(y.red()) > 4
                || x.green().abs_diff(y.green()) > 4
                || x.blue().abs_diff(y.blue()) > 4
        })
        .count()
}

#[test]
fn a_model_document_renders_frames_that_change_as_the_camera_orbits() {
    let Some(gpu) = gpu() else { return };
    let (tmp, _root, ws) = common::scratch();
    let out = tmp.path().join("model-frames");
    let doc = DocId::from(common::MODEL_DOC);

    let report = headless::run(
        &gpu,
        &ws,
        &doc,
        &Options {
            frames: 3,
            out: out.clone(),
            size: [640, 400],
            samples: 4,
            drag_px: 60.0,
            status: true,
        },
    )
    .expect("headless render");

    assert_eq!(report.mode, Mode::Model);
    assert_eq!(report.paths.len(), 3);

    let frames: Vec<Pixmap> = report.paths.iter().map(|p| read(p)).collect();
    for (i, f) in frames.iter().enumerate() {
        assert_eq!(
            (f.width(), f.height()),
            (640, 400),
            "frame {i} is the wrong size"
        );
        assert!(
            distinct_colors(f) > 24,
            "frame {i} has only {} distinct colours — a blank or flat-shaded frame",
            distinct_colors(f)
        );
    }

    let moved = differing_pixels(&frames[0], &frames[1]);
    assert!(
        moved > 640 * 400 / 100,
        "a 60 px orbit drag must visibly change the frame, only {moved} pixels moved"
    );

    // The reason a drag can hold 60 fps: turning the camera re-shades the same buffers.
    assert_eq!(
        report.geometry_uploads, 1,
        "orbiting must not re-upload geometry"
    );
}

#[test]
fn a_model_frame_shows_lit_geometry_against_the_background() {
    let Some(gpu) = gpu() else { return };
    let (tmp, _root, ws) = common::scratch();
    let out = tmp.path().join("one-frame");

    let report = headless::run(
        &gpu,
        &ws,
        &DocId::from(common::MODEL_DOC),
        &Options {
            frames: 1,
            out,
            size: [512, 512],
            samples: 4,
            status: false,
            ..Options::default()
        },
    )
    .expect("headless render");
    let pm = read(&report.paths[0]);

    // The scene is framed by its own bounds, so the middle of the image is the subject.
    let centre = pm.pixel(256, 256).expect("centre pixel");
    assert!(
        centre.alpha() > 0,
        "the subject must be in the middle of the frame"
    );

    // The two materials are gold and blue; a correct render is not monochrome, and a
    // lit render is not flat, so the frame must span a range of both hue and value.
    let mut min = 255u8;
    let mut max = 0u8;
    let mut warm = 0usize;
    let mut cool = 0usize;
    for p in pm.pixels() {
        if p.alpha() == 0 {
            continue;
        }
        let v = p.red().max(p.green()).max(p.blue());
        min = min.min(v);
        max = max.max(v);
        if p.red() > p.blue().saturating_add(20) {
            warm += 1;
        }
        if p.blue() > p.red().saturating_add(20) {
            cool += 1;
        }
    }
    assert!(
        max - min > 40,
        "a lit render has a range of brightness, got {min}..{max}"
    );
    assert!(
        warm > 500 && cool > 500,
        "both materials must be visible: {warm} warm pixels, {cool} cool pixels"
    );
}

#[test]
fn a_raster_document_renders_through_the_canvas_path_and_responds_to_pan_and_zoom() {
    let Some(gpu) = gpu() else { return };
    let (tmp, _root, ws) = common::scratch();
    let out = tmp.path().join("raster-frames");

    let report = headless::run(
        &gpu,
        &ws,
        &DocId::from(common::RASTER_DOC),
        &Options {
            frames: 2,
            out,
            size: [600, 400],
            samples: 1,
            drag_px: 80.0,
            status: true,
        },
    )
    .expect("headless render");

    assert_eq!(report.mode, Mode::Canvas);
    let a = read(&report.paths[0]);
    let b = read(&report.paths[1]);
    assert_eq!((a.width(), a.height()), (600, 400));

    // The fixture paints an orange rectangle on a dark slate background; both must be
    // on screen, which is only true if the pixmap was uploaded and the quad drawn.
    let orange = a
        .pixels()
        .iter()
        .filter(|p| p.red() > 150 && p.green() < 150 && p.blue() < 110)
        .count();
    assert!(
        orange > 2_000,
        "the document's fill is missing: {orange} px"
    );

    let moved = differing_pixels(&a, &b);
    assert!(
        moved > 600 * 400 / 50,
        "panning and zooming must move the image, only {moved} pixels changed"
    );

    // The document was rasterized and uploaded once; pan and zoom are pure GPU state.
    assert_eq!(
        report.canvas_uploads, 1,
        "panning or zooming re-uploaded the document texture"
    );
}

#[test]
fn a_vector_document_is_rasterized_by_the_engine_and_shown() {
    let Some(gpu) = gpu() else { return };
    let (tmp, _root, ws) = common::scratch();
    let out = tmp.path().join("vector-frames");

    let report = headless::run(
        &gpu,
        &ws,
        &DocId::from(common::VECTOR_DOC),
        &Options {
            frames: 1,
            out,
            size: [480, 480],
            samples: 1,
            status: false,
            ..Options::default()
        },
    )
    .expect("headless render");

    let pm = read(&report.paths[0]);
    let cyan = pm
        .pixels()
        .iter()
        .filter(|p| p.blue() > 150 && p.green() > 120 && p.red() < 140)
        .count();
    assert!(cyan > 5_000, "the vector ellipse is missing: {cyan} px");
}

#[test]
fn the_status_panel_is_drawn_over_the_frame() {
    let Some(gpu) = gpu() else { return };
    let (tmp, _root, ws) = common::scratch();
    let doc = DocId::from(common::MODEL_DOC);

    let with = headless::run(
        &gpu,
        &ws,
        &doc,
        &Options {
            frames: 1,
            out: tmp.path().join("with-status"),
            size: [640, 400],
            samples: 1,
            drag_px: 0.0,
            status: true,
        },
    )
    .expect("render");
    let without = headless::run(
        &gpu,
        &ws,
        &doc,
        &Options {
            frames: 1,
            out: tmp.path().join("no-status"),
            size: [640, 400],
            samples: 1,
            drag_px: 0.0,
            status: false,
        },
    )
    .expect("render");

    let a = read(&with.paths[0]);
    let b = read(&without.paths[0]);

    // Only the top-left corner may differ, and it must differ a lot: that region is
    // where the panel goes.
    let panel_diff = (12..300)
        .flat_map(|x| (12..110).map(move |y| (x, y)))
        .filter(|&(x, y)| {
            let p = a.pixel(x, y).expect("pixel");
            let q = b.pixel(x, y).expect("pixel");
            p.red().abs_diff(q.red()) > 8 || p.blue().abs_diff(q.blue()) > 8
        })
        .count();
    assert!(
        panel_diff > 2_000,
        "the status panel should cover a chunk of the top-left corner, {panel_diff} px differ"
    );

    let bottom_right_diff = (400..640)
        .flat_map(|x| (300..400).map(move |y| (x, y)))
        .filter(|&(x, y)| {
            let p = a.pixel(x, y).expect("pixel");
            let q = b.pixel(x, y).expect("pixel");
            p != q
        })
        .count();
    assert_eq!(
        bottom_right_diff, 0,
        "the panel must not touch the rest of the frame"
    );
}

#[test]
fn every_frame_is_written_and_named_in_order() {
    let Some(gpu) = gpu() else { return };
    let (tmp, _root, ws) = common::scratch();
    let out = tmp.path().join("ordered");

    let report = headless::run(
        &gpu,
        &ws,
        &DocId::from(common::MODEL_DOC),
        &Options {
            frames: 4,
            out: out.clone(),
            size: [256, 256],
            samples: 1,
            drag_px: 30.0,
            status: false,
        },
    )
    .expect("render");

    for (i, p) in report.paths.iter().enumerate() {
        assert_eq!(p, &out.join(format!("frame_{i:03}.png")));
        assert!(p.exists(), "{} was not written", p.display());
        assert!(
            std::fs::metadata(p).expect("metadata").len() > 1_000,
            "{} is suspiciously small for a 256x256 render",
            p.display()
        );
    }
    assert_eq!(report.draw_ms.len(), 4);
    assert!(report.draw_ms.iter().all(|m| *m > 0.0));
}
