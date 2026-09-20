//! Compositor behavior: blend modes, groups, clipping, masks, adjustment layers, scale.

mod common;

use common::{at, fixture, gray, DOC};
use dpaint_core::color::Color;
use dpaint_core::doc::common::Paint;
use dpaint_core::doc::raster::{BlendMode, Layer, LayerKind, Mask};
use dpaint_core::LayerId;
use dpaint_raster::Canvas;

#[test]
fn render_at_scale_two_doubles_both_dimensions_exactly() {
    let mut f = fixture(37, 11);
    f.pixel_layer("lyr_a", |_, _| gray(0.5));
    let one = f.render(1.0);
    let two = f.render(2.0);
    assert_eq!((one.width(), one.height()), (37, 11));
    assert_eq!((two.width(), two.height()), (74, 22));
    let half = f.render(0.5);
    assert_eq!((half.width(), half.height()), (19, 6), "rounds, never collapses to zero");
}

#[test]
fn multiply_over_white_is_identity_and_over_black_is_black() {
    // Backdrop white, source mid-gray -> the gray survives untouched.
    let mut f = fixture(4, 4);
    f.pixel_layer("lyr_bg", |_, _| gray(1.0));
    let top = f.pixel_layer("lyr_top", |_, _| gray(0.25));
    f.doc_mut().layer_mut(&top).unwrap().blend = BlendMode::Multiply;
    let out = at(&f.render(1.0), 2, 2);
    assert!((out[0] - 0.25).abs() < 0.01, "multiply over white must be identity: {out:?}");

    // Backdrop black -> black, whatever the source is.
    let mut f = fixture(4, 4);
    f.pixel_layer("lyr_bg", |_, _| gray(0.0));
    let top = f.pixel_layer("lyr_top", |_, _| gray(0.8));
    f.doc_mut().layer_mut(&top).unwrap().blend = BlendMode::Multiply;
    let out = at(&f.render(1.0), 2, 2);
    assert!(out[0] < 0.005, "multiply over black must stay black: {out:?}");
}

#[test]
fn screen_is_the_dual_of_multiply() {
    let backdrop = 0.36f32;
    let source = 0.49f32;
    let run = |mode: BlendMode| {
        let mut f = fixture(4, 4);
        f.pixel_layer("lyr_bg", |_, _| gray(backdrop));
        let top = f.pixel_layer("lyr_top", |_, _| gray(source));
        f.doc_mut().layer_mut(&top).unwrap().blend = mode;
        at(&f.render(1.0), 2, 2)[0]
    };
    let mul = run(BlendMode::Multiply);
    let scr = run(BlendMode::Screen);
    // screen(a,b) == 1 - multiply(1-a, 1-b)
    let expected_screen = 1.0 - (1.0 - backdrop) * (1.0 - source);
    assert!((mul - backdrop * source).abs() < 0.01, "multiply: {mul}");
    assert!((scr - expected_screen).abs() < 0.01, "screen: {scr} vs {expected_screen}");
    assert!(scr > backdrop && mul < backdrop, "screen lightens, multiply darkens");
}

#[test]
fn luminosity_takes_the_source_light_and_keeps_the_backdrop_hue() {
    let mut f = fixture(4, 4);
    // Saturated red backdrop.
    f.pixel_layer("lyr_bg", |_, _| [0.6, 0.05, 0.05, 1.0]);
    let top = f.pixel_layer("lyr_top", |_, _| gray(0.2));
    f.doc_mut().layer_mut(&top).unwrap().blend = BlendMode::Luminosity;
    let out = at(&f.render(1.0), 2, 2);
    assert!(out[0] > out[1] * 3.0, "backdrop hue (red-dominant) must survive: {out:?}");
    let lum = 0.3 * out[0] + 0.59 * out[1] + 0.11 * out[2];
    assert!((lum - 0.2).abs() < 0.02, "light should come from the source: {lum}");
}

#[test]
fn every_blend_mode_composites_without_producing_invalid_pixels() {
    for mode in BlendMode::ALL {
        let mut f = fixture(3, 3);
        f.pixel_layer("lyr_bg", |x, _| [0.2 + x as f32 * 0.3, 0.4, 0.7, 1.0]);
        let top = f.pixel_layer("lyr_top", |_, y| [0.8, 0.3, 0.1, 0.25 + y as f32 * 0.3]);
        {
            let l = f.doc_mut().layer_mut(&top).unwrap();
            l.blend = mode;
            l.opacity = 0.75;
        }
        let pm = f.render(1.0);
        for y in 0..3 {
            for x in 0..3 {
                let p = pm.pixel(x, y).expect("in bounds");
                assert!(
                    p.red() <= p.alpha() && p.green() <= p.alpha() && p.blue() <= p.alpha(),
                    "{mode:?} produced an invalid premultiplied pixel at {x},{y}: {p:?}"
                );
            }
        }
    }
}

#[test]
fn dissolve_is_stochastic_coverage_and_reproducible() {
    let mut f = fixture(32, 32);
    f.pixel_layer("lyr_bg", |_, _| gray(0.0));
    let top = f.pixel_layer("lyr_top", |_, _| gray(1.0));
    {
        let l = f.doc_mut().layer_mut(&top).unwrap();
        l.blend = BlendMode::Dissolve;
        l.opacity = 0.5;
    }
    let a = f.render(1.0);
    let b = f.render(1.0);
    assert_eq!(a.data(), b.data(), "the same document must dissolve identically");
    let lit = a.pixels().iter().filter(|p| p.red() > 200).count();
    assert!(
        (300..=724).contains(&lit),
        "about half the pixels should take the source, got {lit} of 1024"
    );
}

#[test]
fn group_opacity_is_not_the_same_as_per_child_opacity() {
    // Two overlapping opaque squares in a group at 50%: where they overlap, the group
    // composites once (one 50% veil). Fading each child instead veils twice.
    let build = |group: bool| -> [f32; 4] {
        let mut f = fixture(8, 8);
        let a = Layer::new(LayerId::from("lyr_a"), "a", LayerKind::Fill { color: Color::WHITE });
        let mut b = Layer::new(
            LayerId::from("lyr_b"),
            "b",
            LayerKind::Fill { color: Color::BLACK },
        );
        if group {
            b.opacity = 1.0;
            let mut g = Layer::new(
                LayerId::from("lyr_g"),
                "g",
                LayerKind::Group { layers: vec![a, b] },
            );
            g.opacity = 0.5;
            f.push_layer(g);
        } else {
            let mut a = a;
            a.opacity = 0.5;
            b.opacity = 0.5;
            f.push_layer(a);
            f.push_layer(b);
        }
        at(&f.render(1.0), 4, 4)
    };
    let grouped = build(true);
    let flat = build(false);
    // Group: black at 50% over nothing -> half-transparent black.
    assert!((grouped[3] - 0.5).abs() < 0.01, "group alpha: {grouped:?}");
    assert!(grouped[0] < 0.01, "group color is the top child's black: {grouped:?}");
    // Flat: white at 50% then black at 50% -> 75% alpha with white showing through.
    assert!(flat[3] > 0.7, "per-child alpha accumulates: {flat:?}");
    assert!(flat[0] > 0.2, "white survives under the 50% black: {flat:?}");
    assert!(
        (grouped[3] - flat[3]).abs() > 0.2,
        "group opacity must differ from per-child opacity: {grouped:?} vs {flat:?}"
    );
}

#[test]
fn group_blend_mode_applies_to_the_composited_group() {
    let mut f = fixture(4, 4);
    f.pixel_layer("lyr_bg", |_, _| gray(0.5));
    let inner = Layer::new(LayerId::from("lyr_in"), "in", LayerKind::Fill { color: Color::WHITE });
    let mut g = Layer::new(
        LayerId::from("lyr_g"),
        "g",
        LayerKind::Group { layers: vec![inner] },
    );
    g.blend = BlendMode::Multiply;
    f.push_layer(g);
    // White multiplied over mid-gray leaves the gray.
    let out = at(&f.render(1.0), 2, 2);
    assert!((out[0] - 0.5).abs() < 0.01, "group blend must apply to the group: {out:?}");
}

#[test]
fn a_clipping_mask_limits_a_layer_to_the_alpha_beneath_it() {
    let mut f = fixture(8, 8);
    // Base: opaque only in the left half.
    f.pixel_layer("lyr_base", |x, _| if x < 4 { [1.0, 1.0, 1.0, 1.0] } else { [0.0; 4] });
    // Clipped layer: solid red everywhere.
    let top = f.pixel_layer("lyr_top", |_, _| [1.0, 0.0, 0.0, 1.0]);
    f.doc_mut().layer_mut(&top).unwrap().clip = true;
    let pm = f.render(1.0);
    let inside = at(&pm, 1, 4);
    let outside = at(&pm, 6, 4);
    assert!(inside[0] > 0.9 && inside[1] < 0.05, "red shows where the base is opaque: {inside:?}");
    assert_eq!(outside[3], 0.0, "and nowhere else: {outside:?}");
}

#[test]
fn a_layer_mask_multiplies_alpha() {
    let mut f = fixture(8, 8);
    let id = f.pixel_layer("lyr_a", |_, _| [1.0, 1.0, 1.0, 1.0]);
    // Mask: opaque on the top half, transparent on the bottom.
    let cov: Vec<f32> = (0..64).map(|i| if i / 8 < 4 { 1.0 } else { 0.0 }).collect();
    let png = dpaint_raster::canvas::encode_gray_png(8, 8, &cov).unwrap();
    let asset = f.assets.put(&png, "png").unwrap();
    f.doc_mut().layer_mut(&id).unwrap().mask =
        Some(Mask { asset, enabled: true, inverted: false, offset: [0, 0] });
    let pm = f.render(1.0);
    assert_eq!(at(&pm, 4, 1)[3], 1.0);
    assert_eq!(at(&pm, 4, 6)[3], 0.0);

    // Inverting the mask swaps which half survives.
    f.doc_mut().layer_mut(&id).unwrap().mask.as_mut().unwrap().inverted = true;
    let pm = f.render(1.0);
    assert_eq!(at(&pm, 4, 1)[3], 0.0);
    assert_eq!(at(&pm, 4, 6)[3], 1.0);
}

#[test]
fn an_adjustment_layer_affects_layers_beneath_it_and_not_above() {
    let mut f = fixture(4, 12);
    // Bottom layer covers rows 0..4, top layer covers rows 8..12.
    f.pixel_layer("lyr_low", |_, y| if y < 4 { gray(0.5) } else { [0.0; 4] });
    f.push_layer(Layer::new(
        LayerId::from("lyr_adj"),
        "invert",
        LayerKind::Adjustment { adjustment: dpaint_core::doc::raster::Adjustment::Invert },
    ));
    f.pixel_layer("lyr_high", |_, y| if y >= 8 { gray(0.5) } else { [0.0; 4] });

    let pm = f.render(1.0);
    let below = at(&pm, 2, 1);
    let above = at(&pm, 2, 10);
    // 0.5 linear is ~0.735 display; inverting display 0.735 gives ~0.265 -> ~0.056 linear.
    assert!(below[0] < 0.2, "the layer beneath must be inverted: {below:?}");
    assert!((above[0] - 0.5).abs() < 0.02, "the layer above must be untouched: {above:?}");
}

#[test]
fn an_adjustment_layer_inside_a_group_stays_inside_the_group() {
    let mut f = fixture(4, 4);
    f.pixel_layer("lyr_outside", |_, _| gray(0.5));
    let inner = Layer::new(
        LayerId::from("lyr_g_fill"),
        "fill",
        LayerKind::Fill { color: Color::rgba(1.0, 1.0, 1.0, 0.0) },
    );
    let adj = Layer::new(
        LayerId::from("lyr_g_adj"),
        "invert",
        LayerKind::Adjustment { adjustment: dpaint_core::doc::raster::Adjustment::Invert },
    );
    f.push_layer(Layer::new(
        LayerId::from("lyr_g"),
        "g",
        LayerKind::Group { layers: vec![inner, adj] },
    ));
    let out = at(&f.render(1.0), 2, 2);
    assert!(
        (out[0] - 0.5).abs() < 0.02,
        "a grouped adjustment must not reach the document backdrop: {out:?}"
    );
}

#[test]
fn an_adjustment_layer_honors_its_own_mask_and_opacity() {
    let mut f = fixture(8, 8);
    f.pixel_layer("lyr_low", |_, _| gray(0.5));
    let adj = Layer::new(
        LayerId::from("lyr_adj"),
        "invert",
        LayerKind::Adjustment { adjustment: dpaint_core::doc::raster::Adjustment::Invert },
    );
    let id = f.push_layer(adj);
    let cov: Vec<f32> = (0..64).map(|i| if i % 8 < 4 { 1.0 } else { 0.0 }).collect();
    let png = dpaint_raster::canvas::encode_gray_png(8, 8, &cov).unwrap();
    let asset = f.assets.put(&png, "png").unwrap();
    f.doc_mut().layer_mut(&id).unwrap().mask =
        Some(Mask { asset, enabled: true, inverted: false, offset: [0, 0] });
    let pm = f.render(1.0);
    assert!(at(&pm, 1, 4)[0] < 0.2, "masked-in half is inverted");
    assert!((at(&pm, 6, 4)[0] - 0.5).abs() < 0.02, "masked-out half is untouched");
}

#[test]
fn a_linked_layer_renders_another_document_fitted_to_its_box() {
    let mut f = fixture(40, 40);
    // A second raster document, solid red, 10x20 (taller than wide).
    let mut other = dpaint_core::RasterDoc::new("doc_badge".into(), "badge", 10, 20);
    other.layers.push(Layer::new(
        LayerId::from("lyr_red"),
        "red",
        LayerKind::Fill { color: Color::rgba(1.0, 0.0, 0.0, 1.0) },
    ));
    f.project.add_document(dpaint_core::Document::Raster(other));
    f.push_layer(Layer::new(
        LayerId::from("lyr_link"),
        "link",
        LayerKind::Linked {
            document: "doc_badge".into(),
            fit: dpaint_core::doc::raster::Fit::Contain,
            r#box: dpaint_core::doc::common::Rect::new(0.0, 0.0, 40.0, 40.0),
        },
    ));
    let pm = f.render(1.0);
    // Contain fits 10x20 into 40x40 as 20x40, centered: columns 10..30 are red.
    assert!(at(&pm, 20, 20)[0] > 0.9, "linked content is present");
    assert_eq!(at(&pm, 2, 20)[3], 0.0, "and letterboxed, not stretched");
}

#[test]
fn hidden_layers_and_their_clipped_children_are_skipped() {
    let mut f = fixture(4, 4);
    let base = f.pixel_layer("lyr_base", |_, _| [1.0, 1.0, 1.0, 1.0]);
    let top = f.pixel_layer("lyr_top", |_, _| [1.0, 0.0, 0.0, 1.0]);
    f.doc_mut().layer_mut(&top).unwrap().clip = true;
    f.doc_mut().layer_mut(&base).unwrap().visible = false;
    let pm = f.render(1.0);
    assert_eq!(at(&pm, 2, 2)[3], 0.0, "hiding the clip base hides the clipped layer too");
}

#[test]
fn document_background_sits_under_every_layer() {
    let mut f = fixture(4, 4);
    f.doc_mut().background = Some(Color::rgba(0.0, 0.0, 1.0, 1.0));
    f.pixel_layer("lyr_a", |x, _| if x < 2 { [1.0, 0.0, 0.0, 1.0] } else { [0.0; 4] });
    let pm = f.render(1.0);
    assert!(at(&pm, 0, 0)[0] > 0.9, "layer wins where it is opaque");
    assert!(at(&pm, 3, 0)[2] > 0.9, "background shows through where it is not");
}

#[test]
fn layer_transforms_resample_pixel_content_in_linear_light() {
    let mut f = fixture(16, 16);
    let id = f.pixel_layer("lyr_a", |x, y| {
        if (4..8).contains(&x) && (4..8).contains(&y) {
            [1.0, 1.0, 1.0, 1.0]
        } else {
            [0.0; 4]
        }
    });
    f.doc_mut().layer_mut(&id).unwrap().transform =
        dpaint_core::doc::common::Transform::translate(4.0, 4.0);
    let pm = f.render(1.0);
    assert_eq!(at(&pm, 5, 5)[3], 0.0, "content moved away from its old place");
    assert_eq!(at(&pm, 9, 9)[3], 1.0, "and arrived at the new one");
}

#[test]
fn canvas_round_trips_through_a_pixmap_without_drifting() {
    let mut c = Canvas::new(3, 1);
    c.set_straight(0, [1.0, 0.25, 0.0, 1.0]);
    c.set_straight(4, [0.0, 0.0, 0.0, 0.5]);
    c.set_straight(8, [0.5, 0.5, 0.5, 1.0]);
    let back = Canvas::from_pixmap(c.to_pixmap().as_ref());
    for i in [0usize, 4, 8] {
        let (a, b) = (c.straight(i), back.straight(i));
        for ch in 0..4 {
            assert!((a[ch] - b[ch]).abs() < 0.01, "channel {ch} drifted: {a:?} -> {b:?}");
        }
    }
}

#[test]
fn shape_and_text_layers_rasterize_sharper_at_higher_scale() {
    let mut f = fixture(20, 20);
    f.push_layer(Layer::new(
        LayerId::from("lyr_circle"),
        "circle",
        LayerKind::Shape {
            d: "M 4 10 A 6 6 0 1 0 16 10 A 6 6 0 1 0 4 10 Z".into(),
            fill: Paint::solid(Color::BLACK),
            stroke: None,
            fill_rule: Default::default(),
        },
    ));
    let one = f.canvas(1.0);
    let two = f.canvas(2.0);
    let covered = |c: &Canvas| c.data.chunks_exact(4).filter(|p| p[3] > 0.5).count() as f64;
    // Four times the pixels for twice the scale, within anti-aliasing slack.
    let ratio = covered(&two) / covered(&one);
    assert!((ratio - 4.0).abs() < 0.4, "area should scale with the square of the scale: {ratio}");
}

#[test]
fn the_public_api_matches_the_signatures_other_crates_call() {
    // dpaint-render calls exactly these; a signature change here breaks it at compile time,
    // so pin them as function pointers.
    let render: fn(
        &dpaint_core::Project,
        &dpaint_core::DocId,
        &dpaint_core::AssetStore,
        f64,
        &dpaint_raster::LinkResolver<'_>,
    ) -> dpaint_core::Result<tiny_skia::Pixmap> = dpaint_raster::render_doc;
    let ops: fn() -> Vec<Box<dyn dpaint_core::Op>> = dpaint_raster::ops;
    assert!(!ops().is_empty());

    // And the resolver really is called for linked documents.
    let mut f = fixture(8, 8);
    let mut other = dpaint_core::RasterDoc::new("doc_other".into(), "other", 8, 8);
    other.layers.push(Layer::new(
        LayerId::from("lyr_w"),
        "w",
        LayerKind::Fill { color: Color::WHITE },
    ));
    f.project.add_document(dpaint_core::Document::Raster(other));
    f.push_layer(Layer::new(
        LayerId::from("lyr_link"),
        "link",
        LayerKind::Linked {
            document: "doc_other".into(),
            fit: dpaint_core::doc::raster::Fit::Stretch,
            r#box: dpaint_core::doc::common::Rect::new(0.0, 0.0, 8.0, 8.0),
        },
    ));
    let calls = std::cell::Cell::new(0usize);
    let link = |id: &dpaint_core::DocId, w: u32, h: u32| {
        calls.set(calls.get() + 1);
        assert_eq!(id.as_str(), "doc_other");
        let mut pm = tiny_skia::Pixmap::new(w, h).unwrap();
        pm.fill(tiny_skia::Color::from_rgba8(255, 0, 0, 255));
        Ok(pm)
    };
    let pm = render(&f.project, &DOC.into(), &f.assets, 1.0, &link).unwrap();
    assert_eq!(calls.get(), 1, "the supplied resolver must be used, not an internal one");
    assert!(at(&pm, 4, 4)[0] > 0.9, "and its pixmap must land in the composite");
}
