//! The `raster.*` op catalog: the ids agents call, and what each one actually does.

mod common;

use common::{at, bytes_at, fixture, gray, DOC};
use dpaint_core::doc::raster::LayerKind;
use dpaint_core::Registry;
use dpaint_raster::filters::luma_variance;
use dpaint_raster::Canvas;
use serde_json::json;

/// The ids in `docs/op-registry.md`. This is the contract every other surface derives from:
/// the CLI flags, the MCP tool names and the generated docs all come from these strings, so a
/// rename is a breaking change and has to show up here.
const CATALOG: &[&str] = &[
    "raster.canvas.resize",
    "raster.canvas.crop",
    "raster.canvas.trim",
    "raster.canvas.rotate",
    "raster.canvas.flip",
    "raster.canvas.set-dpi",
    "raster.canvas.set-background",
    "raster.canvas.set-guides",
    "raster.layer.add",
    "raster.layer.remove",
    "raster.layer.duplicate",
    "raster.layer.reorder",
    "raster.layer.rename",
    "raster.layer.set",
    "raster.layer.transform",
    "raster.layer.group",
    "raster.layer.ungroup",
    "raster.layer.merge-down",
    "raster.layer.rasterize",
    "raster.layer.from-selection",
    "raster.doc.flatten",
    "raster.mask.add",
    "raster.mask.remove",
    "raster.mask.apply",
    "raster.mask.invert",
    "raster.mask.from-selection",
    "raster.mask.from-luminance",
    "raster.clip.set",
    "raster.adjust.curves",
    "raster.adjust.levels",
    "raster.adjust.brightness-contrast",
    "raster.adjust.hsl",
    "raster.adjust.color-balance",
    "raster.adjust.exposure",
    "raster.adjust.channel-mixer",
    "raster.adjust.threshold",
    "raster.adjust.posterize",
    "raster.adjust.invert",
    "raster.adjust.desaturate",
    "raster.adjust.lut",
    "raster.filter.gaussian-blur",
    "raster.filter.box-blur",
    "raster.filter.motion-blur",
    "raster.filter.radial-blur",
    "raster.filter.unsharp",
    "raster.filter.sharpen",
    "raster.filter.noise-add",
    "raster.filter.noise-reduce",
    "raster.filter.pixelate",
    "raster.filter.convolve",
    "raster.filter.morphology",
    "raster.filter.displace",
    "raster.filter.channel-op",
    "raster.filter.dither",
    "raster.filter.edge-detect",
    "raster.select.rect",
    "raster.select.ellipse",
    "raster.select.path",
    "raster.select.color-range",
    "raster.select.wand",
    "raster.select.alpha",
    "raster.select.text",
    "raster.select.all",
    "raster.select.none",
    "raster.select.invert",
    "raster.select.grow",
    "raster.select.shrink",
    "raster.select.feather",
    "raster.select.to-path",
    "raster.paint.stroke",
    "raster.paint.fill-bucket",
    "raster.paint.gradient",
    "raster.paint.erase",
    "raster.paint.pattern",
    "raster.text.add",
    "raster.text.set",
    "raster.text.fit",
    "raster.text.to-shape",
    "raster.effect.drop-shadow",
    "raster.effect.inner-shadow",
    "raster.effect.stroke",
    "raster.effect.outer-glow",
    "raster.effect.blur",
    "raster.effect.remove",
];

#[test]
fn the_registered_catalog_is_exactly_the_documented_one() {
    let mut registry = Registry::new();
    registry.extend(dpaint_raster::ops());
    let mut got: Vec<&str> = registry.ids();
    got.sort_unstable();
    let mut want: Vec<&str> = CATALOG.to_vec();
    want.sort_unstable();
    assert_eq!(got, want);
    assert_eq!(got.len(), 84, "the v1 raster catalog is 84 ops");
}

#[test]
fn every_op_declares_a_raster_mode_a_summary_and_an_object_schema() {
    for op in dpaint_raster::ops() {
        assert!(!op.about().is_empty(), "{} has no summary", op.id());
        assert_eq!(op.modes(), &[dpaint_core::DocKind::Raster], "{}", op.id());
        let schema = op.schema();
        assert_eq!(
            schema["type"],
            "object",
            "{} schema is not an object",
            op.id()
        );
    }
}

#[test]
fn a_filter_writes_a_new_blob_and_leaves_the_old_one_intact() {
    let mut f = fixture(24, 24);
    let id = f.pixel_layer("lyr_a", |x, y| {
        gray(if (x / 3 + y / 3) % 2 == 0 { 1.0 } else { 0.0 })
    });
    let before_asset = f.layer_asset(&id);
    let before = f.layer_pixels(&id);

    f.ok(
        "raster.filter.gaussian-blur",
        json!({ "target": "#lyr_a", "sigma": 2.0 }),
    );

    let after_asset = f.layer_asset(&id);
    assert_ne!(
        before_asset, after_asset,
        "the layer must point at a new blob"
    );
    // Undo is a JSON patch, so the previous blob has to still be readable.
    let recovered =
        Canvas::from_png(&f.assets.get(&before_asset).expect("old blob survives")).expect("decode");
    assert_eq!(
        recovered.data, before.data,
        "the old blob was mutated in place"
    );

    let after = f.layer_pixels(&id);
    assert!(
        luma_variance(&after) < luma_variance(&before) * 0.5,
        "the blur did not blur: {} -> {}",
        luma_variance(&before),
        luma_variance(&after)
    );
}

#[test]
fn a_selection_scoped_filter_leaves_outside_pixels_bit_identical() {
    let mut f = fixture(32, 32);
    let id = f.pixel_layer("lyr_a", |x, y| {
        gray(if (x / 2 + y / 2) % 2 == 0 { 1.0 } else { 0.05 })
    });
    let before = f.layer_pixels(&id);

    f.ok(
        "raster.select.rect",
        json!({ "rect": [4.0, 4.0, 12.0, 12.0] }),
    );
    f.ok(
        "raster.filter.gaussian-blur",
        json!({ "target": "#lyr_a", "sigma": 3.0 }),
    );
    let after = f.layer_pixels(&id);

    let mut changed_inside = 0;
    for y in 0..32u32 {
        for x in 0..32u32 {
            let i = before.idx(x, y);
            let inside = (5..15).contains(&x) && (5..15).contains(&y);
            if inside {
                if before.data[i..i + 4] != after.data[i..i + 4] {
                    changed_inside += 1;
                }
            } else if !(3..17).contains(&x) || !(3..17).contains(&y) {
                // Well outside the selection (and its anti-aliased border): must be identical
                // down to the bit, not merely close.
                assert_eq!(
                    before.data[i..i + 4],
                    after.data[i..i + 4],
                    "pixel {x},{y} outside the selection changed"
                );
            }
        }
    }
    assert!(
        changed_inside > 50,
        "only {changed_inside} pixels changed inside the selection"
    );
}

#[test]
fn a_selection_scoped_adjustment_only_lightens_inside_the_selection() {
    let mut f = fixture(16, 16);
    let id = f.pixel_layer("lyr_a", |_, _| gray(0.25));
    let before = f.layer_pixels(&id);
    f.ok(
        "raster.select.rect",
        json!({ "rect": [0.0, 0.0, 8.0, 16.0] }),
    );
    f.ok(
        "raster.adjust.brightness-contrast",
        json!({ "target": "#lyr_a", "brightness": 0.3 }),
    );
    let c = f.layer_pixels(&id);
    assert!(
        c.get(2, 8)[0] > before.get(2, 8)[0] + 0.05,
        "inside brightened: {:?}",
        c.get(2, 8)
    );
    let i = c.idx(13, 8);
    assert_eq!(
        c.data[i..i + 4],
        before.data[i..i + 4],
        "outside the selection must be bit-identical"
    );
}

#[test]
fn scope_whole_ignores_the_selection_on_purpose() {
    let mut f = fixture(16, 16);
    let id = f.pixel_layer("lyr_a", |_, _| gray(0.25));
    f.ok(
        "raster.select.rect",
        json!({ "rect": [0.0, 0.0, 4.0, 4.0] }),
    );
    f.ok(
        "raster.adjust.brightness-contrast",
        json!({ "target": "#lyr_a", "brightness": 0.3, "scope": "whole" }),
    );
    let c = f.layer_pixels(&id);
    assert!(
        c.get(13, 13)[0] > 0.3,
        "scope=whole must reach outside the selection"
    );
}

#[test]
fn magic_wand_selects_exactly_one_region_of_a_two_tone_image() {
    let mut f = fixture(20, 10);
    f.pixel_layer("lyr_a", |x, _| if x < 8 { gray(0.0) } else { gray(1.0) });
    let effect = f.ok(
        "raster.select.wand",
        json!({ "at": [2, 5], "tolerance": 0.05 }),
    );
    let area = effect.data.as_ref().unwrap()["area_px"].as_f64().unwrap();
    assert_eq!(area, 80.0, "the dark region is 8x10 pixels");
    let bounds = &effect.data.as_ref().unwrap()["bounds"];
    assert_eq!(bounds[0], 0.0);
    assert_eq!(bounds[2], 8.0, "and nothing to the right of it");

    // Seeding the other tone selects the complement.
    let effect = f.ok(
        "raster.select.wand",
        json!({ "at": [15, 5], "tolerance": 0.05 }),
    );
    assert_eq!(effect.data.unwrap()["area_px"].as_f64().unwrap(), 120.0);
}

#[test]
fn wand_with_contiguous_false_reaches_disconnected_regions() {
    let mut f = fixture(12, 4);
    // Two black bars separated by white.
    f.pixel_layer("lyr_a", |x, _| {
        if !(3..9).contains(&x) {
            gray(0.0)
        } else {
            gray(1.0)
        }
    });
    let connected = f.ok(
        "raster.select.wand",
        json!({ "at": [1, 1], "tolerance": 0.05 }),
    );
    assert_eq!(connected.data.unwrap()["area_px"].as_f64().unwrap(), 12.0);
    let global = f.ok(
        "raster.select.wand",
        json!({ "at": [1, 1], "tolerance": 0.05, "contiguous": false }),
    );
    assert_eq!(global.data.unwrap()["area_px"].as_f64().unwrap(), 24.0);
}

#[test]
fn selection_modes_add_subtract_and_intersect() {
    let mut f = fixture(20, 20);
    f.pixel_layer("lyr_a", |_, _| gray(0.5));
    f.ok(
        "raster.select.rect",
        json!({ "rect": [0.0, 0.0, 10.0, 10.0] }),
    );
    let added = f.ok(
        "raster.select.rect",
        json!({ "rect": [10.0, 0.0, 10.0, 10.0], "mode": "add" }),
    );
    assert!(
        (added.data.unwrap()["area_px"].as_f64().unwrap() - 200.0).abs() < 2.0,
        "add unions the two rects"
    );
    let intersected = f.ok(
        "raster.select.rect",
        json!({ "rect": [5.0, 0.0, 10.0, 10.0], "mode": "intersect" }),
    );
    assert!(
        (intersected.data.unwrap()["area_px"].as_f64().unwrap() - 100.0).abs() < 2.0,
        "intersect keeps only the overlap"
    );
    let subtracted = f.ok(
        "raster.select.rect",
        json!({ "rect": [5.0, 0.0, 5.0, 10.0], "mode": "subtract" }),
    );
    assert!(
        (subtracted.data.unwrap()["area_px"].as_f64().unwrap() - 50.0).abs() < 2.0,
        "subtract removes the overlap"
    );
}

#[test]
fn select_grow_shrink_and_feather_change_the_covered_area() {
    let mut f = fixture(40, 40);
    f.pixel_layer("lyr_a", |_, _| gray(0.5));
    f.ok(
        "raster.select.rect",
        json!({ "rect": [10.0, 10.0, 20.0, 20.0] }),
    );
    let grown = f
        .ok("raster.select.grow", json!({ "pixels": 3 }))
        .data
        .unwrap()["area_px"]
        .as_f64()
        .unwrap();
    assert!(
        grown > 400.0,
        "grow must cover more than the original 400 px: {grown}"
    );
    let shrunk = f
        .ok("raster.select.shrink", json!({ "pixels": 6 }))
        .data
        .unwrap()["area_px"]
        .as_f64()
        .unwrap();
    assert!(
        shrunk < grown,
        "shrink must undo more than it added: {shrunk} vs {grown}"
    );

    // Feathering a vector selection keeps the outline and records the radius, so the
    // selection stays resolution-independent.
    let mut f = fixture(40, 40);
    f.pixel_layer("lyr_a", |_, _| gray(0.5));
    f.ok(
        "raster.select.rect",
        json!({ "rect": [10.0, 10.0, 20.0, 20.0] }),
    );
    f.ok("raster.select.feather", json!({ "radius": 2.0 }));
    let sel = f.doc().selection.clone().unwrap();
    assert_eq!(sel.feather, 2.0);
    assert!(sel.d.is_some() && sel.mask.is_none());

    // And a feathered edge really is soft: an inverting edit fades across it.
    let id = f.pixel_layer("lyr_b", |_, _| gray(0.5));
    f.ok("raster.adjust.invert", json!({ "target": "#lyr_b" }));
    let c = f.layer_pixels(&id);
    let edge = c.get(10, 20)[0];
    assert!(
        edge > 0.05 && edge < 0.45,
        "the feathered border is partial: {edge}"
    );
}

#[test]
fn select_to_path_traces_a_wand_selection_into_usable_path_data() {
    let mut f = fixture(16, 16);
    f.pixel_layer("lyr_a", |x, y| {
        if (4..12).contains(&x) && (4..12).contains(&y) {
            gray(0.0)
        } else {
            gray(1.0)
        }
    });
    f.ok(
        "raster.select.wand",
        json!({ "at": [8, 8], "tolerance": 0.05 }),
    );
    let effect = f.ok("raster.select.to-path", json!({}));
    let d = effect.data.unwrap()["d"].as_str().unwrap().to_string();
    let path = dpaint_core::kurbo::BezPath::from_svg(&d).expect("valid path data");
    let bb = dpaint_core::kurbo::Shape::bounding_box(&path);
    assert_eq!((bb.x0, bb.y0, bb.x1, bb.y1), (4.0, 4.0, 12.0, 12.0));
    assert!(
        f.doc().selection.as_ref().unwrap().d.is_some(),
        "the selection is now vector"
    );
}

#[test]
fn select_invert_flips_which_pixels_a_filter_touches() {
    let mut f = fixture(16, 16);
    let id = f.pixel_layer("lyr_a", |_, _| gray(0.5));
    let before = f.layer_pixels(&id);
    f.ok(
        "raster.select.rect",
        json!({ "rect": [0.0, 0.0, 8.0, 16.0] }),
    );
    f.ok("raster.select.invert", json!({}));
    f.ok("raster.adjust.invert", json!({ "target": "#lyr_a" }));
    let c = f.layer_pixels(&id);
    let i = c.idx(2, 8);
    assert_eq!(
        c.data[i..i + 4],
        before.data[i..i + 4],
        "left half was deselected"
    );
    assert!(c.get(13, 8)[0] < 0.2, "right half was inverted");
}

#[test]
fn text_with_a_missing_font_still_renders_and_reports_the_fallback() {
    let mut f = fixture(200, 60);
    let effect = f.ok(
        "raster.text.add",
        json!({
            "text": "Hello",
            "family": "Definitely Not Installed",
            "size": 36.0,
            "box": [4.0, 4.0, 190.0, 50.0],
        }),
    );
    let warning = effect
        .warnings
        .iter()
        .find(|w| w.code == "font-fallback")
        .expect("a missing family must be reported");
    assert!(
        warning.detail.contains(dpaint_core::FALLBACK_FAMILY),
        "the warning must name the face actually used: {}",
        warning.detail
    );
    assert_eq!(effect.created.len(), 1, "the layer was still created");

    // And it really put ink on the canvas.
    let pm = f.render(1.0);
    let inked = pm.pixels().iter().filter(|p| p.alpha() > 32).count();
    assert!(
        inked > 100,
        "the fallback font must still draw glyphs, got {inked} pixels"
    );
}

#[test]
fn text_fit_shrinks_until_the_string_fits_its_box() {
    let mut f = fixture(120, 40);
    f.ok(
        "raster.text.add",
        json!({
            "text": "shrink me to fit",
            "size": 48.0,
            "box": [0.0, 0.0, 110.0, 30.0],
            "name": "title",
        }),
    );
    let effect = f.ok("raster.text.fit", json!({ "target": "#lyr_title" }));
    let data = effect.data.unwrap();
    let to = data["to"].as_f64().unwrap();
    assert!(to < 48.0, "fit must shrink: {to}");
    let layer = f.doc().layer(&"lyr_title".into()).unwrap();
    let LayerKind::Text { spec, .. } = &layer.kind else {
        panic!("not text")
    };
    assert_eq!(spec.size, to);
}

#[test]
fn text_to_shape_produces_path_data_that_parses() {
    let mut f = fixture(120, 40);
    f.ok(
        "raster.text.add",
        json!({ "text": "Ag", "size": 30.0, "name": "word" }),
    );
    f.ok("raster.text.to-shape", json!({ "target": "#lyr_word" }));
    let layer = f.doc().layer(&"lyr_word".into()).unwrap();
    let LayerKind::Shape { d, .. } = &layer.kind else {
        panic!("expected a shape layer")
    };
    let path = dpaint_core::kurbo::BezPath::from_svg(d).expect("valid outline");
    let bb = dpaint_core::kurbo::Shape::bounding_box(&path);
    assert!(
        bb.width() > 10.0 && bb.height() > 10.0,
        "outline has real extent: {bb:?}"
    );
}

#[test]
fn adjust_as_layer_inserts_a_live_adjustment_above_its_target() {
    let mut f = fixture(8, 8);
    f.pixel_layer("lyr_a", |_, _| gray(0.5));
    let before = f.layer_asset(&"lyr_a".into());
    let effect = f.ok(
        "raster.adjust.hsl",
        json!({ "target": "#lyr_a", "lightness": -0.5, "as_layer": true }),
    );
    assert_eq!(effect.created.len(), 1);
    assert_eq!(f.doc().layers.len(), 2);
    assert!(
        matches!(f.doc().layers[1].kind, LayerKind::Adjustment { .. }),
        "the adjustment goes above the target"
    );
    assert_eq!(
        before,
        f.layer_asset(&"lyr_a".into()),
        "pixels were not touched"
    );
    let out = at(&f.render(1.0), 4, 4);
    assert!(out[0] < 0.5, "and it darkens the render: {out:?}");
}

#[test]
fn curves_levels_and_exposure_move_tone_in_the_directions_they_claim() {
    let mut f = fixture(4, 4);
    let id = f.pixel_layer("lyr_a", |_, _| gray(0.2));
    f.ok(
        "raster.adjust.curves",
        json!({ "target": "#lyr_a", "points": [[0.0, 0.0], [0.5, 0.8], [1.0, 1.0]] }),
    );
    let lifted = f.layer_pixels(&id).get(2, 2)[0];
    assert!(lifted > 0.2, "an upward curve brightens: {lifted}");

    let mut f = fixture(4, 4);
    let id = f.pixel_layer("lyr_a", |_, _| gray(0.2));
    f.ok(
        "raster.adjust.exposure",
        json!({ "target": "#lyr_a", "stops": 1.0 }),
    );
    let doubled = f.layer_pixels(&id).get(2, 2)[0];
    assert!(
        (doubled - 0.4).abs() < 0.01,
        "one stop doubles linear light: {doubled}"
    );

    let mut f = fixture(4, 4);
    let id = f.pixel_layer("lyr_a", |_, _| gray(0.5));
    f.ok(
        "raster.adjust.levels",
        json!({ "target": "#lyr_a", "in_black": 0.0, "in_white": 1.0, "gamma": 0.5 }),
    );
    let darker = f.layer_pixels(&id).get(2, 2)[0];
    assert!(darker < 0.5, "gamma below 1 darkens: {darker}");
}

#[test]
fn threshold_posterize_and_desaturate_quantize_and_gray_out() {
    let mut f = fixture(4, 4);
    let id = f.pixel_layer("lyr_a", |x, _| gray(x as f32 / 3.0));
    f.ok(
        "raster.adjust.threshold",
        json!({ "target": "#lyr_a", "level": 0.5 }),
    );
    let c = f.layer_pixels(&id);
    for x in 0..4 {
        let v = c.get(x, 0)[0];
        assert!(
            v == 0.0 || v == 1.0,
            "threshold must be binary, got {v} at {x}"
        );
    }

    let mut f = fixture(64, 1);
    let id = f.pixel_layer("lyr_a", |x, _| gray(x as f32 / 63.0));
    f.ok(
        "raster.adjust.posterize",
        json!({ "target": "#lyr_a", "levels": 3 }),
    );
    let c = f.layer_pixels(&id);
    let mut distinct: Vec<u32> = (0..64).map(|x| (c.get(x, 0)[0] * 1000.0) as u32).collect();
    distinct.sort_unstable();
    distinct.dedup();
    assert_eq!(distinct.len(), 3, "posterize to 3 levels leaves 3 tones");

    let mut f = fixture(4, 4);
    let id = f.pixel_layer("lyr_a", |_, _| [0.8, 0.1, 0.1, 1.0]);
    f.ok("raster.adjust.desaturate", json!({ "target": "#lyr_a" }));
    let px = f.layer_pixels(&id).get(2, 2);
    assert!(
        (px[0] - px[1]).abs() < 1e-6 && (px[1] - px[2]).abs() < 1e-6,
        "gray: {px:?}"
    );
}

#[test]
fn a_hald_lut_applies_the_cube_it_encodes() {
    // Build an inverting 16-step HALD cube: side 64, because 64^2 == 16^3.
    let n = 16usize;
    let side = 64u32;
    let mut img = image::RgbImage::new(side, side);
    for bi in 0..n {
        for gi in 0..n {
            for ri in 0..n {
                let index = ri + gi * n + bi * n * n;
                let (x, y) = ((index as u32) % side, (index as u32) / side);
                let q = |v: usize| (255.0 * (1.0 - v as f32 / (n - 1) as f32)) as u8;
                img.put_pixel(x, y, image::Rgb([q(ri), q(gi), q(bi)]));
            }
        }
    }
    let mut png = Vec::new();
    image::ImageEncoder::write_image(
        image::codecs::png::PngEncoder::new(&mut png),
        img.as_raw(),
        side,
        side,
        image::ExtendedColorType::Rgb8,
    )
    .unwrap();

    let mut f = fixture(4, 4);
    let asset = f.assets.put(&png, "png").unwrap();
    let id = f.pixel_layer("lyr_a", |_, _| [0.0, 0.0, 0.0, 1.0]);
    f.ok(
        "raster.adjust.lut",
        json!({ "target": "#lyr_a", "asset": asset.0 }),
    );
    let px = f.layer_pixels(&id).get(2, 2);
    assert!(
        px[0] > 0.9 && px[1] > 0.9,
        "an inverting cube must turn black into white: {px:?}"
    );
}

#[test]
fn a_bad_convolution_kernel_is_rejected_before_any_pixel_is_touched() {
    let mut f = fixture(8, 8);
    let id = f.pixel_layer("lyr_a", |_, _| gray(0.5));
    let before = f.layer_asset(&id);
    let err = f
        .run(
            "raster.filter.convolve",
            json!({ "target": "#lyr_a", "width": 2, "height": 2, "kernel": [1, 1, 1, 1] }),
        )
        .expect_err("an even kernel has no center");
    assert!(err.to_string().contains("odd"), "{err}");
    assert_eq!(
        before,
        f.layer_asset(&id),
        "a failed op must not change the document"
    );
}

#[test]
fn a_selector_that_matches_nothing_is_an_error_not_a_no_op() {
    let mut f = fixture(4, 4);
    f.pixel_layer("lyr_a", |_, _| gray(0.5));
    let err = f
        .run(
            "raster.filter.gaussian-blur",
            json!({ "target": "#lyr_nope", "sigma": 1.0 }),
        )
        .expect_err("missing selectors must fail loudly");
    assert!(
        matches!(err, dpaint_core::Error::SelectorNoMatch { .. }),
        "{err}"
    );
}

#[test]
fn filters_on_a_text_layer_say_to_rasterize_first() {
    let mut f = fixture(40, 20);
    f.ok("raster.text.add", json!({ "text": "hi", "name": "t" }));
    let err = f
        .run(
            "raster.filter.gaussian-blur",
            json!({ "target": "#lyr_t", "sigma": 1.0 }),
        )
        .expect_err("text layers have no pixels to filter");
    assert!(err.to_string().contains("rasterize"), "{err}");

    // And after rasterizing, the same filter works.
    f.ok("raster.layer.rasterize", json!({ "target": "#lyr_t" }));
    f.ok(
        "raster.filter.gaussian-blur",
        json!({ "target": "#lyr_t", "sigma": 1.0 }),
    );
}

#[test]
fn pixelate_flattens_detail_into_blocks() {
    let mut f = fixture(16, 16);
    let id = f.pixel_layer("lyr_a", |x, y| {
        gray(if (x + y) % 2 == 0 { 1.0 } else { 0.0 })
    });
    f.ok(
        "raster.filter.pixelate",
        json!({ "target": "#lyr_a", "size": 4 }),
    );
    let c = f.layer_pixels(&id);
    let first = c.get(0, 0);
    for y in 0..4 {
        for x in 0..4 {
            assert_eq!(c.get(x, y), first, "a block must be uniform");
        }
    }
    assert!(
        (first[0] - 0.5).abs() < 0.01,
        "and hold the block average: {first:?}"
    );
}

#[test]
fn edge_detect_finds_the_edge_and_ignores_the_flats() {
    let mut f = fixture(16, 16);
    let id = f.pixel_layer("lyr_a", |x, _| if x < 8 { gray(0.0) } else { gray(1.0) });
    f.ok("raster.filter.edge-detect", json!({ "target": "#lyr_a" }));
    let c = f.layer_pixels(&id);
    assert!(
        c.get(8, 8)[0] > 0.3,
        "the edge lights up: {:?}",
        c.get(8, 8)
    );
    assert!(
        c.get(2, 8)[0] < 0.05,
        "flat areas stay dark: {:?}",
        c.get(2, 8)
    );
    assert!(c.get(14, 8)[0] < 0.05, "both of them: {:?}", c.get(14, 8));
}

#[test]
fn noise_is_reproducible_from_its_seed_and_different_across_seeds() {
    let run = |seed: u64| {
        let mut f = fixture(16, 16);
        let id = f.pixel_layer("lyr_a", |_, _| gray(0.5));
        f.ok(
            "raster.filter.noise-add",
            json!({ "target": "#lyr_a", "amount": 0.2, "seed": seed }),
        );
        f.layer_pixels(&id).data
    };
    assert_eq!(run(7), run(7), "the same seed must give the same grain");
    assert_ne!(run(7), run(8), "a different seed must give different grain");
}

#[test]
fn morphology_grows_and_shrinks_a_shape_through_the_op_surface() {
    let mut f = fixture(32, 32);
    let id = f.pixel_layer("lyr_a", |x, y| {
        if (12..20).contains(&x) && (12..20).contains(&y) {
            gray(1.0)
        } else {
            [0.0; 4]
        }
    });
    let area = |c: &Canvas| c.data.chunks_exact(4).filter(|p| p[3] > 0.5).count();
    let before = area(&f.layer_pixels(&id));
    f.ok(
        "raster.filter.morphology",
        json!({ "target": "#lyr_a", "op": "dilate", "radius": 2 }),
    );
    let grown = area(&f.layer_pixels(&id));
    assert!(grown > before, "dilate grew {before} -> {grown}");
    f.ok(
        "raster.filter.morphology",
        json!({ "target": "#lyr_a", "op": "erode", "radius": 2 }),
    );
    assert!(area(&f.layer_pixels(&id)) < grown, "erode shrank it back");
}

#[test]
fn paint_stroke_deposits_color_along_its_path_and_nowhere_else() {
    let mut f = fixture(40, 40);
    let id = f.pixel_layer("lyr_a", |_, _| [0.0; 4]);
    f.ok(
        "raster.paint.stroke",
        json!({
            "target": "#lyr_a",
            "d": "M 5 20 L 35 20",
            "color": "#ff0000",
            "size": 6.0,
            "hardness": 1.0,
        }),
    );
    let c = f.layer_pixels(&id);
    let on = c.get(20, 20);
    assert!(
        on[3] > 0.9 && on[0] > 0.9 && on[1] < 0.05,
        "red paint on the path: {on:?}"
    );
    assert_eq!(c.get(20, 32)[3], 0.0, "and nothing far from it");
}

#[test]
fn bucket_fill_stays_inside_the_region_it_seeds() {
    let mut f = fixture(20, 20);
    // A vertical black wall down the middle of a white field.
    let id = f.pixel_layer("lyr_a", |x, _| if x == 10 { gray(0.0) } else { gray(1.0) });
    f.ok(
        "raster.paint.fill-bucket",
        json!({ "target": "#lyr_a", "color": "#0000ff", "at": [2, 10], "tolerance": 0.05 }),
    );
    let c = f.layer_pixels(&id);
    assert!(
        c.get(3, 10)[2] > 0.9 && c.get(3, 10)[0] < 0.05,
        "left side filled blue"
    );
    assert!(
        c.get(15, 10)[0] > 0.9,
        "right side untouched: {:?}",
        c.get(15, 10)
    );
}

#[test]
fn gradient_fill_ramps_across_the_layer_in_linear_light() {
    let mut f = fixture(101, 4);
    let id = f.pixel_layer("lyr_a", |_, _| [0.0; 4]);
    f.ok(
        "raster.paint.gradient",
        json!({
            "target": "#lyr_a",
            "kind": "linear",
            "from": [0.0, 0.0],
            "to": [101.0, 0.0],
            "stops": [
                { "offset": 0.0, "color": "#000000" },
                { "offset": 1.0, "color": "#ffffff" }
            ],
        }),
    );
    let c = f.layer_pixels(&id);
    assert!(c.get(0, 2)[0] < 0.02);
    assert!(
        (c.get(50, 2)[0] - 0.5).abs() < 0.03,
        "mid ramp is half the light"
    );
    assert!(c.get(100, 2)[0] > 0.97);
}

#[test]
fn erase_removes_coverage_where_the_brush_passes() {
    let mut f = fixture(40, 40);
    let id = f.pixel_layer("lyr_a", |_, _| gray(1.0));
    f.ok(
        "raster.paint.erase",
        json!({ "target": "#lyr_a", "d": "M 5 20 L 35 20", "size": 8.0, "hardness": 1.0 }),
    );
    let c = f.layer_pixels(&id);
    assert!(
        c.get(20, 20)[3] < 0.05,
        "erased on the path: {:?}",
        c.get(20, 20)
    );
    assert_eq!(c.get(20, 35)[3], 1.0, "intact away from it");
}

#[test]
fn pattern_fill_tiles_its_source_image() {
    let mut f = fixture(16, 16);
    let mut tile = Canvas::new(4, 4);
    for y in 0..4 {
        for x in 0..4 {
            let i = tile.idx(x, y);
            tile.set_straight(
                i,
                if x == 0 {
                    [1.0, 0.0, 0.0, 1.0]
                } else {
                    [0.0, 0.0, 1.0, 1.0]
                },
            );
        }
    }
    let asset = f.assets.put(&tile.to_png().unwrap(), "png").unwrap();
    let id = f.pixel_layer("lyr_a", |_, _| [0.0; 4]);
    f.ok(
        "raster.paint.pattern",
        json!({ "target": "#lyr_a", "asset": asset.0 }),
    );
    let c = f.layer_pixels(&id);
    for tx in [0u32, 4, 8, 12] {
        assert!(c.get(tx, 3)[0] > 0.9, "red stripe repeats at x={tx}");
        assert!(
            c.get(tx + 1, 3)[2] > 0.9,
            "blue between the stripes at x={}",
            tx + 1
        );
    }
}

#[test]
fn a_mask_from_the_selection_hides_everything_outside_it() {
    let mut f = fixture(16, 16);
    f.pixel_layer("lyr_a", |_, _| gray(1.0));
    f.ok(
        "raster.select.rect",
        json!({ "rect": [0.0, 0.0, 8.0, 16.0] }),
    );
    f.ok("raster.mask.from-selection", json!({ "target": "#lyr_a" }));
    let pm = f.render(1.0);
    assert_eq!(at(&pm, 2, 8)[3], 1.0, "inside the old selection survives");
    assert_eq!(at(&pm, 13, 8)[3], 0.0, "outside is masked away");

    // Applying it bakes the alpha into the pixels and drops the mask.
    f.ok("raster.mask.apply", json!({ "target": "#lyr_a" }));
    assert!(f.doc().layer(&"lyr_a".into()).unwrap().mask.is_none());
    let c = f.layer_pixels(&"lyr_a".into());
    assert_eq!(c.get(13, 8)[3], 0.0, "the pixels themselves are now clear");
}

#[test]
fn mask_from_luminance_turns_brightness_into_coverage() {
    let mut f = fixture(16, 16);
    f.pixel_layer("lyr_a", |x, _| if x < 8 { gray(1.0) } else { gray(0.0) });
    f.ok("raster.mask.from-luminance", json!({ "target": "#lyr_a" }));
    let pm = f.render(1.0);
    assert_eq!(at(&pm, 2, 8)[3], 1.0, "bright pixels stay");
    assert_eq!(at(&pm, 13, 8)[3], 0.0, "dark pixels are masked out");
    f.ok("raster.mask.invert", json!({ "target": "#lyr_a" }));
    let pm = f.render(1.0);
    assert_eq!(at(&pm, 2, 8)[3], 0.0, "inverting swaps them");
}

#[test]
fn clip_set_refuses_the_bottom_layer_and_works_above_it() {
    let mut f = fixture(8, 8);
    f.pixel_layer("lyr_base", |x, _| if x < 4 { gray(1.0) } else { [0.0; 4] });
    f.pixel_layer("lyr_top", |_, _| [1.0, 0.0, 0.0, 1.0]);
    let err = f
        .run("raster.clip.set", json!({ "target": "#lyr_base" }))
        .expect_err("nothing beneath the bottom layer");
    assert!(err.to_string().contains("beneath"), "{err}");
    f.ok("raster.clip.set", json!({ "target": "#lyr_top" }));
    let pm = f.render(1.0);
    assert!(at(&pm, 1, 4)[0] > 0.9);
    assert_eq!(at(&pm, 6, 4)[3], 0.0);
}

#[test]
fn layer_group_then_ungroup_keeps_the_same_render() {
    let mut f = fixture(8, 8);
    f.pixel_layer("lyr_a", |x, _| {
        if x < 4 {
            [1.0, 0.0, 0.0, 1.0]
        } else {
            [0.0; 4]
        }
    });
    f.pixel_layer("lyr_b", |x, _| {
        if x >= 4 {
            [0.0, 0.0, 1.0, 1.0]
        } else {
            [0.0; 4]
        }
    });
    let before = f.render(1.0).data().to_vec();
    f.ok(
        "raster.layer.group",
        json!({ "target": "pixel", "name": "both" }),
    );
    assert_eq!(f.doc().layers.len(), 1, "both layers moved into the group");
    assert_eq!(
        f.render(1.0).data(),
        &before[..],
        "grouping does not change pixels"
    );
    f.ok("raster.layer.ungroup", json!({ "target": "#lyr_both" }));
    assert_eq!(f.doc().layers.len(), 2);
    assert_eq!(
        f.render(1.0).data(),
        &before[..],
        "and neither does ungrouping"
    );
}

#[test]
fn merge_down_and_flatten_collapse_the_stack_without_changing_the_picture() {
    let mut f = fixture(8, 8);
    f.pixel_layer("lyr_a", |_, _| [1.0, 1.0, 1.0, 1.0]);
    let top = f.pixel_layer("lyr_b", |x, _| {
        if x < 4 {
            [1.0, 0.0, 0.0, 0.5]
        } else {
            [0.0; 4]
        }
    });
    f.doc_mut().layer_mut(&top).unwrap().opacity = 0.75;
    let before = f.render(1.0).data().to_vec();

    f.ok("raster.layer.merge-down", json!({ "target": "#lyr_b" }));
    assert_eq!(f.doc().layers.len(), 1, "two layers became one");
    let merged = f.render(1.0).data().to_vec();
    assert!(
        merged.iter().zip(&before).all(|(a, b)| a.abs_diff(*b) <= 1),
        "merge-down changed the picture"
    );

    f.ok("raster.doc.flatten", json!({}));
    assert_eq!(f.doc().layers.len(), 1);
    let flat = f.render(1.0).data().to_vec();
    assert!(
        flat.iter().zip(&merged).all(|(a, b)| a.abs_diff(*b) <= 1),
        "flatten changed it"
    );
}

#[test]
fn flatten_bakes_the_background_instead_of_doubling_it() {
    let mut f = fixture(4, 4);
    f.doc_mut().background = Some(dpaint_core::Color::rgba(0.0, 0.0, 1.0, 1.0));
    f.pixel_layer("lyr_a", |x, _| {
        if x < 2 {
            [1.0, 0.0, 0.0, 1.0]
        } else {
            [0.0; 4]
        }
    });
    let before = f.render(1.0).data().to_vec();
    f.ok("raster.doc.flatten", json!({}));
    assert!(
        f.doc().background.is_none(),
        "the background is in the pixels now"
    );
    assert_eq!(f.render(1.0).data(), &before[..]);
}

#[test]
fn layer_duplicate_gives_the_copy_fresh_ids() {
    let mut f = fixture(4, 4);
    f.pixel_layer("lyr_a", |_, _| gray(0.5));
    let effect = f.ok("raster.layer.duplicate", json!({ "target": "#lyr_a" }));
    assert_eq!(effect.created.len(), 1);
    let new_id = effect.created[0].clone();
    assert_ne!(new_id, "lyr_a");
    assert_eq!(f.doc().layers.len(), 2);
    assert_eq!(
        f.doc().layers[1].id.as_str(),
        new_id,
        "the copy sits above the original"
    );
}

#[test]
fn layer_reorder_moves_a_layer_within_its_siblings() {
    let mut f = fixture(4, 4);
    f.pixel_layer("lyr_a", |_, _| [1.0, 0.0, 0.0, 1.0]);
    f.pixel_layer("lyr_b", |_, _| [0.0, 1.0, 0.0, 1.0]);
    assert!(
        at(&f.render(1.0), 2, 2)[1] > 0.9,
        "green is on top to start with"
    );
    f.ok(
        "raster.layer.reorder",
        json!({ "target": "#lyr_a", "to": "front" }),
    );
    assert!(at(&f.render(1.0), 2, 2)[0] > 0.9, "red is on top now");
    f.ok(
        "raster.layer.reorder",
        json!({ "target": "#lyr_a", "index": 0 }),
    );
    assert!(at(&f.render(1.0), 2, 2)[1] > 0.9, "and back down again");
}

#[test]
fn layer_set_and_rename_change_only_what_they_name() {
    let mut f = fixture(4, 4);
    f.pixel_layer("lyr_a", |_, _| gray(1.0));
    f.ok(
        "raster.layer.set",
        json!({ "target": "#lyr_a", "opacity": 0.5 }),
    );
    assert!((at(&f.render(1.0), 2, 2)[3] - 0.5).abs() < 0.01);
    f.ok(
        "raster.layer.rename",
        json!({ "target": "#lyr_a", "name": "Sky" }),
    );
    assert_eq!(f.doc().layer(&"lyr_a".into()).unwrap().name, "Sky");
    assert!(
        f.run(
            "raster.layer.set",
            json!({ "target": "#lyr_a", "opacity": 2.0 })
        )
        .is_err(),
        "opacity outside 0..=1 must be refused"
    );
}

#[test]
fn layer_transform_can_bake_itself_into_pixels() {
    let mut f = fixture(16, 16);
    let id = f.pixel_layer("lyr_a", |x, y| {
        if (0..4).contains(&x) && (0..4).contains(&y) {
            gray(1.0)
        } else {
            [0.0; 4]
        }
    });
    f.ok(
        "raster.layer.transform",
        json!({ "target": "#lyr_a", "translate": [8.0, 8.0], "bake": true }),
    );
    assert!(
        f.doc().layer(&id).unwrap().transform.is_identity(),
        "the transform was baked"
    );
    let c = f.layer_pixels(&id);
    assert_eq!(c.get(1, 1)[3], 0.0);
    assert_eq!(c.get(9, 9)[3], 1.0, "the pixels themselves moved");
}

#[test]
fn layer_from_selection_lifts_and_can_cut() {
    let mut f = fixture(16, 16);
    let src = f.pixel_layer("lyr_a", |_, _| [1.0, 1.0, 1.0, 1.0]);
    f.ok(
        "raster.select.rect",
        json!({ "rect": [0.0, 0.0, 8.0, 16.0] }),
    );
    let effect = f.ok(
        "raster.layer.from-selection",
        json!({ "source": "#lyr_a", "name": "lifted", "cut": true }),
    );
    let new_id: dpaint_core::LayerId = effect.created[0].clone().into();
    let lifted = f.layer_pixels(&new_id);
    assert_eq!(lifted.get(2, 8)[3], 1.0, "the selected pixels came across");
    assert_eq!(lifted.get(13, 8)[3], 0.0, "and only those");
    let remaining = f.layer_pixels(&src);
    assert_eq!(
        remaining.get(2, 8)[3],
        0.0,
        "cut removed them from the source"
    );
    assert_eq!(remaining.get(13, 8)[3], 1.0);
}

#[test]
fn canvas_resize_scales_content_and_extend_keeps_it() {
    let mut f = fixture(16, 16);
    f.pixel_layer(
        "lyr_a",
        |x, y| if x < 8 && y < 8 { gray(1.0) } else { [0.0; 4] },
    );
    f.ok("raster.canvas.resize", json!({ "width": 32, "height": 32 }));
    assert_eq!(f.doc().size, [32, 32]);
    let pm = f.render(1.0);
    assert_eq!(
        at(&pm, 8, 8)[3],
        1.0,
        "scaled content covers twice the area"
    );
    assert_eq!(at(&pm, 20, 20)[3], 0.0);

    let mut f = fixture(16, 16);
    f.pixel_layer("lyr_a", |_, _| gray(1.0));
    f.ok(
        "raster.canvas.resize",
        json!({ "width": 32, "height": 32, "mode": "extend", "anchor": "top-left" }),
    );
    let pm = f.render(1.0);
    assert_eq!(at(&pm, 8, 8)[3], 1.0, "content kept its size");
    assert_eq!(at(&pm, 20, 20)[3], 0.0, "and the new page is empty");
}

#[test]
fn canvas_crop_trim_flip_and_rotate_move_the_page_as_advertised() {
    let mut f = fixture(20, 20);
    f.pixel_layer("lyr_a", |x, y| {
        if (4..12).contains(&x) && (4..12).contains(&y) {
            [1.0, 0.0, 0.0, 1.0]
        } else {
            [0.0; 4]
        }
    });
    f.ok(
        "raster.canvas.crop",
        json!({ "rect": [4.0, 4.0, 8.0, 8.0] }),
    );
    assert_eq!(f.doc().size, [8, 8]);
    let pm = f.render(1.0);
    assert_eq!(
        at(&pm, 0, 0)[3],
        1.0,
        "the cropped region starts at the origin"
    );
    assert_eq!(pm.width(), 8);

    // Trim finds the content bounds on its own.
    let mut f = fixture(20, 20);
    f.pixel_layer("lyr_a", |x, y| {
        if (6..10).contains(&x) && (2..18).contains(&y) {
            gray(1.0)
        } else {
            [0.0; 4]
        }
    });
    f.ok("raster.canvas.trim", json!({}));
    assert_eq!(f.doc().size, [4, 16]);

    // Flip mirrors, rotate turns and swaps the page dimensions.
    let mut f = fixture(8, 4);
    f.pixel_layer("lyr_a", |x, _| if x < 2 { gray(1.0) } else { [0.0; 4] });
    f.ok("raster.canvas.flip", json!({ "axis": "horizontal" }));
    let pm = f.render(1.0);
    assert_eq!(at(&pm, 0, 2)[3], 0.0);
    assert_eq!(at(&pm, 7, 2)[3], 1.0, "content moved to the right edge");
    f.ok("raster.canvas.rotate", json!({ "degrees": 90.0 }));
    assert_eq!(
        f.doc().size,
        [4, 8],
        "a quarter turn swaps width and height"
    );
}

#[test]
fn canvas_dpi_background_and_guides_are_recorded() {
    let mut f = fixture(4, 4);
    f.ok("raster.canvas.set-dpi", json!({ "dpi": 300.0 }));
    assert_eq!(f.doc().dpi, 300.0);
    assert!(f
        .run("raster.canvas.set-dpi", json!({ "dpi": 0.0 }))
        .is_err());
    f.ok(
        "raster.canvas.set-background",
        json!({ "color": "#102030" }),
    );
    assert_eq!(f.doc().background.unwrap().to_hex(), "#102030");
    f.ok("raster.canvas.set-background", json!({}));
    assert!(f.doc().background.is_none(), "omitting the color clears it");
    f.ok(
        "raster.canvas.set-guides",
        json!({ "bleed": 3.0, "vertical": [1.0, 2.0] }),
    );
    assert_eq!(f.doc().guides.bleed, 3.0);
    assert_eq!(f.doc().guides.vertical, vec![1.0, 2.0]);
    f.ok("raster.canvas.set-guides", json!({ "safe": 5.0 }));
    assert_eq!(
        f.doc().guides.bleed,
        3.0,
        "unspecified guide fields are left alone"
    );
    assert_eq!(f.doc().guides.safe, 5.0);
}

#[test]
fn effects_are_stored_live_and_removable() {
    let mut f = fixture(40, 40);
    let id = f.pixel_layer("lyr_a", |x, y| {
        if (10..30).contains(&x) && (10..30).contains(&y) {
            gray(1.0)
        } else {
            [0.0; 4]
        }
    });
    let asset_before = f.layer_asset(&id);
    f.ok(
        "raster.effect.drop-shadow",
        json!({ "target": "#lyr_a", "dx": 6.0, "dy": 6.0, "blur": 4.0, "color": "#000000" }),
    );
    assert_eq!(f.doc().layer(&id).unwrap().effects.len(), 1);
    assert_eq!(
        asset_before,
        f.layer_asset(&id),
        "effects do not touch pixels"
    );
    let pm = f.render(1.0);
    assert!(
        bytes_at(&pm, 33, 33)[3] > 40,
        "the shadow shows in the render"
    );

    // Asking twice replaces rather than stacking, by default.
    f.ok(
        "raster.effect.drop-shadow",
        json!({ "target": "#lyr_a", "dx": 2.0, "dy": 2.0, "blur": 2.0, "color": "#000000" }),
    );
    assert_eq!(f.doc().layer(&id).unwrap().effects.len(), 1);
    f.ok(
        "raster.effect.outer-glow",
        json!({ "target": "#lyr_a", "blur": 6.0, "color": "#00ff00" }),
    );
    assert_eq!(
        f.doc().layer(&id).unwrap().effects.len(),
        2,
        "different kinds coexist"
    );
    f.ok(
        "raster.effect.remove",
        json!({ "target": "#lyr_a", "effect": "drop-shadow" }),
    );
    assert_eq!(f.doc().layer(&id).unwrap().effects.len(), 1);
    f.ok("raster.effect.remove", json!({ "target": "#lyr_a" }));
    assert!(f.doc().layer(&id).unwrap().effects.is_empty());
    assert!(
        f.run("raster.effect.remove", json!({ "target": "#lyr_a" }))
            .is_err(),
        "removing nothing is a mistake worth reporting"
    );
}

#[test]
fn an_effect_stroke_rings_a_shape_in_the_render() {
    let mut f = fixture(40, 40);
    f.pixel_layer("lyr_a", |x, y| {
        if (10..30).contains(&x) && (10..30).contains(&y) {
            gray(1.0)
        } else {
            [0.0; 4]
        }
    });
    f.ok(
        "raster.effect.stroke",
        json!({ "target": "#lyr_a", "width": 3.0, "color": "#ff0000", "align": "outside" }),
    );
    let pm = f.render(1.0);
    let ring = at(&pm, 8, 20);
    assert!(
        ring[3] > 0.5 && ring[0] > 0.5 && ring[1] < 0.1,
        "red ring outside: {ring:?}"
    );
    assert!(at(&pm, 20, 20)[1] > 0.9, "the shape is still white inside");
}

#[test]
fn layer_add_builds_every_kind_and_rejects_missing_parameters() {
    let mut f = fixture(20, 20);
    f.ok("raster.layer.add", json!({ "type": "pixel", "name": "px" }));
    f.ok(
        "raster.layer.add",
        json!({ "type": "fill", "color": "#ff8800", "name": "fl" }),
    );
    f.ok(
        "raster.layer.add",
        json!({
            "type": "gradient",
            "name": "gr",
            "paint": {
                "type": "linear",
                "from": [0.0, 0.0],
                "to": [20.0, 0.0],
                "stops": [
                    { "offset": 0.0, "color": "#000000" },
                    { "offset": 1.0, "color": "#ffffff" }
                ]
            }
        }),
    );
    f.ok(
        "raster.layer.add",
        json!({ "type": "shape", "d": "M2 2 L18 2 L18 18 Z", "name": "sh" }),
    );
    f.ok(
        "raster.layer.add",
        json!({ "type": "adjustment", "name": "ad", "adjustment": { "kind": "invert" } }),
    );
    f.ok("raster.layer.add", json!({ "type": "group", "name": "gp" }));
    f.ok(
        "raster.layer.add",
        json!({ "type": "pixel", "name": "child", "parent": "#lyr_gp" }),
    );
    assert_eq!(f.doc().walk().len(), 7, "six roots plus one child");
    let group = f.doc().layer(&"lyr_gp".into()).unwrap();
    let LayerKind::Group { layers } = &group.kind else {
        panic!("not a group")
    };
    assert_eq!(layers.len(), 1, "the child went into the group");

    assert!(
        f.run("raster.layer.add", json!({ "type": "fill" }))
            .is_err(),
        "a fill layer without a color must be refused"
    );
    assert!(
        f.run(
            "raster.layer.add",
            json!({ "type": "shape", "d": "not a path" })
        )
        .is_err(),
        "bad path data must be refused"
    );
    assert!(
        f.run(
            "raster.layer.add",
            json!({ "type": "linked", "document": DOC, "box": [0.0, 0.0, 4.0, 4.0] })
        )
        .is_err(),
        "a document may not link itself"
    );
    // The render still works with all of those layers in the stack.
    assert_eq!(f.render(1.0).width(), 20);
}

#[test]
fn layer_remove_reports_what_it_removed() {
    let mut f = fixture(4, 4);
    f.pixel_layer("lyr_a", |_, _| gray(0.5));
    f.pixel_layer("lyr_b", |_, _| gray(0.5));
    let effect = f.ok("raster.layer.remove", json!({ "target": "pixel" }));
    assert_eq!(effect.removed.len(), 2);
    assert!(f.doc().layers.is_empty());
}

#[test]
fn a_dry_run_validates_without_changing_anything() {
    let mut f = fixture(8, 8);
    let id = f.pixel_layer("lyr_a", |_, _| gray(0.5));
    let before = f.layer_asset(&id);
    let op = f.registry.get("raster.filter.gaussian-blur").unwrap();
    let mut cx = dpaint_core::OpCx::new(&f.assets).with_doc(Some(DOC.to_string()));
    cx.dry_run = true;
    op.apply(
        &mut f.project,
        json!({ "target": "#lyr_a", "sigma": 2.0 }),
        &mut cx,
    )
    .expect("dry run succeeds");
    assert_eq!(
        before,
        f.layer_asset(&id),
        "a dry run must not repoint the layer"
    );
}

#[test]
fn channel_op_moves_luminance_into_alpha() {
    let mut f = fixture(8, 8);
    let id = f.pixel_layer("lyr_a", |x, _| if x < 4 { gray(1.0) } else { gray(0.0) });
    f.ok(
        "raster.filter.channel-op",
        json!({ "target": "#lyr_a", "op": "copy", "source": "luma", "dest": "alpha" }),
    );
    let c = f.layer_pixels(&id);
    assert!(c.get(1, 4)[3] > 0.95, "bright pixels became opaque");
    assert!(c.get(6, 4)[3] < 0.05, "dark pixels became transparent");
}

#[test]
fn displace_pushes_pixels_by_its_map() {
    let mut f = fixture(32, 32);
    // Map: constant shift to the right (red below 0.5 means negative x sampling offset).
    let mut map = Canvas::new(32, 32);
    for i in (0..map.data.len()).step_by(4) {
        map.set_straight(i, [1.0, 0.5, 0.0, 1.0]);
    }
    let asset = f.assets.put(&map.to_png().unwrap(), "png").unwrap();
    let id = f.pixel_layer("lyr_a", |x, _| if x >= 16 { gray(1.0) } else { [0.0; 4] });
    f.ok(
        "raster.filter.displace",
        json!({ "target": "#lyr_a", "map": asset.0, "scale_x": 8.0, "scale_y": 0.0 }),
    );
    let c = f.layer_pixels(&id);
    assert!(
        c.get(10, 16)[3] > 0.9,
        "the edge moved left by the map's shift"
    );
    assert!(c.get(6, 16)[3] < 0.1, "but not further than asked");
}

#[test]
fn dither_reduces_a_ramp_to_two_tones_that_still_read_as_a_ramp() {
    let mut f = fixture(64, 8);
    let id = f.pixel_layer("lyr_a", |x, _| gray(x as f32 / 63.0));
    f.ok(
        "raster.filter.dither",
        json!({ "target": "#lyr_a", "levels": 2, "matrix": 8 }),
    );
    let c = f.layer_pixels(&id);
    for i in (0..c.data.len()).step_by(4) {
        let v = c.data[i];
        assert!(
            !(1e-6..=1.0 - 1e-6).contains(&v),
            "two levels only, got {v}"
        );
    }
    let lit = |x0: u32, x1: u32| {
        (x0..x1)
            .flat_map(|x| (0..8).map(move |y| (x, y)))
            .filter(|(x, y)| c.get(*x, *y)[0] > 0.5)
            .count()
    };
    assert!(
        lit(0, 16) < lit(48, 64),
        "the dark end must stay darker than the bright end"
    );
}

#[test]
fn unsharp_and_sharpen_put_local_contrast_back_after_a_blur() {
    let mut f = fixture(32, 32);
    let id = f.pixel_layer("lyr_a", |x, y| {
        gray(if (x / 4 + y / 4) % 2 == 0 { 0.9 } else { 0.1 })
    });
    f.ok(
        "raster.filter.gaussian-blur",
        json!({ "target": "#lyr_a", "sigma": 2.0 }),
    );
    let blurred = luma_variance(&f.layer_pixels(&id));
    f.ok(
        "raster.filter.unsharp",
        json!({ "target": "#lyr_a", "sigma": 2.0, "amount": 1.5 }),
    );
    let sharpened = luma_variance(&f.layer_pixels(&id));
    assert!(
        sharpened > blurred,
        "unsharp must raise local contrast: {blurred} -> {sharpened}"
    );
}

#[test]
fn noise_reduce_removes_speckles_but_keeps_the_edge() {
    let mut f = fixture(32, 32);
    let id = f.pixel_layer("lyr_a", |x, y| {
        let base = if x < 16 { 0.15f32 } else { 0.85 };
        // A few isolated speckles.
        if (x % 7 == 3) && (y % 5 == 2) {
            gray(if base > 0.5 { 0.1 } else { 0.9 })
        } else {
            gray(base)
        }
    });
    let before = f.layer_pixels(&id);
    f.ok(
        "raster.filter.noise-reduce",
        json!({ "target": "#lyr_a", "radius": 1, "threshold": 1.0 }),
    );
    let after = f.layer_pixels(&id);
    let speckles = |c: &Canvas| {
        (1..31u32)
            .flat_map(|x| (1..31u32).map(move |y| (x, y)))
            .filter(|(x, y)| {
                let v = c.get(*x, *y)[0];
                let expect = if *x < 16 { 0.15 } else { 0.85 };
                (v - expect).abs() > 0.3
            })
            .count()
    };
    assert!(speckles(&before) > 10, "the fixture has speckles to remove");
    assert!(
        speckles(&after) < speckles(&before) / 2,
        "most speckles should be gone"
    );
    // The edge between the two fields survives.
    assert!(
        after.get(14, 20)[0] < 0.3 && after.get(18, 20)[0] > 0.6,
        "edge preserved"
    );
}

#[test]
fn radial_and_motion_blur_smear_without_losing_the_image() {
    let mut f = fixture(32, 32);
    let id = f.pixel_layer("lyr_a", |x, y| {
        gray(if (x / 4 + y / 4) % 2 == 0 { 1.0 } else { 0.0 })
    });
    let sharp = luma_variance(&f.layer_pixels(&id));
    f.ok(
        "raster.filter.motion-blur",
        json!({ "target": "#lyr_a", "distance": 8.0, "angle": 0.0 }),
    );
    let motion = luma_variance(&f.layer_pixels(&id));
    assert!(
        motion < sharp,
        "motion blur must reduce variance: {sharp} -> {motion}"
    );

    let mut f = fixture(32, 32);
    let id = f.pixel_layer("lyr_a", |x, y| {
        gray(if (x / 4 + y / 4) % 2 == 0 { 1.0 } else { 0.0 })
    });
    f.ok(
        "raster.filter.radial-blur",
        json!({ "target": "#lyr_a", "mode": "spin", "amount": 20.0 }),
    );
    assert!(
        luma_variance(&f.layer_pixels(&id)) < sharp,
        "spin blur must smear too"
    );
}

#[test]
fn color_range_and_alpha_selections_scope_later_edits() {
    let mut f = fixture(16, 16);
    let id = f.pixel_layer("lyr_a", |x, _| {
        if x < 8 {
            [1.0, 0.0, 0.0, 1.0]
        } else {
            [0.0, 0.0, 1.0, 1.0]
        }
    });
    f.ok(
        "raster.select.color-range",
        json!({ "color": "#ff0000", "tolerance": 0.2 }),
    );
    f.ok("raster.adjust.desaturate", json!({ "target": "#lyr_a" }));
    let c = f.layer_pixels(&id);
    let left = c.get(2, 8);
    assert!(
        (left[0] - left[2]).abs() < 1e-5,
        "the red field went gray: {left:?}"
    );
    assert!(c.get(13, 8)[2] > 0.9, "the blue field is untouched");

    // Alpha selections come from a layer's own coverage.
    let mut f = fixture(16, 16);
    f.pixel_layer("lyr_shape", |x, _| if x < 4 { gray(1.0) } else { [0.0; 4] });
    let target = f.pixel_layer("lyr_b", |_, _| [0.0, 0.0, 1.0, 1.0]);
    f.ok("raster.select.alpha", json!({ "target": "#lyr_shape" }));
    f.ok("raster.adjust.invert", json!({ "target": "#lyr_b" }));
    let c = f.layer_pixels(&target);
    assert!(
        c.get(1, 8)[0] > 0.9,
        "inverted inside the alpha selection: {:?}",
        c.get(1, 8)
    );
    assert!(c.get(10, 8)[0] < 0.1, "untouched outside it");
}

#[test]
fn select_text_selects_the_glyphs_of_a_text_layer() {
    let mut f = fixture(120, 60);
    f.ok(
        "raster.text.add",
        json!({ "text": "OO", "size": 48.0, "name": "word" }),
    );
    let effect = f.ok("raster.select.text", json!({ "target": "#lyr_word" }));
    assert!(
        effect.warnings.is_empty(),
        "the default family is available"
    );
    let sel = f.doc().selection.clone().expect("a selection was stored");
    assert!(sel.d.is_some(), "glyph outlines stay vector");
    assert!(
        sel.bounds.w() > 20.0 && sel.bounds.h() > 10.0,
        "bounds: {:?}",
        sel.bounds.0
    );
}

#[test]
fn select_none_clears_and_all_covers_the_page() {
    let mut f = fixture(8, 8);
    let id = f.pixel_layer("lyr_a", |_, _| gray(0.5));
    f.ok(
        "raster.select.rect",
        json!({ "rect": [0.0, 0.0, 2.0, 2.0] }),
    );
    f.ok("raster.select.all", json!({}));
    f.ok("raster.adjust.invert", json!({ "target": "#lyr_a" }));
    assert!(
        f.layer_pixels(&id).get(7, 7)[0] < 0.2,
        "select.all reaches the far corner"
    );
    f.ok("raster.select.none", json!({}));
    assert!(f.doc().selection.is_none());
    assert!(
        f.run("raster.select.invert", json!({})).is_err(),
        "inverting nothing has no useful meaning"
    );
}

#[test]
fn mask_add_starts_from_white_or_black_and_can_be_removed() {
    let mut f = fixture(8, 8);
    f.pixel_layer("lyr_a", |_, _| gray(1.0));
    f.ok(
        "raster.mask.add",
        json!({ "target": "#lyr_a", "from": "black" }),
    );
    assert_eq!(
        at(&f.render(1.0), 4, 4)[3],
        0.0,
        "a black mask hides the layer"
    );
    f.ok(
        "raster.mask.add",
        json!({ "target": "#lyr_a", "from": "white" }),
    );
    assert_eq!(at(&f.render(1.0), 4, 4)[3], 1.0, "a white mask reveals it");
    f.ok("raster.mask.remove", json!({ "target": "#lyr_a" }));
    assert!(f.doc().layer(&"lyr_a".into()).unwrap().mask.is_none());
    assert!(
        f.run("raster.mask.remove", json!({ "target": "#lyr_a" }))
            .is_err(),
        "removing a mask that is not there is worth reporting"
    );
}

#[test]
fn box_blur_and_sharpen_trade_local_contrast_in_opposite_directions() {
    let checker = |x: u32, y: u32| gray(if (x / 3 + y / 3) % 2 == 0 { 0.9 } else { 0.1 });
    let mut f = fixture(24, 24);
    let id = f.pixel_layer("lyr_a", checker);
    let sharp = luma_variance(&f.layer_pixels(&id));
    f.ok(
        "raster.filter.box-blur",
        json!({ "target": "#lyr_a", "radius": 2, "iterations": 2 }),
    );
    let blurred = luma_variance(&f.layer_pixels(&id));
    assert!(
        blurred < sharp * 0.6,
        "box blur must smooth: {sharp} -> {blurred}"
    );
    f.ok(
        "raster.filter.sharpen",
        json!({ "target": "#lyr_a", "amount": 1.0 }),
    );
    assert!(
        luma_variance(&f.layer_pixels(&id)) > blurred,
        "sharpen must push contrast back up"
    );
}

#[test]
fn ellipse_and_path_selections_scope_edits_to_their_interior() {
    let mut f = fixture(40, 40);
    let id = f.pixel_layer("lyr_a", |_, _| gray(0.5));
    let before = f.layer_pixels(&id);
    f.ok(
        "raster.select.ellipse",
        json!({ "center": [20.0, 20.0], "radius": [12.0, 6.0] }),
    );
    f.ok("raster.adjust.invert", json!({ "target": "#lyr_a" }));
    let c = f.layer_pixels(&id);
    assert!(c.get(20, 20)[0] < 0.2, "the ellipse center was inverted");
    let i = c.idx(20, 4);
    assert_eq!(
        c.data[i..i + 4],
        before.data[i..i + 4],
        "outside the ellipse is untouched"
    );

    let mut f = fixture(40, 40);
    let id = f.pixel_layer("lyr_a", |_, _| gray(0.5));
    f.ok(
        "raster.select.path",
        json!({ "d": "M 2 2 L 18 2 L 18 18 L 2 18 Z" }),
    );
    f.ok("raster.adjust.invert", json!({ "target": "#lyr_a" }));
    let c = f.layer_pixels(&id);
    assert!(c.get(10, 10)[0] < 0.2, "inside the path");
    assert!(c.get(30, 30)[0] > 0.4, "outside it");
}

#[test]
fn inner_shadow_darkens_inside_the_edge_and_effect_blur_softens_it() {
    let mut f = fixture(40, 40);
    f.pixel_layer("lyr_a", |x, y| {
        if (10..30).contains(&x) && (10..30).contains(&y) {
            gray(1.0)
        } else {
            [0.0; 4]
        }
    });
    let plain = bytes_at(&f.render(1.0), 11, 20);
    f.ok(
        "raster.effect.inner-shadow",
        json!({ "target": "#lyr_a", "dx": 4.0, "dy": 0.0, "blur": 4.0, "color": "#000000" }),
    );
    let shadowed = bytes_at(&f.render(1.0), 11, 20);
    assert!(
        shadowed[0] < plain[0],
        "the inside edge darkened: {plain:?} -> {shadowed:?}"
    );
    assert_eq!(
        bytes_at(&f.render(1.0), 5, 20)[3],
        0,
        "an inner shadow never spills outside"
    );

    f.ok("raster.effect.remove", json!({ "target": "#lyr_a" }));
    f.ok(
        "raster.effect.blur",
        json!({ "target": "#lyr_a", "radius": 8.0 }),
    );
    let a = at(&f.render(1.0), 8, 20)[3];
    assert!(
        a > 0.0 && a < 1.0,
        "a live blur softens the layer's edge: {a}"
    );
}

#[test]
fn hsl_color_balance_and_channel_mixer_move_color_where_they_say() {
    let mut f = fixture(4, 4);
    let id = f.pixel_layer("lyr_a", |_, _| [0.5, 0.1, 0.1, 1.0]);
    f.ok(
        "raster.adjust.hsl",
        json!({ "target": "#lyr_a", "hue": 120.0 }),
    );
    let px = f.layer_pixels(&id).get(2, 2);
    assert!(
        px[1] > px[0] && px[1] > px[2],
        "rotating red by 120 degrees gives green: {px:?}"
    );

    let mut f = fixture(4, 4);
    let id = f.pixel_layer("lyr_a", |_, _| gray(0.5));
    f.ok(
        "raster.adjust.color-balance",
        json!({ "target": "#lyr_a", "midtones": [0.2, 0.0, -0.2] }),
    );
    let px = f.layer_pixels(&id).get(2, 2);
    assert!(px[0] > px[1] && px[1] > px[2], "warm midtones: {px:?}");

    let mut f = fixture(4, 4);
    let id = f.pixel_layer("lyr_a", |_, _| [0.8, 0.2, 0.0, 1.0]);
    f.ok(
        "raster.adjust.channel-mixer",
        json!({ "target": "#lyr_a", "matrix": [[0.0, 0.0, 1.0], [0.0, 1.0, 0.0], [1.0, 0.0, 0.0]] }),
    );
    let px = f.layer_pixels(&id).get(2, 2);
    assert!(px[2] > 0.7 && px[0] < 0.05, "red and blue swapped: {px:?}");
}

#[test]
fn a_pressure_curve_tapers_the_stroke_it_paints() {
    let mut f = fixture(60, 40);
    let id = f.pixel_layer("lyr_a", |_, _| [0.0; 4]);
    f.ok(
        "raster.paint.stroke",
        json!({
            "target": "#lyr_a",
            "d": "M 5 20 L 55 20",
            "color": "#000000",
            "size": 12.0,
            "hardness": 1.0,
            "spacing": 0.1,
            "pressure": [[0.0, 0.1], [0.5, 1.0], [1.0, 0.1]],
        }),
    );
    let c = f.layer_pixels(&id);
    let solid = |x: u32| (0..40u32).filter(|y| c.get(x, *y)[3] > 0.5).count();
    let any_ink = |x: u32| (0..40u32).filter(|y| c.get(x, *y)[3] > 0.01).count();
    assert!(
        solid(30) > solid(6) + 4,
        "the middle must be fatter than the taper: {} vs {}",
        solid(6),
        solid(30)
    );
    assert!(
        any_ink(6) > 0,
        "the tapered end still lays down faint paint"
    );
    assert!(
        any_ink(6) < any_ink(30),
        "and covers less of the column than the middle"
    );
}
