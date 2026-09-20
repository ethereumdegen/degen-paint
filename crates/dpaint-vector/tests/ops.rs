//! End-to-end behaviour of the `vector.*` op catalog, driven exactly the way the CLI and
//! the MCP server drive it: an id, a JSON argument object, and the resulting document.

use dpaint_core::doc::vector::{VKind, VectorDoc};
use dpaint_core::doc::Document;
use dpaint_core::ids::{DocId, ObjectId};
use dpaint_core::{AssetStore, DocKind, OpCx, OpEffect, Project, Registry};
use serde_json::{json, Value};

struct Fixture {
    project: Project,
    registry: Registry,
    assets: AssetStore,
    _tmp: tempfile::TempDir,
    doc: DocId,
}

impl Fixture {
    fn new(w: f64, h: f64) -> Self {
        let v = VectorDoc::new(DocId::from("doc_v"), "v", w, h);
        let doc = v.id.clone();
        let project = Project::new("t", Document::Vector(v));
        let mut registry = Registry::new();
        registry.extend(dpaint_vector::ops());
        let tmp = tempfile::tempdir().unwrap();
        let assets = AssetStore::new(tmp.path());
        Self {
            project,
            registry,
            assets,
            _tmp: tmp,
            doc,
        }
    }

    fn run(&mut self, id: &str, args: Value) -> dpaint_core::Result<OpEffect> {
        let op = self.registry.get(id)?;
        let mut cx = OpCx::new(&self.assets);
        op.apply(&mut self.project, args, &mut cx)
    }

    fn must(&mut self, id: &str, args: Value) -> OpEffect {
        self.run(id, args)
            .unwrap_or_else(|e| panic!("{id} failed: {e}"))
    }

    fn vec(&self) -> &VectorDoc {
        self.project.vector(&self.doc).unwrap()
    }

    fn render(&self, scale: f64) -> tiny_skia::Pixmap {
        dpaint_vector::render_doc(&self.project, &self.doc, &self.assets, scale).unwrap()
    }

    fn covered(&self, scale: f64) -> usize {
        self.render(scale)
            .pixels()
            .iter()
            .filter(|p| p.alpha() > 128)
            .count()
    }

    fn path(&self, id: &str) -> dpaint_core::kurbo::BezPath {
        dpaint_vector::path_of(&self.project, &self.doc, &ObjectId::from(id)).unwrap()
    }
}

fn area(p: &dpaint_core::kurbo::BezPath) -> f64 {
    use dpaint_core::kurbo::Shape;
    p.area().abs()
}

// ---------------------------------------------------------------------------- catalog

#[test]
fn the_catalog_registers_cleanly_and_marks_only_measure_ops_as_queries() {
    let ops = dpaint_vector::ops();
    assert!(ops.len() >= 60, "the v1 catalog is {} ops", ops.len());
    let mut reg = Registry::new();
    reg.extend(ops);
    for op in reg.iter() {
        assert!(
            op.id().starts_with("vector."),
            "'{}' is outside this crate's domain",
            op.id()
        );
        assert_eq!(op.modes(), &[DocKind::Vector], "{}", op.id());
        assert_eq!(
            op.is_query(),
            op.id().starts_with("vector.measure."),
            "query flag is wrong for {}",
            op.id()
        );
        assert!(
            op.schema().get("properties").is_some() || op.schema().get("type").is_some(),
            "{} has no usable schema",
            op.id()
        );
    }
}

#[test]
fn a_bad_selector_fails_before_anything_is_mutated() {
    let mut f = Fixture::new(100.0, 100.0);
    f.must(
        "vector.object.add-rect",
        json!({"x": 0, "y": 0, "width": 10, "height": 10, "name": "box"}),
    );
    let before = serde_json::to_value(f.vec()).unwrap();
    let err = f
        .run(
            "vector.style.fill",
            json!({"target": "#nope", "color": "#ff0000"}),
        )
        .unwrap_err();
    assert_eq!(err.code(), "selector_no_match");
    assert_eq!(serde_json::to_value(f.vec()).unwrap(), before);
}

// ----------------------------------------------------------------------------- shapes

#[test]
fn adding_shapes_creates_addressable_objects_with_the_requested_geometry() {
    let mut f = Fixture::new(200.0, 200.0);
    let eff = f.must(
        "vector.object.add-rect",
        json!({"x": 10, "y": 10, "width": 40, "height": 20, "fill": "#ff0000", "name": "box"}),
    );
    assert_eq!(eff.created, vec!["obj_box".to_string()]);
    assert!((area(&f.path("obj_box")) - 800.0).abs() < 1e-6);

    f.must(
        "vector.object.add-star",
        json!({"cx": 100, "cy": 100, "outer": 40, "inner": 16, "points": 5, "name": "star"}),
    );
    let star = f.path("obj_star");
    assert_eq!(
        star.segments().count(),
        10,
        "a five-pointed star has ten edges"
    );

    f.must(
        "vector.object.add-ellipse",
        json!({"cx": 50, "cy": 150, "rx": 20, "name": "dot"}),
    );
    let a = area(&f.path("obj_dot"));
    assert!(
        (a - std::f64::consts::PI * 400.0).abs() < 1.0,
        "circle area {a}"
    );
}

#[test]
fn a_degenerate_shape_is_refused_with_a_geometry_error() {
    let mut f = Fixture::new(100.0, 100.0);
    let err = f
        .run(
            "vector.object.add-polygon",
            json!({"cx": 0, "cy": 0, "radius": 10, "sides": 2}),
        )
        .unwrap_err();
    assert_eq!(err.code(), "degenerate_geometry");
}

// --------------------------------------------------------------------------- booleans

#[test]
fn subtracting_a_circle_from_a_square_removes_the_circle_from_the_render() {
    let mut f = Fixture::new(100.0, 100.0);
    f.must(
        "vector.object.add-rect",
        json!({"x": 0, "y": 0, "width": 100, "height": 100, "fill": "#000000", "name": "sq"}),
    );
    let before = f.covered(1.0);
    f.must(
        "vector.object.add-ellipse",
        json!({"cx": 50, "cy": 50, "rx": 20, "fill": "#000000", "name": "hole"}),
    );
    f.must(
        "vector.path.boolean",
        json!({"target": "#obj_sq, #obj_hole", "op": "subtract", "tolerance": 0.05}),
    );
    assert!(
        f.vec().object(&ObjectId::from("obj_hole")).is_none(),
        "operand consumed"
    );
    let after = f.covered(1.0);
    let circle = std::f64::consts::PI * 400.0;
    assert!(
        ((before - after) as f64 - circle).abs() < 60.0,
        "coverage dropped by {} px, the circle is {circle:.0}",
        before - after
    );
}

#[test]
fn union_of_two_overlapping_circles_covers_less_than_both_separately() {
    let mut f = Fixture::new(100.0, 100.0);
    f.must(
        "vector.object.add-ellipse",
        json!({"cx": 40, "cy": 50, "rx": 25, "fill": "#000000", "name": "a"}),
    );
    f.must(
        "vector.object.add-ellipse",
        json!({"cx": 60, "cy": 50, "rx": 25, "fill": "#000000", "name": "b"}),
    );
    f.must(
        "vector.path.boolean",
        json!({"target": "#obj_a, #obj_b", "op": "union"}),
    );
    let u = area(&f.path("obj_a"));
    let one = std::f64::consts::PI * 625.0;
    assert!(
        u < 2.0 * one - 100.0,
        "union {u} is less than {}",
        2.0 * one
    );
    assert!(u > one, "and more than a single circle {one}");
}

#[test]
fn divide_replaces_the_subject_with_its_pieces() {
    let mut f = Fixture::new(100.0, 100.0);
    f.must(
        "vector.object.add-rect",
        json!({"x": 0, "y": 0, "width": 40, "height": 40, "fill": "#000000", "name": "sq"}),
    );
    f.must(
        "vector.object.add-rect",
        json!({"x": 20, "y": -10, "width": 40, "height": 60, "fill": "#000000", "name": "knife"}),
    );
    let eff = f.must(
        "vector.path.boolean",
        json!({"target": "#obj_sq, #obj_knife", "op": "divide"}),
    );
    assert_eq!(
        eff.created.len(),
        2,
        "one piece inside the cutter, one outside"
    );
    let total: f64 = eff.created.iter().map(|id| area(&f.path(id))).sum();
    assert!(
        (total - 1600.0).abs() < 1.0,
        "the pieces reassemble the square: {total}"
    );
}

// ------------------------------------------------------------------------- path edits

#[test]
fn outline_stroke_turns_a_line_into_a_fillable_band() {
    let mut f = Fixture::new(200.0, 50.0);
    f.must(
        "vector.object.add-line",
        json!({"x1": 10, "y1": 25, "x2": 110, "y2": 25, "stroke": "#000000", "stroke_width": 8, "name": "l"}),
    );
    f.must("vector.path.outline-stroke", json!({"target": "#obj_l"}));
    let o = f.vec().object(&ObjectId::from("obj_l")).unwrap();
    assert!(o.stroke.is_none(), "the stroke became a fill");
    assert!(
        !o.fill.is_none(),
        "the outline is filled with the stroke paint"
    );
    let a = area(&f.path("obj_l"));
    assert!((a - 800.0).abs() < 1.0, "100 long x 8 wide = 800, got {a}");
}

#[test]
fn offsetting_outward_grows_the_bounding_box_by_the_distance_on_every_side() {
    use dpaint_core::kurbo::Shape;
    let mut f = Fixture::new(200.0, 200.0);
    f.must(
        "vector.object.add-rect",
        json!({"x": 50, "y": 60, "width": 40, "height": 30, "fill": "#000000", "name": "r"}),
    );
    f.must(
        "vector.path.offset",
        json!({"target": "#obj_r", "distance": 6, "tolerance": 0.02}),
    );
    let b = f.path("obj_r").bounding_box();
    assert!((b.x0 - 44.0).abs() < 0.1, "left {}", b.x0);
    assert!((b.y0 - 54.0).abs() < 0.1, "top {}", b.y0);
    assert!((b.x1 - 96.0).abs() < 0.1, "right {}", b.x1);
    assert!((b.y1 - 96.0).abs() < 0.1, "bottom {}", b.y1);
}

#[test]
fn simplify_drops_nodes_and_keeps_the_shape_within_tolerance() {
    use dpaint_core::kurbo::Shape;
    let mut f = Fixture::new(300.0, 300.0);
    let dense = dpaint_vector::geom::subpaths_to_bez(&dpaint_vector::geom::flatten(
        &dpaint_core::kurbo::Circle::new((150.0, 150.0), 100.0).to_path(1e-6),
        0.002,
    ));
    f.must(
        "vector.object.add-path",
        json!({"d": dpaint_vector::geom::to_d(&dense), "fill": "#000000", "name": "c"}),
    );
    let before = f.path("obj_c").segments().count();
    f.must(
        "vector.path.simplify",
        json!({"target": "#obj_c", "tolerance": 1.0}),
    );
    let after = f.path("obj_c");
    assert!(
        after.segments().count() * 4 < before,
        "{} from {before}",
        after.segments().count()
    );
    let dev = dpaint_vector::geom::flatten(&dense, 0.1)
        .iter()
        .flat_map(|sp| sp.points.clone())
        .map(|p| dpaint_vector::geom::distance_to(&after, p))
        .fold(0.0, f64::max);
    assert!(
        dev <= 1.0,
        "max deviation {dev} stays inside the stated tolerance"
    );
}

#[test]
fn node_editing_moves_one_point_and_leaves_the_others() {
    use dpaint_core::kurbo::Shape;
    let mut f = Fixture::new(100.0, 100.0);
    f.must(
        "vector.object.add-path",
        json!({"d": "M 10 10 L 50 10 L 50 50 L 10 50 Z", "fill": "#000000", "name": "q"}),
    );
    f.must(
        "vector.path.node-move",
        json!({"target": "#obj_q", "index": 1, "x": 30, "y": 0, "relative": true}),
    );
    let b = f.path("obj_q").bounding_box();
    assert!(
        (b.x1 - 80.0).abs() < 1e-6,
        "the moved corner extends the box: {b:?}"
    );
    assert!((b.x0 - 10.0).abs() < 1e-6, "the others stayed");

    f.must(
        "vector.path.node-insert",
        json!({"target": "#obj_q", "index": 0, "t": 0.5}),
    );
    let err = f
        .run(
            "vector.path.node-remove",
            json!({"target": "#obj_q", "index": 99}),
        )
        .unwrap_err();
    assert_eq!(err.code(), "invalid");
}

#[test]
fn round_corners_and_reverse_preserve_the_covered_region() {
    let mut f = Fixture::new(120.0, 120.0);
    f.must(
        "vector.object.add-rect",
        json!({"x": 10, "y": 10, "width": 100, "height": 100, "fill": "#000000", "name": "r"}),
    );
    let square = f.covered(1.0);
    f.must(
        "vector.path.round-corners",
        json!({"target": "#obj_r", "radius": 20}),
    );
    let rounded = f.covered(1.0);
    assert!(
        rounded < square,
        "rounded corners cover less: {rounded} < {square}"
    );
    assert!(rounded > square - 500, "but only the corners: {rounded}");
    f.must("vector.path.reverse", json!({"target": "#obj_r"}));
    assert!(
        (f.covered(1.0) as i64 - rounded as i64).abs() < 4,
        "reversing a nonzero fill paints the same pixels"
    );
}

// ------------------------------------------------------------------------ style ops

#[test]
fn gradients_dashes_and_blend_survive_into_the_render() {
    let mut f = Fixture::new(100.0, 100.0);
    f.must(
        "vector.object.add-rect",
        json!({"x": 0, "y": 0, "width": 100, "height": 100, "fill": "#808080", "name": "bg"}),
    );
    f.must(
        "vector.object.add-rect",
        json!({"x": 10, "y": 10, "width": 80, "height": 40, "name": "band"}),
    );
    f.must(
        "vector.style.gradient",
        json!({"target": "#obj_band", "kind": "linear",
               "stops": ["0:#000000", "1:#ffffff"]}),
    );
    let pm = f.render(1.0);
    let (l, r) = (
        pm.pixel(12, 30).unwrap().red(),
        pm.pixel(88, 30).unwrap().red(),
    );
    assert!(
        r > l + 150,
        "the gradient ramps across the shape: {l} -> {r}"
    );

    f.must(
        "vector.style.blend",
        json!({"target": "#obj_band", "mode": "multiply"}),
    );
    assert_eq!(
        f.vec().object(&ObjectId::from("obj_band")).unwrap().blend,
        dpaint_core::doc::common::BlendMode::Multiply
    );
    let multiplied = f.render(1.0).pixel(88, 30).unwrap().red();
    assert!(
        multiplied < 200,
        "white over grey multiplies down to grey: {multiplied}"
    );
}

#[test]
fn copying_style_moves_fill_and_stroke_but_not_geometry() {
    let mut f = Fixture::new(100.0, 100.0);
    f.must(
        "vector.object.add-rect",
        json!({"x": 0, "y": 0, "width": 10, "height": 10, "fill": "#ff0000",
               "stroke": "#00ff00", "stroke_width": 3, "name": "src"}),
    );
    f.must(
        "vector.object.add-ellipse",
        json!({"cx": 50, "cy": 50, "rx": 10, "name": "dst"}),
    );
    let before = area(&f.path("obj_dst"));
    f.must(
        "vector.style.copy",
        json!({"source": "#obj_src", "target": "#obj_dst"}),
    );
    let dst = f.vec().object(&ObjectId::from("obj_dst")).unwrap();
    assert_eq!(dst.fill.average_color().unwrap().to_hex(), "#ff0000");
    assert_eq!(dst.stroke.as_ref().unwrap().width, 3.0);
    assert!(
        (area(&f.path("obj_dst")) - before).abs() < 1e-9,
        "geometry untouched"
    );
}

#[test]
fn a_locked_object_refuses_edits_until_it_is_unlocked() {
    let mut f = Fixture::new(100.0, 100.0);
    f.must(
        "vector.object.add-rect",
        json!({"x": 0, "y": 0, "width": 10, "height": 10, "name": "r"}),
    );
    f.must(
        "vector.style.opacity",
        json!({"target": "#obj_r", "locked": true}),
    );
    let err = f
        .run(
            "vector.style.fill",
            json!({"target": "#obj_r", "color": "#ff0000"}),
        )
        .unwrap_err();
    assert_eq!(err.code(), "invalid");
    f.must(
        "vector.style.opacity",
        json!({"target": "#obj_r", "locked": false}),
    );
    f.must(
        "vector.style.fill",
        json!({"target": "#obj_r", "color": "#ff0000"}),
    );
}

#[test]
fn markers_appear_as_real_geometry_and_are_replaced_not_stacked() {
    let mut f = Fixture::new(100.0, 100.0);
    f.must(
        "vector.object.add-line",
        json!({"x1": 10, "y1": 50, "x2": 90, "y2": 50, "stroke": "#000000",
               "stroke_width": 2, "name": "arrow"}),
    );
    let eff = f.must(
        "vector.style.marker",
        json!({"target": "#obj_arrow", "shape": "arrow", "at": "both"}),
    );
    assert_eq!(eff.created.len(), 2);
    let count = f.vec().walk().len();
    let again = f.must(
        "vector.style.marker",
        json!({"target": "#obj_arrow", "shape": "triangle", "at": "both"}),
    );
    assert_eq!(again.removed.len(), 2, "the previous markers are replaced");
    assert_eq!(f.vec().walk().len(), count, "no stacking");
    f.must(
        "vector.style.marker",
        json!({"target": "#obj_arrow", "shape": "none", "at": "both"}),
    );
    assert_eq!(f.vec().walk().len(), count - 2, "'none' clears them");
}

// ------------------------------------------------------------------------- transforms

#[test]
fn rotating_about_a_pivot_moves_the_shape_and_flatten_bakes_it() {
    use dpaint_core::kurbo::Shape;
    let mut f = Fixture::new(200.0, 200.0);
    f.must(
        "vector.object.add-rect",
        json!({"x": 0, "y": 0, "width": 40, "height": 10, "fill": "#000000", "name": "bar"}),
    );
    f.must(
        "vector.transform.rotate",
        json!({"target": "#obj_bar", "degrees": 90, "around": [0, 0]}),
    );
    let b = f.path("obj_bar").bounding_box();
    assert!(
        (b.x1 - 0.0).abs() < 1e-6 && (b.y1 - 40.0).abs() < 1e-6,
        "rotated to {b:?}"
    );
    f.must("vector.transform.flatten", json!({"target": "#obj_bar"}));
    let o = f.vec().object(&ObjectId::from("obj_bar")).unwrap();
    assert!(o.transform.is_identity(), "the matrix is spent");
    assert!(
        matches!(o.kind, VKind::Path { .. }),
        "geometry is baked into a path"
    );
    let after = f.path("obj_bar").bounding_box();
    assert!(
        (after.y1 - 40.0).abs() < 1e-6,
        "and it did not move: {after:?}"
    );
}

#[test]
fn align_and_distribute_arrange_objects_by_their_bounding_boxes() {
    use dpaint_core::kurbo::Shape;
    let mut f = Fixture::new(300.0, 300.0);
    for (i, x) in [10.0, 90.0, 200.0].iter().enumerate() {
        f.must(
            "vector.object.add-rect",
            json!({"x": x, "y": 10.0 + i as f64 * 30.0, "width": 20, "height": 20,
                   "fill": "#000000", "name": format!("r{i}")}),
        );
    }
    f.must(
        "vector.transform.align",
        json!({"target": "#obj_r0, #obj_r1, #obj_r2", "edge": "top"}),
    );
    for i in 0..3 {
        let b = f.path(&format!("obj_r{i}")).bounding_box();
        assert!((b.y0 - 10.0).abs() < 1e-6, "r{i} top is {}", b.y0);
    }
    f.must(
        "vector.transform.distribute",
        json!({"target": "#obj_r0, #obj_r1, #obj_r2", "axis": "horizontal"}),
    );
    let centers: Vec<f64> = (0..3)
        .map(|i| f.path(&format!("obj_r{i}")).bounding_box().center().x)
        .collect();
    let g1 = centers[1] - centers[0];
    let g2 = centers[2] - centers[1];
    assert!((g1 - g2).abs() < 1e-6, "even spacing: {g1} vs {g2}");
}

// ------------------------------------------------------------------------------- text

#[test]
fn text_to_outlines_renders_identically_to_the_shaped_text() {
    let mut f = Fixture::new(220.0, 80.0);
    f.must(
        "vector.object.add-text",
        json!({"text": "Handgloves", "x": 10, "y": 10, "size": 36, "fill": "#000000", "name": "t"}),
    );
    let before = f.render(2.0);
    let eff = f.must("vector.text.to-outlines", json!({"target": "#obj_t"}));
    assert_eq!(eff.removed, vec!["obj_t".to_string()]);
    assert_eq!(eff.created.len(), 1, "one line becomes one path");
    let after = f.render(2.0);
    assert_eq!(
        (before.width(), before.height()),
        (after.width(), after.height())
    );
    // Path data is stored at three decimals, so a handful of edge pixels can shift by a
    // fraction of a coverage step. Anything more would mean the outlines differ.
    let deltas: Vec<i32> = before
        .data()
        .iter()
        .zip(after.data().iter())
        .map(|(a, b)| (*a as i32 - *b as i32).abs())
        .filter(|d| *d > 0)
        .collect();
    let total = before.data().len();
    assert!(
        deltas.len() * 1000 < total,
        "{} of {total} bytes differ; the outlines are not the same shape",
        deltas.len()
    );
    assert!(
        deltas.iter().copied().max().unwrap_or(0) <= 16,
        "largest channel difference {:?} is a visible change",
        deltas.iter().max()
    );
    let created = ObjectId::from(eff.created[0].clone());
    assert!(matches!(
        f.vec().object(&created).unwrap().kind,
        VKind::Path { .. }
    ));
}

#[test]
fn a_multiline_text_outlines_into_a_group_of_one_path_per_line() {
    let mut f = Fixture::new(200.0, 200.0);
    f.must(
        "vector.object.add-text",
        json!({"text": "one\ntwo\nthree", "x": 10, "y": 10, "size": 20, "fill": "#000000", "name": "t"}),
    );
    let eff = f.must("vector.text.to-outlines", json!({"target": "#obj_t"}));
    assert_eq!(eff.created.len(), 4, "a group plus three line paths");
    let group = f
        .vec()
        .objects
        .iter()
        .find(|o| matches!(o.kind, VKind::Group { .. }));
    let VKind::Group { objects } = &group.expect("a group was created").kind else {
        unreachable!()
    };
    assert_eq!(objects.len(), 3);
}

#[test]
fn text_on_a_path_starts_at_the_path_start_and_advances_by_arc_length() {
    use dpaint_core::kurbo::{BezPath, Shape};
    let fonts = dpaint_vector::text::fonts();
    let mut spec = dpaint_core::doc::common::TextSpec::new("MINIMUM");
    spec.size = 24.0;

    let mut line = BezPath::new();
    line.move_to((0.0, 100.0));
    line.line_to((400.0, 100.0));
    let flat = dpaint_vector::geom::flatten(&line, 0.01);
    let (outline, sub, span) =
        dpaint_vector::text::outline_on_path(fonts, &spec, &flat, 0.0, false);
    assert!(sub.is_none());
    let span = span.expect("a run was placed");
    assert!(
        span.start.x.abs() < 1e-6 && (span.start.y - 100.0).abs() < 1e-6,
        "the first glyph sits at the path start: {:?}",
        span.start
    );
    assert!(
        span.end.x < 400.0 && span.end.x > 0.0,
        "the last glyph ends before the path does: {}",
        span.end.x
    );
    assert!(
        (span.end.x - span.advance).abs() < 1e-6,
        "on a straight path the run end equals the shaped advance: {} vs {}",
        span.end.x,
        span.advance
    );

    // On a straight baseline, text-on-path must place glyphs exactly where plain layout
    // would: that is what "advance follows arc length" means.
    let (plain, _) = dpaint_vector::text::outline_block(fonts, &spec, (0.0, 0.0));
    let pb = plain.bounding_box();
    let ob = outline.bounding_box();
    assert!((ob.width() - pb.width()).abs() < 1e-6, "same run width");
    assert!((ob.x0 - pb.x0).abs() < 1e-6, "same left edge");

    // Halfway along, the offset moves the run by exactly that arc length.
    let (_, _, moved) = dpaint_vector::text::outline_on_path(fonts, &spec, &flat, 50.0, false);
    assert!((moved.unwrap().start.x - 50.0).abs() < 1e-6);
}

#[test]
fn text_on_a_curve_follows_the_tangent() {
    let mut f = Fixture::new(300.0, 300.0);
    f.must(
        "vector.object.add-ellipse",
        json!({"cx": 150, "cy": 150, "rx": 100, "name": "ring"}),
    );
    f.must(
        "vector.object.add-text",
        json!({"text": "AROUND", "x": 0, "y": 0, "size": 20, "fill": "#000000", "name": "t"}),
    );
    f.must(
        "vector.text.on-path",
        json!({"target": "#obj_t", "path": "#obj_ring"}),
    );
    let curved = f.path("obj_t");
    assert!(!curved.elements().is_empty(), "the glyphs resolved");
    use dpaint_core::kurbo::Shape;
    let b = curved.bounding_box();
    // The run starts at the ellipse's first point (its right-hand extreme) and curls down
    // around the rim, so a bound taller than one line of 20 px text proves it followed the
    // tangent instead of staying on a flat baseline.
    assert!(
        b.height() > 40.0,
        "the run bends around the ellipse instead of staying flat: {b:?}"
    );
    assert!(
        b.x1 <= 270.0 && b.y1 <= 260.0,
        "and it stays on the rim: {b:?}"
    );
    f.must(
        "vector.text.on-path",
        json!({"target": "#obj_t", "release": true}),
    );
    let flat = f.path("obj_t").bounding_box();
    assert!(flat.height() < b.height(), "released text lies flat again");
}

#[test]
fn an_unavailable_font_family_reports_a_font_fallback_warning() {
    let mut f = Fixture::new(100.0, 50.0);
    let eff = f.must(
        "vector.object.add-text",
        json!({"text": "hi", "family": "Nonexistent Grotesk", "name": "t"}),
    );
    assert_eq!(eff.warnings.len(), 1);
    assert_eq!(eff.warnings[0].code, "font-fallback");
}

#[test]
fn flowing_text_into_a_shape_keeps_it_inside_and_reports_overflow() {
    use dpaint_core::kurbo::Shape;
    let mut f = Fixture::new(200.0, 200.0);
    f.must(
        "vector.object.add-ellipse",
        json!({"cx": 100, "cy": 100, "rx": 80, "ry": 60, "name": "blob"}),
    );
    f.must(
        "vector.object.add-text",
        json!({"text": "alpha beta gamma delta epsilon zeta eta theta",
               "size": 14, "fill": "#000000", "name": "t"}),
    );
    let eff = f.must(
        "vector.text.flow-in-shape",
        json!({"target": "#obj_t", "shape": "#obj_blob"}),
    );
    let id = ObjectId::from(eff.created[0].clone());
    let b = dpaint_vector::path_of(&f.project, &f.doc, &id)
        .unwrap()
        .bounding_box();
    let shape = f.path("obj_blob").bounding_box();
    assert!(
        b.x0 >= shape.x0 - 1.0 && b.x1 <= shape.x1 + 1.0 && b.y1 <= shape.y1 + 1.0,
        "the flowed text stays inside {shape:?}, got {b:?}"
    );
}

// ------------------------------------------------------------------- clipping, groups

#[test]
fn a_clip_limits_the_render_and_releasing_it_restores_the_object() {
    let mut f = Fixture::new(100.0, 100.0);
    f.must(
        "vector.object.add-rect",
        json!({"x": 10, "y": 10, "width": 20, "height": 80, "name": "window"}),
    );
    f.must(
        "vector.object.add-rect",
        json!({"x": 0, "y": 0, "width": 100, "height": 100, "fill": "#000000", "name": "sheet"}),
    );
    let full = f.covered(1.0);
    f.must(
        "vector.clip.set",
        json!({"target": "#obj_sheet", "source": "#obj_window"}),
    );
    let clipped = f.covered(1.0);
    assert!(
        (clipped as i64 - 1600).abs() < 80,
        "clipped to the window: {clipped}"
    );
    f.must("vector.clip.release", json!({"target": "#obj_sheet"}));
    assert_eq!(f.covered(1.0), full);
}

#[test]
fn a_luminance_mask_fades_what_it_covers() {
    let mut f = Fixture::new(60.0, 60.0);
    f.must(
        "vector.object.add-rect",
        json!({"x": 0, "y": 0, "width": 60, "height": 60, "fill": "#808080", "name": "gate"}),
    );
    // The mask source is not painted itself, only sampled.
    f.must(
        "vector.style.opacity",
        json!({"target": "#obj_gate", "visible": false}),
    );
    f.must(
        "vector.object.add-rect",
        json!({"x": 0, "y": 0, "width": 60, "height": 60, "fill": "#ff0000", "name": "sheet"}),
    );
    let opaque = f.render(1.0).pixel(30, 30).unwrap().alpha();
    f.must(
        "vector.mask.set",
        json!({"target": "#obj_sheet", "source": "#obj_gate"}),
    );
    let masked = f.render(1.0).pixel(30, 30).unwrap().alpha();
    assert_eq!(opaque, 255);
    assert!(
        masked > 0 && masked < 200,
        "mid grey mask thins the fill: {masked}"
    );
    f.must("vector.mask.release", json!({"target": "#obj_sheet"}));
    assert_eq!(f.render(1.0).pixel(30, 30).unwrap().alpha(), 255);
}

#[test]
fn grouping_then_ungrouping_leaves_the_render_unchanged() {
    let mut f = Fixture::new(120.0, 120.0);
    f.must(
        "vector.object.add-rect",
        json!({"x": 10, "y": 10, "width": 30, "height": 30, "fill": "#ff0000", "name": "a"}),
    );
    f.must(
        "vector.object.add-rect",
        json!({"x": 60, "y": 60, "width": 30, "height": 30, "fill": "#0000ff", "name": "b"}),
    );
    let before = f.render(1.0);
    f.must(
        "vector.object.group",
        json!({"target": "#obj_a, #obj_b", "name": "pair"}),
    );
    f.must(
        "vector.transform.translate",
        json!({"target": "#obj_pair", "dx": 10, "dy": 0}),
    );
    let moved = f.path("obj_a");
    use dpaint_core::kurbo::Shape;
    assert!(
        (moved.bounding_box().x0 - 20.0).abs() < 1e-9,
        "the group carried its child"
    );
    f.must(
        "vector.transform.translate",
        json!({"target": "#obj_pair", "dx": -10, "dy": 0}),
    );
    f.must("vector.object.ungroup", json!({"target": "#obj_pair"}));
    assert_eq!(f.vec().objects.len(), 2, "children came back to the root");
    assert_eq!(before.data(), f.render(1.0).data(), "pixels unchanged");
}

#[test]
fn reordering_changes_which_object_wins_the_overlap() {
    let mut f = Fixture::new(60.0, 60.0);
    f.must(
        "vector.object.add-rect",
        json!({"x": 0, "y": 0, "width": 60, "height": 60, "fill": "#ff0000", "name": "red"}),
    );
    f.must(
        "vector.object.add-rect",
        json!({"x": 0, "y": 0, "width": 60, "height": 60, "fill": "#0000ff", "name": "blue"}),
    );
    assert!(f.render(1.0).pixel(30, 30).unwrap().blue() > 200);
    f.must(
        "vector.object.reorder",
        json!({"target": "#obj_red", "to": "front"}),
    );
    assert!(f.render(1.0).pixel(30, 30).unwrap().red() > 200);
}

// ------------------------------------------------------------------------ artboards

#[test]
fn artboards_control_the_rendered_canvas() {
    let mut f = Fixture::new(100.0, 100.0);
    assert_eq!((f.render(1.0).width(), f.render(1.0).height()), (100, 100));
    f.must(
        "vector.artboard.resize",
        json!({"width": 50, "height": 200}),
    );
    let pm = f.render(1.0);
    assert_eq!((pm.width(), pm.height()), (50, 200));

    f.must(
        "vector.object.add-rect",
        json!({"x": 20, "y": 20, "width": 10, "height": 10, "fill": "#000000", "name": "r"}),
    );
    f.must("vector.artboard.fit-content", json!({"padding": 5}));
    let pm = f.render(1.0);
    assert_eq!(
        (pm.width(), pm.height()),
        (20, 20),
        "fitted to content plus padding"
    );

    let err = f
        .run("vector.artboard.remove", json!({"artboard": "artboard"}))
        .unwrap_err();
    assert_eq!(err.code(), "invalid", "the last artboard cannot be removed");
}

#[test]
fn rendering_at_scale_four_quadruples_the_dimensions_and_the_geometry() {
    let mut f = Fixture::new(80.0, 60.0);
    f.must(
        "vector.object.add-ellipse",
        json!({"cx": 40, "cy": 30, "rx": 20, "fill": "#000000", "name": "dot"}),
    );
    let one = f.render(1.0);
    let four = f.render(4.0);
    assert_eq!(
        (one.width() * 4, one.height() * 4),
        (four.width(), four.height())
    );
    let ratio = f.covered(4.0) as f64 / f.covered(1.0) as f64;
    assert!(
        (ratio - 16.0).abs() < 0.2,
        "coverage scales with the area: {ratio}"
    );
}

// ---------------------------------------------------------------------------- measure

#[test]
fn measure_ops_return_data_and_change_nothing() {
    let mut f = Fixture::new(200.0, 200.0);
    f.must(
        "vector.object.add-rect",
        json!({"x": 10, "y": 20, "width": 40, "height": 30, "fill": "#000000", "name": "r"}),
    );
    f.must(
        "vector.object.add-line",
        json!({"x1": 0, "y1": 100, "x2": 100, "y2": 100, "stroke": "#000000", "name": "l"}),
    );
    let snapshot = serde_json::to_value(f.vec()).unwrap();

    let d = f
        .must("vector.measure.bbox", json!({"target": "#obj_r"}))
        .data
        .unwrap();
    assert_eq!(d["bbox"]["x"], 10.0);
    assert_eq!(d["bbox"]["width"], 40.0);

    let d = f
        .must("vector.measure.area", json!({"target": "#obj_r"}))
        .data
        .unwrap();
    assert!((d["total"].as_f64().unwrap() - 1200.0).abs() < 1e-6);

    let d = f
        .must("vector.measure.length", json!({"target": "#obj_l"}))
        .data
        .unwrap();
    assert!((d["total"].as_f64().unwrap() - 100.0).abs() < 1e-6);

    let d = f
        .must(
            "vector.measure.sample",
            json!({"target": "#obj_l", "t": 0.25}),
        )
        .data
        .unwrap();
    assert!((d["point"][0].as_f64().unwrap() - 25.0).abs() < 1e-6);

    let d = f
        .must(
            "vector.measure.tangent",
            json!({"target": "#obj_l", "t": 0.5}),
        )
        .data
        .unwrap();
    assert!((d["tangent"][0].as_f64().unwrap() - 1.0).abs() < 1e-6);
    assert!((d["normal"][1].as_f64().unwrap() - 1.0).abs() < 1e-6);

    let d = f
        .must(
            "vector.measure.intersections",
            json!({"target": "#obj_r", "with": "#obj_l"}),
        )
        .data
        .unwrap();
    assert_eq!(d["count"], 0, "the line passes below the rectangle");

    assert_eq!(
        serde_json::to_value(f.vec()).unwrap(),
        snapshot,
        "queries never mutate"
    );
}

// ------------------------------------------------------------------------------ trace

#[test]
fn tracing_a_bitmap_produces_editable_paths_in_the_document() {
    let mut f = Fixture::new(64.0, 64.0);
    let mut pm = tiny_skia::Pixmap::new(64, 64).unwrap();
    pm.fill(tiny_skia::Color::WHITE);
    let mut blob = tiny_skia::Pixmap::new(30, 30).unwrap();
    blob.fill(tiny_skia::Color::BLACK);
    pm.draw_pixmap(
        16,
        16,
        blob.as_ref(),
        &tiny_skia::PixmapPaint::default(),
        tiny_skia::Transform::identity(),
        None,
    );
    let file = f._tmp.path().join("blob.png");
    std::fs::write(&file, pm.encode_png().unwrap()).unwrap();

    let eff = f.must(
        "vector.trace.image",
        json!({"path": file.to_str().unwrap(), "mode": "binary", "name": "logo"}),
    );
    assert!(eff.created.len() >= 2, "a group and at least one path");
    let traced = f.vec().walk();
    let path = traced
        .iter()
        .find(|o| matches!(o.kind, VKind::Path { .. }))
        .expect("a traced path exists");
    let a = area(&dpaint_vector::path_of(&f.project, &f.doc, &path.id).unwrap());
    assert!((a - 900.0).abs() < 80.0, "traced area {a} ~ 30x30");
}

// -------------------------------------------------------------------------------- svg

#[test]
fn a_built_document_survives_an_svg_round_trip_unchanged() {
    let mut f = Fixture::new(200.0, 150.0);
    f.must(
        "vector.object.add-rect",
        json!({"x": 10, "y": 10, "width": 80, "height": 60, "name": "panel"}),
    );
    f.must(
        "vector.style.gradient",
        json!({"target": "#obj_panel", "kind": "linear", "stops": ["0:#ff0000", "1:#0000ff"]}),
    );
    f.must(
        "vector.object.add-line",
        json!({"x1": 10, "y1": 120, "x2": 190, "y2": 120, "stroke": "#008800",
               "stroke_width": 4, "name": "rule"}),
    );
    f.must(
        "vector.style.dash",
        json!({"target": "#obj_rule", "pattern": [8, 4], "offset": 2}),
    );
    f.must(
        "vector.object.add-ellipse",
        json!({"cx": 150, "cy": 50, "rx": 30, "name": "window"}),
    );
    f.must(
        "vector.object.group",
        json!({"target": "#obj_panel, #obj_rule", "name": "art"}),
    );
    f.must(
        "vector.clip.set",
        json!({"target": "#obj_art", "source": "#obj_window"}),
    );

    let first = dpaint_vector::to_svg(&f.project, &f.doc).unwrap();
    let back = dpaint_vector::import_svg(&first, DocId::from("doc_v"), "v").unwrap();
    let mut p2 = Project::new("t2", Document::Vector(back));
    let id2 = p2.active.clone();
    let second = dpaint_vector::to_svg(&p2, &id2).unwrap();
    assert_eq!(first, second, "round trip is not stable");
    let _ = p2.vector_mut(&id2).unwrap();
}

// ------------------------------------------------------------- images and doc fills

#[test]
fn an_embedded_image_renders_from_the_asset_store_at_its_placed_rectangle() {
    let mut f = Fixture::new(100.0, 100.0);
    let mut src = tiny_skia::Pixmap::new(4, 4).unwrap();
    src.fill(tiny_skia::Color::from_rgba8(0, 128, 255, 255));
    let file = f._tmp.path().join("chip.png");
    std::fs::write(&file, src.encode_png().unwrap()).unwrap();

    f.must(
        "vector.object.add-image",
        json!({"path": file.to_str().unwrap(), "x": 20, "y": 20,
               "width": 40, "height": 40, "name": "chip"}),
    );
    let pm = f.render(1.0);
    let inside = pm.pixel(40, 40).unwrap();
    assert!(
        inside.blue() > 200 && inside.red() < 40,
        "the image paints its own pixels: {inside:?}"
    );
    assert_eq!(
        pm.pixel(5, 5).unwrap().alpha(),
        0,
        "and only inside its rectangle"
    );
    assert!((f.covered(1.0) as i64 - 1600).abs() < 40);
}

#[test]
fn a_document_fill_paints_another_vector_document_into_the_shape() {
    use dpaint_core::doc::common::Paint;
    let mut f = Fixture::new(100.0, 100.0);
    // A second vector document, solid green, used as a fill.
    let mut swatch = VectorDoc::new(DocId::from("doc_swatch"), "swatch", 10.0, 10.0);
    swatch.artboards[0].background = Some(dpaint_core::Color::parse("#00ff00").unwrap());
    f.project.add_document(Document::Vector(swatch));

    f.must(
        "vector.object.add-rect",
        json!({"x": 10, "y": 10, "width": 50, "height": 50, "name": "panel"}),
    );
    f.project
        .vector_mut(&f.doc)
        .unwrap()
        .object_mut(&ObjectId::from("obj_panel"))
        .unwrap()
        .fill = Paint::Document {
        document: DocId::from("doc_swatch"),
    };
    let px = f.render(1.0).pixel(30, 30).unwrap();
    assert!(
        px.green() > 200 && px.red() < 40,
        "the linked document paints: {px:?}"
    );
    assert_eq!(f.render(1.0).pixel(5, 5).unwrap().alpha(), 0);
}
