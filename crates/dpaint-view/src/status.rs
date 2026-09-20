//! The on-window readout.
//!
//! Composing it is a pure function over a plain struct so the wording is testable and the
//! renderer only has to draw strings.

use crate::input::Mode;

/// Everything the status panel reports.
#[derive(Debug, Clone)]
pub struct Status {
    pub document: String,
    pub kind: String,
    pub mode: Mode,
    /// Model: the fitted-distance multiplier. Canvas: surface pixels per document pixel.
    pub zoom: f32,
    pub yaw: f32,
    pub pitch: f32,
    pub pan: [f32; 2],
    /// Document extent in pixels, for 2D documents.
    pub content: Option<[u32; 2]>,
    pub frame_ms: f32,
    pub backend: String,
    pub adapter: String,
    pub samples: u32,
    pub reloads: u32,
}

impl Status {
    pub fn fps(&self) -> f32 {
        if self.frame_ms > 0.0 {
            1000.0 / self.frame_ms
        } else {
            0.0
        }
    }
}

/// The lines the overlay draws, top to bottom.
pub fn lines(s: &Status) -> Vec<String> {
    let mut out = Vec::with_capacity(4);
    out.push(format!("{}  ·  {}", s.document, s.kind));

    match s.mode {
        Mode::Model => out.push(format!(
            "zoom {:.2}x   yaw {:.1}°   pitch {:.1}°   pan {:.2}, {:.2}",
            s.zoom, s.yaw, s.pitch, s.pan[0], s.pan[1]
        )),
        Mode::Canvas => {
            let size = match s.content {
                Some([w, h]) => format!("{w}x{h}"),
                None => "-".to_string(),
            };
            out.push(format!(
                "zoom {:.0}%   {}   pan {:.0}, {:.0}",
                s.zoom * 100.0,
                size,
                s.pan[0],
                s.pan[1]
            ));
        }
    }

    out.push(format!("{:.2} ms   {:.0} fps", s.frame_ms, s.fps()));

    let msaa = if s.samples > 1 {
        format!("   {}x MSAA", s.samples)
    } else {
        String::new()
    };
    out.push(format!("gpu: {} · {}{}", s.backend, s.adapter, msaa));

    if s.reloads > 0 {
        let plural = if s.reloads == 1 { "" } else { "s" };
        out.push(format!("journal: {} reload{}", s.reloads, plural));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Status {
        Status {
            document: "doc_scene".into(),
            kind: "model".into(),
            mode: Mode::Model,
            zoom: 1.0,
            yaw: 35.0,
            pitch: 20.0,
            pan: [0.0, 0.0],
            content: None,
            frame_ms: 8.0,
            backend: "Metal".into(),
            adapter: "Apple A18 Pro".into(),
            samples: 4,
            reloads: 0,
        }
    }

    #[test]
    fn the_readout_names_the_document_its_kind_and_the_backend() {
        let text = lines(&sample()).join("\n");
        assert!(text.contains("doc_scene"), "{text}");
        assert!(text.contains("model"), "{text}");
        assert!(text.contains("Metal"), "{text}");
        assert!(text.contains("Apple A18 Pro"), "{text}");
        assert!(text.contains("4x MSAA"), "{text}");
    }

    #[test]
    fn a_model_shows_camera_angles_and_a_canvas_shows_its_pixel_size() {
        let model = lines(&sample()).join("\n");
        assert!(model.contains("yaw 35.0°"), "{model}");
        assert!(model.contains("pitch 20.0°"), "{model}");

        let canvas = lines(&Status {
            mode: Mode::Canvas,
            kind: "raster".into(),
            zoom: 1.5,
            content: Some([1920, 1080]),
            ..sample()
        })
        .join("\n");
        assert!(canvas.contains("150%"), "{canvas}");
        assert!(canvas.contains("1920x1080"), "{canvas}");
        assert!(
            !canvas.contains("yaw"),
            "a flat document has no camera angles: {canvas}"
        );
    }

    #[test]
    fn frame_time_is_reported_with_the_rate_it_implies() {
        let text = lines(&Status {
            frame_ms: 16.0,
            ..sample()
        })
        .join("\n");
        assert!(text.contains("16.00 ms"), "{text}");
        assert!(text.contains("62 fps"), "{text}");
    }

    #[test]
    fn a_first_frame_with_no_measurement_yet_does_not_divide_by_zero() {
        let s = Status {
            frame_ms: 0.0,
            ..sample()
        };
        assert_eq!(s.fps(), 0.0);
        assert!(lines(&s).iter().any(|l| l.contains("0 fps")));
    }

    #[test]
    fn reloads_are_only_mentioned_once_they_have_happened() {
        assert!(!lines(&sample()).iter().any(|l| l.contains("journal")));

        let one = lines(&Status {
            reloads: 1,
            ..sample()
        });
        assert!(one.iter().any(|l| l.contains("1 reload")), "{one:?}");

        let many = lines(&Status {
            reloads: 4,
            ..sample()
        });
        assert!(many.iter().any(|l| l.contains("4 reloads")), "{many:?}");
    }

    #[test]
    fn a_single_sample_target_does_not_claim_msaa() {
        let text = lines(&Status {
            samples: 1,
            ..sample()
        })
        .join("\n");
        assert!(!text.contains("MSAA"), "{text}");
    }
}
