//! The op catalog, driven the way the CLI and MCP drive it: ids, schemas, and effects.

mod common;

use common::*;
use dpaint_core::doc::common::FillRule;
use dpaint_core::doc::model::{Mesh, MeshSource, Primitive};
use dpaint_core::{DocId, MeshId, OpCx, OpEffect, Project, Registry, Result};

/// Exactly the `model.*` catalog in docs/op-registry.md.
const CATALOG: &[&str] = &[
    "model.node.add",
    "model.node.remove",
    "model.node.reparent",
    "model.node.rename",
    "model.node.set-trs",
    "model.node.look-at",
    "model.mesh.primitive",
    "model.mesh.extrude",
    "model.mesh.revolve",
    "model.mesh.loft",
    "model.mesh.from-text",
    "model.mesh.merge",
    "model.mesh.weld",
    "model.mesh.recompute-normals",
    "model.mesh.generate-tangents",
    "model.mesh.generate-uv",
    "model.mesh.transform-bake",
    "model.mesh.decimate",
    "model.mesh.import",
    "model.material.create",
    "model.material.set-pbr",
    "model.material.set-texture",
    "model.material.from-raster-doc",
    "model.material.assign",
    "model.light.add",
    "model.light.set",
    "model.camera.add",
    "model.camera.set",
    "model.scene.set-up-axis",
    "model.scene.center",
    "model.scene.scale-to-fit",
    "model.anim.track-add",
    "model.anim.key-add",
    "model.anim.key-remove",
    "model.anim.set-interpolation",
    "model.anim.clip-create",
    "model.validate",
];

fn registry() -> Registry {
    let mut r = Registry::new();
    r.extend(dpaint_model3d::ops());
    r
}

fn run(
    reg: &Registry,
    project: &mut Project,
    assets: &dpaint_core::AssetStore,
    id: &str,
    args: serde_json::Value,
) -> Result<OpEffect> {
    let op = reg.get(id).unwrap();
    let mut cx = OpCx::new(assets);
    op.apply(project, args, &mut cx)
}

#[test]
fn the_registry_carries_the_whole_model_catalog_with_usable_schemas() {
    let reg = registry();
    for id in CATALOG {
        let op = reg.get(id).unwrap_or_else(|_| panic!("missing op '{id}'"));
        assert_eq!(op.id(), *id);
        assert!(!op.about().is_empty(), "{id} needs a summary");
        let schema = op.schema();
        assert_eq!(
            schema["type"], "object",
            "{id} must take an object of arguments"
        );
        assert!(
            schema.get("properties").is_some() || schema.get("$ref").is_some(),
            "{id} schema has no properties"
        );
        assert_eq!(op.modes(), &[dpaint_core::doc::DocKind::Model]);
    }
    assert_eq!(
        reg.len(),
        CATALOG.len(),
        "ids beyond the catalog: {:?}",
        reg.ids()
    );
    assert!(reg.get("model.validate").unwrap().is_query());
    assert!(!reg.get("model.node.add").unwrap().is_query());
}

#[test]
fn building_a_scene_through_ops_produces_an_exportable_document() {
    let (_tmp, assets) = store();
    let reg = registry();
    let (mut project, doc) = model_project();

    let effect = run(
        &reg,
        &mut project,
        &assets,
        "model.mesh.primitive",
        serde_json::json!({ "shape": "cylinder", "name": "post", "size": [1.0, 4.0, 1.0], "segments": 24 }),
    )
    .unwrap();
    assert_eq!(effect.created, vec!["msh_post", "nd_post"]);

    run(
        &reg,
        &mut project,
        &assets,
        "model.material.create",
        serde_json::json!({ "name": "brass", "base_color": "#b5a642", "metallic": 0.9, "roughness": 0.3 }),
    )
    .unwrap();
    run(
        &reg,
        &mut project,
        &assets,
        "model.material.assign",
        serde_json::json!({ "target": "#nd_post", "material": "#mat_brass" }),
    )
    .unwrap();
    run(
        &reg,
        &mut project,
        &assets,
        "model.light.add",
        serde_json::json!({ "name": "key", "kind": "directional", "intensity": 3.0, "translation": [4.0, 6.0, 4.0], "look_at": [0.0, 0.0, 0.0] }),
    )
    .unwrap();
    run(
        &reg,
        &mut project,
        &assets,
        "model.camera.add",
        serde_json::json!({ "name": "hero", "fov": 40.0, "translation": [0.0, 2.0, 8.0], "look_at": [0.0, 2.0, 0.0] }),
    )
    .unwrap();

    let model = project.model(&doc).unwrap();
    assert_eq!(model.materials.len(), 1);
    assert_eq!(
        model
            .node(&"nd_post".into())
            .unwrap()
            .material
            .as_ref()
            .unwrap()
            .as_str(),
        "mat_brass"
    );
    assert_eq!(model.lights.len(), 1);
    assert_eq!(model.cameras.len(), 1);
    // fov is authored in degrees and stored in radians, as glTF wants.
    assert!((model.cameras[0].yfov - 40f32.to_radians()).abs() < 1e-6);

    let out = dpaint_model3d::export(&project, &doc, &assets, &|_| Ok(TINY_PNG.to_vec())).unwrap();
    let g = gltf::Gltf::from_slice(&out.glb).expect("the op-built scene exports cleanly");
    assert_eq!(g.document.meshes().count(), 1);
    assert_eq!(g.document.cameras().count(), 1);
    assert_eq!(g.document.lights().unwrap().count(), 1);
}

#[test]
fn a_camera_aimed_with_look_at_actually_points_at_its_target() {
    let (_tmp, assets) = store();
    let reg = registry();
    let (mut project, doc) = model_project();
    run(
        &reg,
        &mut project,
        &assets,
        "model.camera.add",
        serde_json::json!({ "name": "hero", "translation": [0.0, 0.0, 5.0], "look_at": [0.0, 0.0, 0.0] }),
    )
    .unwrap();
    let node = project
        .model(&doc)
        .unwrap()
        .node(&"nd_hero".into())
        .unwrap()
        .clone();
    // -Z is forward in glTF; from +5 on Z, looking at the origin means no rotation at all.
    let forward = rotate(node.rotation, [0.0, 0.0, -1.0]);
    assert!(
        (forward[2] + 1.0).abs() < 1e-4 && forward[0].abs() < 1e-4 && forward[1].abs() < 1e-4,
        "forward was {forward:?}"
    );

    run(
        &reg,
        &mut project,
        &assets,
        "model.node.look-at",
        serde_json::json!({ "target": "#nd_hero", "at": [10.0, 0.0, 5.0] }),
    )
    .unwrap();
    let node = project
        .model(&doc)
        .unwrap()
        .node(&"nd_hero".into())
        .unwrap()
        .clone();
    let forward = rotate(node.rotation, [0.0, 0.0, -1.0]);
    assert!((forward[0] - 1.0).abs() < 1e-4, "forward was {forward:?}");
}

fn rotate(q: [f32; 4], v: [f32; 3]) -> [f32; 3] {
    let u = [q[0], q[1], q[2]];
    let w = q[3];
    let cross = |a: [f32; 3], b: [f32; 3]| {
        [
            a[1] * b[2] - a[2] * b[1],
            a[2] * b[0] - a[0] * b[2],
            a[0] * b[1] - a[1] * b[0],
        ]
    };
    let t = cross(u, v);
    let tt = cross(u, t);
    [
        v[0] + 2.0 * (w * t[0] + tt[0]),
        v[1] + 2.0 * (w * t[1] + tt[1]),
        v[2] + 2.0 * (w * t[2] + tt[2]),
    ]
}

#[test]
fn extruding_through_the_op_records_a_recipe_that_rebuilds_from_the_source_path() {
    let (_tmp, assets) = store();
    let reg = registry();
    let (mut project, doc) = model_project();
    add_vector_doc(
        &mut project,
        &[("obj_mark", &square_path(4.0))],
        FillRule::Nonzero,
    );

    run(
        &reg,
        &mut project,
        &assets,
        "model.mesh.extrude",
        serde_json::json!({ "path": "doc_art:#obj_mark", "depth": 2.0, "name": "mark" }),
    )
    .unwrap();
    let mesh_id = MeshId::from("msh_mark");
    let before = dpaint_model3d::build_mesh(&project, &doc, &mesh_id, &assets).unwrap();
    assert!(
        (before.volume() - 32.0).abs() < 0.01,
        "4*4*2, got {}",
        before.volume()
    );

    // Edit the source path: the mesh is a recipe, so the solid follows.
    {
        let v = project.vector_mut(&DocId::from("doc_art")).unwrap();
        let obj = v.object_mut(&"obj_mark".into()).unwrap();
        obj.kind = dpaint_core::doc::vector::VKind::Path {
            d: square_path(8.0),
        };
    }
    let after = dpaint_model3d::build_mesh(&project, &doc, &mesh_id, &assets).unwrap();
    assert!(
        (after.volume() - 128.0).abs() < 0.05,
        "8*8*2 after editing the source, got {}",
        after.volume()
    );
}

#[test]
fn baking_ops_replace_the_recipe_with_a_buffer_and_keep_the_geometry_usable() {
    let (_tmp, assets) = store();
    let reg = registry();
    let (mut project, doc) = model_project();
    run(
        &reg,
        &mut project,
        &assets,
        "model.mesh.primitive",
        serde_json::json!({ "shape": "sphere", "name": "ball", "size": [2.0, 2.0, 2.0], "segments": 32 }),
    )
    .unwrap();
    let mesh = MeshId::from("msh_ball");
    let before = dpaint_model3d::build_mesh(&project, &doc, &mesh, &assets).unwrap();

    run(
        &reg,
        &mut project,
        &assets,
        "model.mesh.decimate",
        serde_json::json!({ "target": "#msh_ball", "ratio": 0.3 }),
    )
    .unwrap();

    let source = &project.model(&doc).unwrap().mesh(&mesh).unwrap().source;
    assert!(
        matches!(source, MeshSource::Buffer { .. }),
        "decimation bakes, leaving a buffer"
    );
    let after = dpaint_model3d::build_mesh(&project, &doc, &mesh, &assets).unwrap();
    assert!(after.triangle_count() < before.triangle_count());
    assert!(after.triangle_count() > 0);
    // The baked blob is reachable from the document, so asset gc keeps it.
    assert!(project.referenced_assets().len() == 1);

    run(
        &reg,
        &mut project,
        &assets,
        "model.mesh.generate-uv",
        serde_json::json!({ "target": "#msh_ball", "mode": "box" }),
    )
    .unwrap();
    run(
        &reg,
        &mut project,
        &assets,
        "model.mesh.generate-tangents",
        serde_json::json!({ "target": "#msh_ball" }),
    )
    .unwrap();
    let tangents = dpaint_model3d::mesh_tangents(&project, &doc, &mesh, &assets).unwrap();
    let final_mesh = dpaint_model3d::build_mesh(&project, &doc, &mesh, &assets).unwrap();
    assert_eq!(
        tangents.len(),
        final_mesh.positions.len(),
        "baked tangents come back with the mesh"
    );
}

#[test]
fn transform_bake_moves_the_node_transform_into_the_vertices() {
    let (_tmp, assets) = store();
    let reg = registry();
    let (mut project, doc) = model_project();
    run(
        &reg,
        &mut project,
        &assets,
        "model.mesh.primitive",
        serde_json::json!({ "shape": "box", "name": "cube", "size": [1.0, 1.0, 1.0] }),
    )
    .unwrap();
    run(
        &reg,
        &mut project,
        &assets,
        "model.node.set-trs",
        serde_json::json!({ "target": "#nd_cube", "translation": [5.0, 0.0, 0.0], "scale": [2.0, 2.0, 2.0] }),
    )
    .unwrap();
    run(
        &reg,
        &mut project,
        &assets,
        "model.mesh.transform-bake",
        serde_json::json!({ "target": "#nd_cube" }),
    )
    .unwrap();

    let node = project
        .model(&doc)
        .unwrap()
        .node(&"nd_cube".into())
        .unwrap()
        .clone();
    assert_eq!(node.translation, [0.0, 0.0, 0.0]);
    assert_eq!(node.scale, [1.0, 1.0, 1.0]);
    let m = dpaint_model3d::build_mesh(&project, &doc, &MeshId::from("msh_cube"), &assets).unwrap();
    let (lo, hi) = m.bounds().unwrap();
    assert_eq!(
        [lo[0], hi[0]],
        [4.0, 6.0],
        "the translation and scale are in the vertices"
    );
    assert!((m.volume() - 8.0).abs() < 1e-3);
}

#[test]
fn merging_nodes_unions_their_world_space_geometry_and_retires_the_sources() {
    let (_tmp, assets) = store();
    let reg = registry();
    let (mut project, doc) = model_project();
    for (name, x) in [("left", -3.0f32), ("right", 3.0f32)] {
        run(
            &reg,
            &mut project,
            &assets,
            "model.mesh.primitive",
            serde_json::json!({ "shape": "box", "name": name, "size": [1.0, 1.0, 1.0] }),
        )
        .unwrap();
        run(
            &reg,
            &mut project,
            &assets,
            "model.node.set-trs",
            serde_json::json!({ "target": format!("#nd_{name}"), "translation": [x, 0.0, 0.0] }),
        )
        .unwrap();
    }
    let effect = run(
        &reg,
        &mut project,
        &assets,
        "model.mesh.merge",
        serde_json::json!({ "targets": "node", "name": "pair" }),
    )
    .unwrap();
    assert!(effect.removed.contains(&"nd_left".to_string()));
    assert!(effect.removed.contains(&"nd_right".to_string()));

    let model = project.model(&doc).unwrap();
    assert_eq!(model.nodes.len(), 1, "only the merged node remains");
    assert_eq!(model.meshes.len(), 1, "the orphaned meshes are gone");
    let m = dpaint_model3d::build_mesh(&project, &doc, &MeshId::from("msh_pair"), &assets).unwrap();
    assert_eq!(m.triangle_count(), 24, "both boxes");
    let (lo, hi) = m.bounds().unwrap();
    assert_eq!([lo[0], hi[0]], [-3.5, 3.5], "world positions were baked in");
    assert!((m.volume() - 2.0).abs() < 1e-3);
}

#[test]
fn scene_center_and_scale_to_fit_reposition_the_whole_scene() {
    let (_tmp, assets) = store();
    let reg = registry();
    let (mut project, doc) = model_project();
    run(
        &reg,
        &mut project,
        &assets,
        "model.mesh.primitive",
        serde_json::json!({ "shape": "box", "name": "cube", "size": [2.0, 2.0, 2.0] }),
    )
    .unwrap();
    run(
        &reg,
        &mut project,
        &assets,
        "model.node.set-trs",
        serde_json::json!({ "target": "#nd_cube", "translation": [10.0, 4.0, 0.0] }),
    )
    .unwrap();

    run(
        &reg,
        &mut project,
        &assets,
        "model.scene.center",
        serde_json::json!({}),
    )
    .unwrap();
    let bounds = dpaint_model3d::export::scene_bounds(&project, &doc, &assets)
        .unwrap()
        .unwrap();
    for i in 0..3 {
        assert!(
            (bounds.0[i] + bounds.1[i]).abs() < 1e-4,
            "axis {i} is not centred: {bounds:?}"
        );
    }

    run(
        &reg,
        &mut project,
        &assets,
        "model.scene.scale-to-fit",
        serde_json::json!({ "size": 10.0 }),
    )
    .unwrap();
    let (lo, hi) = dpaint_model3d::export::scene_bounds(&project, &doc, &assets)
        .unwrap()
        .unwrap();
    let extent = (hi[0] - lo[0]).max(hi[1] - lo[1]).max(hi[2] - lo[2]);
    assert!((extent - 10.0).abs() < 1e-3, "largest extent is {extent}");

    run(
        &reg,
        &mut project,
        &assets,
        "model.scene.center",
        serde_json::json!({ "mode": "ground" }),
    )
    .unwrap();
    let (lo, _) = dpaint_model3d::export::scene_bounds(&project, &doc, &assets)
        .unwrap()
        .unwrap();
    assert!(
        lo[1].abs() < 1e-4,
        "ground mode rests the scene on y=0, got {}",
        lo[1]
    );
}

#[test]
fn animation_ops_build_a_track_and_guard_its_key_shape() {
    let (_tmp, assets) = store();
    let reg = registry();
    let (mut project, doc) = model_project();
    run(
        &reg,
        &mut project,
        &assets,
        "model.mesh.primitive",
        serde_json::json!({ "shape": "box", "name": "cube" }),
    )
    .unwrap();
    run(
        &reg,
        &mut project,
        &assets,
        "model.anim.clip-create",
        serde_json::json!({ "name": "spin" }),
    )
    .unwrap();
    run(
        &reg,
        &mut project,
        &assets,
        "model.anim.track-add",
        serde_json::json!({ "animation": "anm_spin", "node": "#nd_cube", "path": "rotation", "interpolation": "LINEAR" }),
    )
    .unwrap();

    // A rotation key needs four numbers.
    let err = run(
        &reg,
        &mut project,
        &assets,
        "model.anim.key-add",
        serde_json::json!({ "animation": "anm_spin", "node": "#nd_cube", "path": "rotation", "t": 0.0, "value": [0.0, 0.0, 0.0] }),
    )
    .unwrap_err();
    assert!(err.to_string().contains("needs 4 values"), "got {err}");

    for (t, v) in [(0.0, [0.0, 0.0, 0.0, 1.0]), (1.0, [0.0, 1.0, 0.0, 0.0])] {
        run(
            &reg,
            &mut project,
            &assets,
            "model.anim.key-add",
            serde_json::json!({ "animation": "spin", "node": "#nd_cube", "path": "rotation", "t": t, "value": v }),
        )
        .unwrap();
    }
    assert_eq!(
        project.model(&doc).unwrap().animations[0].channels[0]
            .keys
            .len(),
        2
    );

    // Switching to CUBICSPLINE rewrites existing keys into tangent triples.
    run(
        &reg,
        &mut project,
        &assets,
        "model.anim.set-interpolation",
        serde_json::json!({ "animation": "spin", "node": "#nd_cube", "path": "rotation", "interpolation": "CUBICSPLINE" }),
    )
    .unwrap();
    let channel = &project.model(&doc).unwrap().animations[0].channels[0];
    assert_eq!(
        channel.interpolation,
        dpaint_core::doc::model::Interpolation::CubicSpline
    );
    assert_eq!(channel.keys[0].v.len(), 12);
    assert_eq!(
        &channel.keys[0].v[4..8],
        &[0.0, 0.0, 0.0, 1.0],
        "the value is preserved"
    );

    let out = dpaint_model3d::export(&project, &doc, &assets, &|_| Ok(TINY_PNG.to_vec())).unwrap();
    let g = gltf::Gltf::from_slice(&out.glb).unwrap();
    let anim = g.document.animations().next().unwrap();
    assert_eq!(
        anim.channels().next().unwrap().sampler().interpolation(),
        gltf::animation::Interpolation::CubicSpline
    );

    run(
        &reg,
        &mut project,
        &assets,
        "model.anim.key-remove",
        serde_json::json!({ "animation": "spin", "node": "#nd_cube", "path": "rotation", "t": 1.0 }),
    )
    .unwrap();
    assert_eq!(
        project.model(&doc).unwrap().animations[0].channels[0]
            .keys
            .len(),
        1
    );
}

#[test]
fn node_removal_takes_the_subtree_and_its_animation_channels() {
    let (_tmp, assets) = store();
    let reg = registry();
    let (mut project, doc) = model_project();
    run(
        &reg,
        &mut project,
        &assets,
        "model.node.add",
        serde_json::json!({ "name": "root" }),
    )
    .unwrap();
    run(
        &reg,
        &mut project,
        &assets,
        "model.node.add",
        serde_json::json!({ "name": "child", "parent": "#nd_root" }),
    )
    .unwrap();
    run(
        &reg,
        &mut project,
        &assets,
        "model.anim.track-add",
        serde_json::json!({ "animation": "walk", "node": "#nd_child", "path": "translation" }),
    )
    .unwrap();
    assert_eq!(project.model(&doc).unwrap().animations[0].channels.len(), 1);

    let effect = run(
        &reg,
        &mut project,
        &assets,
        "model.node.remove",
        serde_json::json!({ "target": "#nd_root" }),
    )
    .unwrap();
    assert_eq!(effect.removed.len(), 2);
    assert!(project.model(&doc).unwrap().nodes.is_empty());
    assert!(
        project.model(&doc).unwrap().animations[0]
            .channels
            .is_empty(),
        "channels targeting a removed node must go too"
    );
}

#[test]
fn reparenting_refuses_to_build_a_cycle() {
    let (_tmp, assets) = store();
    let reg = registry();
    let (mut project, _doc) = model_project();
    run(
        &reg,
        &mut project,
        &assets,
        "model.node.add",
        serde_json::json!({ "name": "a" }),
    )
    .unwrap();
    run(
        &reg,
        &mut project,
        &assets,
        "model.node.add",
        serde_json::json!({ "name": "b", "parent": "#nd_a" }),
    )
    .unwrap();
    let err = run(
        &reg,
        &mut project,
        &assets,
        "model.node.reparent",
        serde_json::json!({ "target": "#nd_a", "parent": "#nd_b" }),
    )
    .unwrap_err();
    assert!(
        matches!(err, dpaint_core::Error::CyclicLink { .. }),
        "got {err:?}"
    );
}

#[test]
fn a_failed_op_leaves_the_document_untouched() {
    let (_tmp, assets) = store();
    let reg = registry();
    let (mut project, doc) = model_project();
    run(
        &reg,
        &mut project,
        &assets,
        "model.mesh.primitive",
        serde_json::json!({ "shape": "box", "name": "cube" }),
    )
    .unwrap();
    let before = project.model(&doc).unwrap().clone();

    // A material selector that matches nothing.
    let err = run(
        &reg,
        &mut project,
        &assets,
        "model.material.assign",
        serde_json::json!({ "target": "#nd_cube", "material": "#mat_nope" }),
    )
    .unwrap_err();
    assert!(
        matches!(err, dpaint_core::Error::SelectorNoMatch { .. }),
        "got {err:?}"
    );
    assert_eq!(project.model(&doc).unwrap(), &before);

    // An impossible primitive never lands a half-built mesh.
    let err = run(
        &reg,
        &mut project,
        &assets,
        "model.mesh.primitive",
        serde_json::json!({ "shape": "sphere", "name": "bad", "size": [1.0, -1.0, 1.0] }),
    )
    .unwrap_err();
    assert!(
        matches!(err, dpaint_core::Error::DegenerateGeometry(_)),
        "got {err:?}"
    );
    assert_eq!(project.model(&doc).unwrap(), &before);
}

#[test]
fn importing_an_exported_glb_reproduces_the_geometry() {
    let (tmp, assets) = store();
    let reg = registry();
    let (mut project, doc) = model_project();
    project.model_mut(&doc).unwrap().meshes.push(Mesh {
        id: MeshId::from("msh_src"),
        name: "src".into(),
        source: MeshSource::Primitive {
            shape: Primitive::Torus,
            size: [3.0, 1.0, 3.0],
            segments: 20,
        },
    });
    let mut node = dpaint_core::doc::model::Node::new("nd_src".into(), "src");
    node.mesh = Some(MeshId::from("msh_src"));
    project.model_mut(&doc).unwrap().nodes.push(node);
    let original =
        dpaint_model3d::build_mesh(&project, &doc, &MeshId::from("msh_src"), &assets).unwrap();

    let out = dpaint_model3d::export(&project, &doc, &assets, &|_| Ok(TINY_PNG.to_vec())).unwrap();
    let path = tmp.path().join("torus.glb");
    std::fs::write(&path, &out.glb).unwrap();

    let (mut project2, doc2) = model_project();
    run(
        &reg,
        &mut project2,
        &assets,
        "model.mesh.import",
        serde_json::json!({ "file": path.to_str().unwrap(), "name": "ring" }),
    )
    .unwrap();
    let imported =
        dpaint_model3d::build_mesh(&project2, &doc2, &MeshId::from("msh_ring"), &assets).unwrap();
    assert_eq!(imported.triangle_count(), original.triangle_count());
    assert!(
        (imported.surface_area() - original.surface_area()).abs() / original.surface_area() < 1e-4
    );
    let (lo0, hi0) = original.bounds().unwrap();
    let (lo1, hi1) = imported.bounds().unwrap();
    for i in 0..3 {
        assert!((lo0[i] - lo1[i]).abs() < 1e-4 && (hi0[i] - hi1[i]).abs() < 1e-4);
    }
}

#[test]
fn material_from_a_raster_doc_binds_that_document_and_refuses_a_model_one() {
    let (_tmp, assets) = store();
    let reg = registry();
    let (mut project, doc) = model_project();
    project.add_document(dpaint_core::doc::Document::Raster(
        dpaint_core::RasterDoc::new(DocId::from("doc_skin"), "skin", 16, 16),
    ));
    run(
        &reg,
        &mut project,
        &assets,
        "model.material.from-raster-doc",
        serde_json::json!({ "source": "doc_skin", "name": "skin", "roughness": 0.8 }),
    )
    .unwrap();
    let mat = project.model(&doc).unwrap().materials[0].clone();
    assert_eq!(mat.roughness, 0.8);
    assert!(matches!(
        &mat.textures[0].source,
        dpaint_core::doc::model::TextureSource::Document { document } if document.as_str() == "doc_skin"
    ));
    // The model document now depends on the raster one, which is what render ordering needs.
    assert!(project
        .doc(&doc)
        .unwrap()
        .dependencies()
        .contains(&DocId::from("doc_skin")));

    let err = run(
        &reg,
        &mut project,
        &assets,
        "model.material.from-raster-doc",
        serde_json::json!({ "source": "doc_scene", "name": "self" }),
    )
    .unwrap_err();
    assert!(
        matches!(err, dpaint_core::Error::WrongDocumentKind { .. }),
        "got {err:?}"
    );
}

/// Every op in the catalog, executed once against a real document. An op that has never
/// run is an op whose selector types and arg plumbing were never checked.
#[test]
fn every_op_in_the_catalog_runs_and_changes_something() {
    let (tmp, assets) = store();
    let reg = registry();
    let (mut project, doc) = model_project();
    add_vector_doc(
        &mut project,
        &[
            ("obj_square", &square_path(4.0)),
            ("obj_small", &square_path(2.0)),
            ("obj_profile", "M 2 0 L 2 -4"),
        ],
        FillRule::Nonzero,
    );
    {
        let v = project.vector_mut(&DocId::from("doc_art")).unwrap();
        v.objects.push(dpaint_core::doc::vector::VObject::new(
            "obj_label".into(),
            "label",
            dpaint_core::doc::vector::VKind::Text {
                spec: dpaint_core::doc::common::TextSpec::new("Hi"),
                origin: [0.0, 0.0],
                on_path: None,
            },
        ));
    }
    project.add_document(dpaint_core::doc::Document::Raster(
        dpaint_core::RasterDoc::new(DocId::from("doc_skin"), "skin", 16, 16),
    ));
    let png = assets.put(TINY_PNG, "png").unwrap();

    let mut ran: Vec<&str> = Vec::new();
    let mut go = |project: &mut Project, id: &'static str, args: serde_json::Value| {
        run(&reg, project, &assets, id, args).unwrap_or_else(|e| panic!("{id} failed: {e}"));
        ran.push(id);
    };

    go(
        &mut project,
        "model.mesh.primitive",
        serde_json::json!({ "shape": "box", "name": "cube", "size": [2.0, 2.0, 2.0] }),
    );
    go(
        &mut project,
        "model.mesh.extrude",
        serde_json::json!({ "path": "doc_art:#obj_square", "depth": 1.0, "name": "slab", "bevel": 0.2 }),
    );
    go(
        &mut project,
        "model.mesh.revolve",
        serde_json::json!({ "path": "doc_art:#obj_profile", "name": "vase", "segments": 16 }),
    );
    go(
        &mut project,
        "model.mesh.loft",
        serde_json::json!({ "paths": ["doc_art:#obj_square", "doc_art:#obj_small"], "name": "taper" }),
    );
    go(
        &mut project,
        "model.mesh.from-text",
        serde_json::json!({ "path": "doc_art:#obj_label", "depth": 0.5, "name": "label" }),
    );
    go(
        &mut project,
        "model.mesh.weld",
        serde_json::json!({ "target": "#msh_slab", "tolerance": 1e-4 }),
    );
    go(
        &mut project,
        "model.mesh.recompute-normals",
        serde_json::json!({ "target": "#msh_slab", "flat": true }),
    );
    go(
        &mut project,
        "model.mesh.generate-uv",
        serde_json::json!({ "target": "#msh_slab", "mode": "unwrap", "angle": 50.0 }),
    );
    go(
        &mut project,
        "model.mesh.generate-uv",
        serde_json::json!({ "target": "#msh_cube", "mode": "planar", "axis": "z" }),
    );
    go(
        &mut project,
        "model.mesh.generate-tangents",
        serde_json::json!({ "target": "#msh_slab" }),
    );
    go(
        &mut project,
        "model.mesh.decimate",
        serde_json::json!({ "target": "#msh_vase", "ratio": 0.5 }),
    );
    go(
        &mut project,
        "model.mesh.transform-bake",
        serde_json::json!({ "target": "#nd_vase" }),
    );

    go(
        &mut project,
        "model.material.create",
        serde_json::json!({ "name": "paint", "base_color": "#3366cc", "roughness": 0.4 }),
    );
    go(
        &mut project,
        "model.material.set-pbr",
        serde_json::json!({ "target": "#mat_paint", "metallic": 0.2, "emissive": "#101010", "alpha_mode": "MASK", "double_sided": true }),
    );
    go(
        &mut project,
        "model.material.set-texture",
        serde_json::json!({ "target": "#mat_paint", "slot": "base-color", "asset": png.as_str() }),
    );
    go(
        &mut project,
        "model.material.set-texture",
        serde_json::json!({ "target": "#mat_paint", "slot": "normal", "source": "doc_skin", "scale": 0.7 }),
    );
    go(
        &mut project,
        "model.material.from-raster-doc",
        serde_json::json!({ "source": "doc_skin", "name": "decal" }),
    );
    go(
        &mut project,
        "model.material.assign",
        serde_json::json!({ "target": "#nd_cube", "material": "#mat_paint" }),
    );

    go(
        &mut project,
        "model.node.add",
        serde_json::json!({ "name": "pivot" }),
    );
    go(
        &mut project,
        "model.node.rename",
        serde_json::json!({ "target": "#nd_pivot", "name": "hub" }),
    );
    go(
        &mut project,
        "model.node.reparent",
        serde_json::json!({ "target": "#nd_cube", "parent": "#nd_pivot" }),
    );
    go(
        &mut project,
        "model.node.set-trs",
        serde_json::json!({ "target": "#nd_pivot", "rotation": [0.0, 45.0, 0.0] }),
    );
    go(
        &mut project,
        "model.node.look-at",
        serde_json::json!({ "target": "#nd_pivot", "at": [0.0, 0.0, -1.0] }),
    );

    go(
        &mut project,
        "model.light.add",
        serde_json::json!({ "name": "fill", "kind": "point", "intensity": 50.0 }),
    );
    go(
        &mut project,
        "model.light.set",
        serde_json::json!({ "target": "#lgt_fill", "intensity": 75.0, "range": 12.0, "color": "#ffeedd" }),
    );
    go(
        &mut project,
        "model.camera.add",
        serde_json::json!({ "name": "wide", "fov": 60.0 }),
    );
    go(
        &mut project,
        "model.camera.set",
        serde_json::json!({ "target": "#cam_wide", "fov": 50.0, "zfar": 80.0 }),
    );
    go(
        &mut project,
        "model.scene.set-up-axis",
        serde_json::json!({ "axis": "z" }),
    );
    go(&mut project, "model.scene.center", serde_json::json!({}));
    go(
        &mut project,
        "model.scene.scale-to-fit",
        serde_json::json!({ "size": 4.0 }),
    );

    go(
        &mut project,
        "model.anim.clip-create",
        serde_json::json!({ "name": "idle" }),
    );
    go(
        &mut project,
        "model.anim.track-add",
        serde_json::json!({ "animation": "idle", "node": "#nd_cube", "path": "translation" }),
    );
    go(
        &mut project,
        "model.anim.key-add",
        serde_json::json!({ "animation": "idle", "node": "#nd_cube", "path": "translation", "t": 0.0, "value": [0.0, 0.0, 0.0] }),
    );
    go(
        &mut project,
        "model.anim.key-add",
        serde_json::json!({ "animation": "idle", "node": "#nd_cube", "path": "translation", "t": 1.0, "value": [0.0, 1.0, 0.0] }),
    );
    go(
        &mut project,
        "model.anim.set-interpolation",
        serde_json::json!({ "animation": "idle", "node": "#nd_cube", "path": "translation", "interpolation": "STEP" }),
    );
    go(
        &mut project,
        "model.anim.key-remove",
        serde_json::json!({ "animation": "idle", "node": "#nd_cube", "path": "translation", "t": 1.0 }),
    );

    // mesh.merge needs two nodes with meshes; slab and label still have theirs.
    go(
        &mut project,
        "model.mesh.merge",
        serde_json::json!({ "targets": "#nd_slab, #nd_label", "name": "signage" }),
    );

    let glb = {
        let out =
            dpaint_model3d::export(&project, &doc, &assets, &|_| Ok(TINY_PNG.to_vec())).unwrap();
        let path = tmp.path().join("scene.glb");
        std::fs::write(&path, &out.glb).unwrap();
        path
    };
    go(
        &mut project,
        "model.mesh.import",
        serde_json::json!({ "file": glb.to_str().unwrap(), "name": "reimported", "node": false }),
    );

    // Ids never change, so the renamed node is still #nd_pivot; address it by name.
    go(
        &mut project,
        "model.node.remove",
        serde_json::json!({ "target": "@hub" }),
    );
    go(&mut project, "model.validate", serde_json::json!({}));

    let mut expected: Vec<&str> = CATALOG.to_vec();
    expected.sort_unstable();
    let mut seen: Vec<&str> = ran.clone();
    seen.sort_unstable();
    seen.dedup();
    assert_eq!(seen, expected, "some catalog ops never ran");

    // The document that came out of all of that still exports as valid glTF.
    let out = dpaint_model3d::export(&project, &doc, &assets, &|_| Ok(TINY_PNG.to_vec())).unwrap();
    gltf::Gltf::from_slice(&out.glb).expect("the fully exercised scene still exports");
}
