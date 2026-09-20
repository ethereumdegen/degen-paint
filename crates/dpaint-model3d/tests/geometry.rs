//! Geometry behavior: primitives, extrusion, revolve, loft, UVs, tangents, decimation.

mod common;

use common::*;
use dpaint_core::doc::common::FillRule;
use dpaint_core::doc::model::{Bevel, Caps, Mesh, MeshSource, Primitive};
use dpaint_core::{MeshId, Project};
use dpaint_model3d::build::build_mesh;
use dpaint_model3d::geom::{dot, length, MeshData};

fn primitive_mesh(shape: Primitive, size: [f32; 3], segments: u32) -> MeshData {
    let (_tmp, assets) = store();
    let (mut project, doc) = model_project();
    project.model_mut(&doc).unwrap().meshes.push(Mesh {
        id: MeshId::from("msh_p"),
        name: "p".into(),
        source: MeshSource::Primitive {
            shape,
            size,
            segments,
        },
    });
    build_mesh(&project, &doc, &MeshId::from("msh_p"), &assets).unwrap()
}

#[test]
fn box_primitive_has_24_vertices_12_triangles_unit_normals_and_uvs_in_range() {
    let m = primitive_mesh(Primitive::Box, [2.0, 3.0, 4.0], 32);
    assert_eq!(m.vertex_count(), 24, "one vertex per face corner");
    assert_eq!(m.triangle_count(), 12);
    for n in &m.normals {
        assert!(
            (length(*n) - 1.0).abs() < 1e-5,
            "normal {n:?} is not unit length"
        );
    }
    for uv in &m.uvs {
        assert!(
            (0.0..=1.0).contains(&uv[0]) && (0.0..=1.0).contains(&uv[1]),
            "uv {uv:?} escapes [0,1]"
        );
    }
    let (lo, hi) = m.bounds().unwrap();
    assert!((hi[0] - lo[0] - 2.0).abs() < 1e-5);
    assert!((hi[1] - lo[1] - 3.0).abs() < 1e-5);
    assert!((hi[2] - lo[2] - 4.0).abs() < 1e-5);
    // Closed, outward-wound: the divergence theorem returns the box volume, positive.
    assert!(
        (m.volume() - 24.0).abs() < 1e-3,
        "volume was {}",
        m.volume()
    );
}

#[test]
fn every_primitive_has_unit_normals_uvs_in_range_and_outward_winding() {
    for shape in [
        Primitive::Box,
        Primitive::Sphere,
        Primitive::Cylinder,
        Primitive::Cone,
        Primitive::Torus,
        Primitive::Plane,
        Primitive::Capsule,
    ] {
        let m = primitive_mesh(shape, [2.0, 2.0, 2.0], 24);
        assert!(m.triangle_count() > 0, "{shape:?} produced no triangles");
        assert_eq!(m.normals.len(), m.positions.len(), "{shape:?} normals");
        assert_eq!(m.uvs.len(), m.positions.len(), "{shape:?} uvs");
        for n in &m.normals {
            assert!(
                (length(*n) - 1.0).abs() < 1e-4,
                "{shape:?} normal {n:?} is not unit length"
            );
        }
        for uv in &m.uvs {
            assert!(
                (-1e-6..=1.000001).contains(&uv[0]) && (-1e-6..=1.000001).contains(&uv[1]),
                "{shape:?} uv {uv:?} escapes [0,1]"
            );
        }
        assert!(
            dpaint_model3d::prim::winding_matches_normals(&m),
            "{shape:?} has triangles wound against their normals"
        );
    }
}

#[test]
fn sphere_volume_and_radius_match_its_size() {
    let m = primitive_mesh(Primitive::Sphere, [2.0, 2.0, 2.0], 64);
    let exact = 4.0 / 3.0 * std::f64::consts::PI;
    let v = m.volume();
    assert!(
        (v - exact).abs() / exact < 0.01,
        "volume {v} is not within 1% of {exact}"
    );
    for p in &m.positions {
        assert!(
            (length(*p) - 1.0).abs() < 1e-4,
            "point {p:?} is off the unit sphere"
        );
    }
}

#[test]
fn cylinder_is_a_closed_solid_of_the_expected_volume() {
    let m = primitive_mesh(Primitive::Cylinder, [2.0, 5.0, 2.0], 96);
    let exact = std::f64::consts::PI * 1.0 * 5.0;
    assert!(
        (m.volume() - exact).abs() / exact < 0.01,
        "volume {} vs {exact}",
        m.volume()
    );
}

/// Extruding a square of side `s` by depth `d`: a closed solid whose bounds and volume
/// follow from `s` and `d`, with 12 triangles (2 per cap, 2 per side wall).
#[test]
fn extruding_a_square_yields_a_closed_solid_of_the_right_size() {
    let (_tmp, assets) = store();
    let (s, d) = (4.0f32, 3.0f32);
    let (project, doc, mesh) = extruded(
        &assets,
        &square_path(s as f64),
        FillRule::Nonzero,
        d,
        None,
        Caps::Both,
    );
    let m = build_mesh(&project, &doc, &mesh, &assets).unwrap();

    let (lo, hi) = m.bounds().unwrap();
    assert!(
        (hi[0] - lo[0] - s).abs() < 1e-4,
        "x extent {}",
        hi[0] - lo[0]
    );
    assert!(
        (hi[1] - lo[1] - s).abs() < 1e-4,
        "y extent {}",
        hi[1] - lo[1]
    );
    assert!(
        (hi[2] - lo[2] - d).abs() < 1e-4,
        "z extent {}",
        hi[2] - lo[2]
    );
    assert_eq!(m.triangle_count(), 12, "4 cap + 8 wall triangles");

    let volume = m.volume();
    let expected = (s * s * d) as f64;
    assert!(
        (volume - expected).abs() / expected < 1e-3,
        "volume {volume} should be s*s*d = {expected}"
    );
    assert_eq!(
        dpaint_model3d::validate::topology(&m).boundary_edges,
        0,
        "a both-caps extrusion must be closed"
    );
    let area = m.surface_area();
    let expected_area = (2.0 * s * s + 4.0 * s * d) as f64;
    assert!(
        (area - expected_area).abs() / expected_area < 1e-3,
        "surface area {area} vs {expected_area}"
    );
}

/// The marquee case: a ring extrudes with its hole intact. If the caps ignored the inner
/// contour the volume would be `outer^2 * depth` instead of `(outer^2 - inner^2) * depth`.
#[test]
fn extruding_a_ring_keeps_the_hole_in_the_caps() {
    let (_tmp, assets) = store();
    let (outer, inner, depth) = (10.0f64, 4.0f64, 2.0f32);
    let (project, doc, mesh) = extruded(
        &assets,
        &ring_path(outer, inner),
        FillRule::Evenodd,
        depth,
        None,
        Caps::Both,
    );
    let m = build_mesh(&project, &doc, &mesh, &assets).unwrap();

    let cap_area = outer * outer - inner * inner;
    let volume = m.volume();
    let expected = cap_area * depth as f64;
    assert!(
        (volume - expected).abs() / expected < 1e-3,
        "volume {volume} should be (outer^2 - inner^2) * depth = {expected}; \
         a filled cap would give {}",
        outer * outer * depth as f64
    );

    // Both cap faces plus both walls. The inner wall exists only if the hole was extruded.
    let expected_area = 2.0 * cap_area + 4.0 * outer * depth as f64 + 4.0 * inner * depth as f64;
    let area = m.surface_area();
    assert!(
        (area - expected_area).abs() / expected_area < 1e-3,
        "surface area {area} vs {expected_area}"
    );
    assert_eq!(
        dpaint_model3d::validate::topology(&m).boundary_edges,
        0,
        "the ring solid must be closed"
    );
}

#[test]
fn a_bevel_adds_surface_area_and_only_grows_the_depth_axis() {
    let (_tmp, assets) = store();
    let (s, d, b) = (6.0f32, 4.0f32, 0.5f32);
    let (project, doc, plain) = extruded(
        &assets,
        &square_path(s as f64),
        FillRule::Nonzero,
        d,
        None,
        Caps::Both,
    );
    let flat = build_mesh(&project, &doc, &plain, &assets).unwrap();

    let (project, doc, beveled_id) = extruded(
        &assets,
        &square_path(s as f64),
        FillRule::Nonzero,
        d,
        Some(Bevel {
            size: b,
            segments: 4,
        }),
        Caps::Both,
    );
    let beveled = build_mesh(&project, &doc, &beveled_id, &assets).unwrap();

    assert!(
        beveled.surface_area() > flat.surface_area(),
        "bevelled area {} should exceed flat area {}",
        beveled.surface_area(),
        flat.surface_area()
    );
    let (lo, hi) = beveled.bounds().unwrap();
    let z = hi[2] - lo[2];
    assert!(
        z <= d + 2.0 * b + 1e-4,
        "depth axis grew to {z}, past depth + 2*bevel = {}",
        d + 2.0 * b
    );
    assert!(z > d, "a bevel must extend past the straight wall, got {z}");
    assert!(
        hi[0] - lo[0] <= s + 1e-4 && hi[1] - lo[1] <= s + 1e-4,
        "a bevel must never grow the cross section"
    );
    // The rounded lip stays inside the bounding box it just grew: not a plain box.
    let box_volume = (s * s * (d + 2.0 * b)) as f64;
    assert!(
        beveled.volume() < box_volume * 0.995,
        "volume {} should stay under the {box_volume} bounding box",
        beveled.volume()
    );
    assert!(
        beveled.volume() > flat.volume(),
        "the lip adds material beyond the straight wall"
    );
}

#[test]
fn caps_none_leaves_an_open_tube_and_caps_front_leaves_one_opening() {
    let (_tmp, assets) = store();
    for (caps, closed) in [
        (Caps::None, false),
        (Caps::Front, false),
        (Caps::Both, true),
    ] {
        let (project, doc, mesh) = extruded(
            &assets,
            &square_path(2.0),
            FillRule::Nonzero,
            1.0,
            None,
            caps,
        );
        let m = build_mesh(&project, &doc, &mesh, &assets).unwrap();
        let topo = dpaint_model3d::validate::topology(&m);
        assert_eq!(
            topo.boundary_edges == 0,
            closed,
            "{caps:?} should {} be closed, boundary edges {}",
            if closed { "" } else { "not" },
            topo.boundary_edges
        );
        assert_eq!(topo.non_manifold_edges, 0, "{caps:?} must stay manifold");
    }
}

#[test]
fn revolving_a_straight_segment_approximates_a_cylinder() {
    let (_tmp, assets) = store();
    let (radius, height) = (2.0f64, 3.0f64);
    let (project, doc) = model_project_with_vector(&format!("M {radius} 0 L {radius} -{height}"));
    let mut project = project;
    project.model_mut(&doc).unwrap().meshes.push(Mesh {
        id: MeshId::from("msh_r"),
        name: "lathe".into(),
        source: MeshSource::Revolve {
            from: path_ref(),
            angle: 360.0,
            segments: 128,
            flatten: 0.25,
        },
    });
    let m = build_mesh(&project, &doc, &MeshId::from("msh_r"), &assets).unwrap();

    let (lo, hi) = m.bounds().unwrap();
    assert!((hi[0] - radius as f32).abs() < 0.01 && (lo[0] + radius as f32).abs() < 0.01);
    assert!((hi[2] - radius as f32).abs() < 0.01 && (lo[2] + radius as f32).abs() < 0.01);
    assert!(
        (lo[1]).abs() < 1e-4 && (hi[1] - height as f32).abs() < 1e-4,
        "height spans {} .. {}",
        lo[1],
        hi[1]
    );
    let lateral = 2.0 * std::f64::consts::PI * radius * height;
    let area = m.surface_area();
    assert!(
        (area - lateral).abs() / lateral < 0.01,
        "lateral area {area} vs 2*pi*r*h = {lateral}"
    );
    // Outward normals: every normal points away from the Y axis.
    for (p, n) in m.positions.iter().zip(m.normals.iter()) {
        let radial = [p[0], 0.0, p[2]];
        if length(radial) > 1e-3 {
            assert!(dot(radial, *n) > 0.0, "normal {n:?} at {p:?} points inward");
        }
    }
}

#[test]
fn lofting_two_equal_squares_builds_a_closed_prism() {
    let (_tmp, assets) = store();
    let s = 3.0f64;
    let (mut project, doc) = model_project();
    let vdoc = add_vector_doc(
        &mut project,
        &[("obj_a", &square_path(s)), ("obj_b", &square_path(s))],
        FillRule::Nonzero,
    );
    project.model_mut(&doc).unwrap().meshes.push(Mesh {
        id: MeshId::from("msh_l"),
        name: "loft".into(),
        source: MeshSource::Loft {
            sections: vec![
                dpaint_core::doc::model::PathRef {
                    document: vdoc.clone(),
                    object: "obj_a".into(),
                },
                dpaint_core::doc::model::PathRef {
                    document: vdoc,
                    object: "obj_b".into(),
                },
            ],
            flatten: 0.25,
        },
    });
    let m = build_mesh(&project, &doc, &MeshId::from("msh_l"), &assets).unwrap();
    let expected = s * s * 1.0; // sections sit one unit apart
    assert!(
        (m.volume().abs() - expected).abs() / expected < 0.02,
        "loft volume {} vs {expected}",
        m.volume().abs()
    );
    assert_eq!(
        dpaint_model3d::validate::topology(&m).boundary_edges,
        0,
        "a capped loft is closed"
    );
}

#[test]
fn decimation_drops_triangles_while_keeping_the_silhouette() {
    let mut m = primitive_mesh(Primitive::Sphere, [2.0, 2.0, 2.0], 48);
    let before = m.triangle_count();
    let (lo0, hi0) = m.bounds().unwrap();
    m.decimate(0.25, 40.0);
    let after = m.triangle_count();
    assert!(
        after < before && after > 0,
        "decimation went from {before} to {after}"
    );
    assert!(
        (after as f32) < before as f32 * 0.6,
        "expected a real reduction, {before} -> {after}"
    );
    let (lo1, hi1) = m.bounds().unwrap();
    for i in 0..3 {
        assert!((lo1[i] - lo0[i]).abs() < 0.2 && (hi1[i] - hi0[i]).abs() < 0.2);
    }
    assert_eq!(m.normals.len(), m.positions.len());
}

#[test]
fn welding_a_split_mesh_rejoins_it_and_leaves_the_shape_alone() {
    // Two triangles sharing an edge, authored with duplicated vertices.
    let mut m = MeshData {
        positions: vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 0.0],
            [0.0, 1.0, 0.0],
        ],
        normals: vec![[0.0, 0.0, 1.0]; 6],
        uvs: vec![[0.0, 0.0]; 6],
        indices: vec![0, 1, 2, 3, 4, 5],
    };
    let area_before = m.surface_area();
    let removed = m.weld(1e-4);
    assert_eq!(removed, 2, "the two duplicated corners collapse");
    assert_eq!(m.vertex_count(), 4);
    assert_eq!(m.triangle_count(), 2);
    assert!((m.surface_area() - area_before).abs() < 1e-6);
}

#[test]
fn planar_and_box_uv_projection_stay_inside_the_unit_square() {
    let mut m = primitive_mesh(Primitive::Box, [2.0, 1.0, 3.0], 1);
    dpaint_model3d::uv::planar(&mut m, dpaint_model3d::uv::Axis::Y);
    assert!(m
        .uvs
        .iter()
        .all(|uv| (0.0..=1.0).contains(&uv[0]) && (0.0..=1.0).contains(&uv[1])));
    let mut m = primitive_mesh(Primitive::Box, [2.0, 1.0, 3.0], 1);
    dpaint_model3d::uv::box_project(&mut m);
    assert_eq!(m.uvs.len(), m.positions.len());
    assert!(m
        .uvs
        .iter()
        .all(|uv| (0.0..=1.0).contains(&uv[0]) && (0.0..=1.0).contains(&uv[1])));
    // Box projection gives each face the full square, so opposite faces do not share UVs
    // with the stretched single-axis projection.
    let spread = m.uvs.iter().fold((1.0f32, 0.0f32), |(lo, hi), uv| {
        (lo.min(uv[0]), hi.max(uv[0]))
    });
    assert!(
        spread.1 - spread.0 > 0.9,
        "box projection should span the square"
    );
}

#[test]
fn unwrap_packs_charts_without_leaving_the_unit_square() {
    let mut m = primitive_mesh(Primitive::Box, [2.0, 2.0, 2.0], 1);
    dpaint_model3d::uv::unwrap(&mut m, 60.0);
    assert_eq!(m.uvs.len(), m.positions.len());
    assert!(m
        .uvs
        .iter()
        .all(|uv| (0.0..=1.0).contains(&uv[0]) && (0.0..=1.0).contains(&uv[1])));
    // Six faces at 90 degrees to each other become six charts, so the packed UV area is at
    // most the unit square and each face keeps a non-degenerate footprint.
    let mut total = 0.0f32;
    for t in m.indices.chunks_exact(3) {
        let (a, b, c) = (
            m.uvs[t[0] as usize],
            m.uvs[t[1] as usize],
            m.uvs[t[2] as usize],
        );
        total += ((b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])).abs() * 0.5;
    }
    assert!(total > 0.05, "charts collapsed: packed area {total}");
    assert!(
        total <= 1.0,
        "charts overlap the atlas: packed area {total}"
    );
}

#[test]
fn mikktspace_tangents_are_unit_length_and_perpendicular_to_their_normals() {
    let m = primitive_mesh(Primitive::Sphere, [2.0, 2.0, 2.0], 32);
    let tangents = dpaint_model3d::uv::tangents(&m);
    assert_eq!(tangents.len(), m.positions.len());
    for (t, n) in tangents.iter().zip(m.normals.iter()) {
        let tv = [t[0], t[1], t[2]];
        assert!(
            (length(tv) - 1.0).abs() < 1e-3,
            "tangent {t:?} is not unit length"
        );
        assert!(
            dot(tv, *n).abs() < 1e-3,
            "tangent {t:?} is not perpendicular to {n:?}"
        );
        assert!(
            t[3] == 1.0 || t[3] == -1.0,
            "handedness must be +-1, got {}",
            t[3]
        );
    }
}

#[test]
fn a_baked_mesh_blob_round_trips_exactly() {
    let m = primitive_mesh(Primitive::Torus, [2.0, 1.0, 2.0], 16);
    let tangents = dpaint_model3d::uv::tangents(&m);
    let blob = dpaint_model3d::encode_blob(&m, Some(&tangents));
    let (back, back_tangents) = dpaint_model3d::decode_blob(&blob).unwrap();
    assert_eq!(back, m);
    assert_eq!(back_tangents, tangents);
    assert!(dpaint_model3d::decode_blob(b"nope").is_err());
    assert!(dpaint_model3d::decode_blob(&blob[..blob.len() - 8]).is_err());
}

#[test]
fn world_transforms_compose_down_the_hierarchy() {
    let (_tmp, assets) = store();
    let (mut project, doc) = model_project();
    {
        let model = project.model_mut(&doc).unwrap();
        model.meshes.push(Mesh {
            id: MeshId::from("msh_b"),
            name: "b".into(),
            source: MeshSource::Primitive {
                shape: Primitive::Box,
                size: [1.0, 1.0, 1.0],
                segments: 1,
            },
        });
        let mut parent = dpaint_core::doc::model::Node::new("nd_parent".into(), "parent");
        parent.translation = [10.0, 0.0, 0.0];
        parent.children.push("nd_child".into());
        let mut child = dpaint_core::doc::model::Node::new("nd_child".into(), "child");
        child.translation = [0.0, 5.0, 0.0];
        child.mesh = Some(MeshId::from("msh_b"));
        model.nodes.push(parent);
        model.nodes.push(child);
    }
    let drawables = dpaint_model3d::scene_meshes(&project, &doc, &assets).unwrap();
    assert_eq!(drawables.len(), 1);
    let (node, mesh, _material, world) = &drawables[0];
    assert_eq!(node.as_str(), "nd_child");
    assert_eq!(mesh.triangle_count(), 12);
    // Column-major: the translation lives in the last column.
    assert_eq!([world[3][0], world[3][1], world[3][2]], [10.0, 5.0, 0.0]);
}

/// `build_mesh` must fail loudly, not silently, when a recipe cannot be built.
#[test]
fn a_broken_recipe_reports_an_error_instead_of_an_empty_mesh() {
    let (_tmp, assets) = store();
    let (mut project, doc) = model_project();
    project.model_mut(&doc).unwrap().meshes.push(Mesh {
        id: MeshId::from("msh_bad"),
        name: "bad".into(),
        source: MeshSource::Primitive {
            shape: Primitive::Sphere,
            size: [0.0, 1.0, 1.0],
            segments: 8,
        },
    });
    let err = build_mesh(&project, &doc, &MeshId::from("msh_bad"), &assets).unwrap_err();
    assert!(
        matches!(err, dpaint_core::Error::DegenerateGeometry(_)),
        "got {err:?}"
    );
}

#[test]
fn an_oversized_bevel_is_rejected_rather_than_turning_the_solid_inside_out() {
    let (_tmp, assets) = store();
    let (project, doc, mesh) = extruded(
        &assets,
        &square_path(2.0),
        FillRule::Nonzero,
        1.0,
        Some(Bevel {
            size: 5.0,
            segments: 3,
        }),
        Caps::Both,
    );
    let err = build_mesh(&project, &doc, &mesh, &assets).unwrap_err();
    assert!(
        matches!(err, dpaint_core::Error::DegenerateGeometry(_)),
        "a bevel wider than the shape must be an error, got {err:?}"
    );
}

fn extruded(
    assets: &dpaint_core::AssetStore,
    d: &str,
    rule: FillRule,
    depth: f32,
    bevel: Option<Bevel>,
    caps: Caps,
) -> (Project, dpaint_core::DocId, MeshId) {
    let _ = assets;
    let (mut project, doc) = model_project();
    let vdoc = add_vector_doc(&mut project, &[("obj_shape", d)], rule);
    project.model_mut(&doc).unwrap().meshes.push(Mesh {
        id: MeshId::from("msh_e"),
        name: "extruded".into(),
        source: MeshSource::Extrude {
            from: dpaint_core::doc::model::PathRef {
                document: vdoc,
                object: "obj_shape".into(),
            },
            depth,
            bevel,
            caps,
            flatten: 0.05,
        },
    });
    (project, doc, MeshId::from("msh_e"))
}
