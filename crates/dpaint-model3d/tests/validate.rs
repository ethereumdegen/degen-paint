//! `model.validate`: it has to catch real problems and stay quiet about healthy documents.

mod common;

use common::*;
use dpaint_core::doc::model::{
    Material, Mesh, MeshSource, Node, Primitive, TextureBinding, TextureSlot, TextureSource,
};
use dpaint_core::{AssetStore, DocId, MeshId, OpCx, Project, Registry};
use dpaint_model3d::geom::MeshData;

fn registry() -> Registry {
    let mut r = Registry::new();
    r.extend(dpaint_model3d::ops());
    r
}

fn validate(project: &mut Project, assets: &AssetStore, max_texture: u32) -> serde_json::Value {
    let reg = registry();
    let op = reg.get("model.validate").unwrap();
    let mut cx = OpCx::new(assets);
    let effect = op
        .apply(
            project,
            serde_json::json!({ "max_texture_size": max_texture }),
            &mut cx,
        )
        .unwrap();
    assert!(
        effect.changed.is_empty(),
        "a query op must not report changes"
    );
    effect.data.expect("findings")
}

fn codes(report: &serde_json::Value) -> Vec<String> {
    report["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["code"].as_str().unwrap().to_string())
        .collect()
}

/// Bake arbitrary triangles into the store and hang a node off them.
fn scene_with_mesh(assets: &AssetStore, mesh: &MeshData) -> (Project, DocId) {
    let (mut project, doc) = model_project();
    let asset = dpaint_model3d::build::bake(assets, mesh, None).unwrap();
    let model = project.model_mut(&doc).unwrap();
    model.meshes.push(Mesh {
        id: MeshId::from("msh_test"),
        name: "test".into(),
        source: MeshSource::Buffer { asset },
    });
    let mut node = Node::new("nd_test".into(), "test");
    node.mesh = Some(MeshId::from("msh_test"));
    model.nodes.push(node);
    (project, doc)
}

#[test]
fn a_clean_scene_passes_with_no_errors() {
    let (_tmp, assets) = store();
    let (mut project, doc) = model_project();
    let model = project.model_mut(&doc).unwrap();
    model.meshes.push(Mesh {
        id: MeshId::from("msh_box"),
        name: "box".into(),
        source: MeshSource::Primitive {
            shape: Primitive::Box,
            size: [1.0, 1.0, 1.0],
            segments: 1,
        },
    });
    let mut node = Node::new("nd_box".into(), "box");
    node.mesh = Some(MeshId::from("msh_box"));
    model.nodes.push(node);

    let report = validate(&mut project, &assets, 4096);
    assert_eq!(
        report["ok"],
        serde_json::json!(true),
        "report was {report:#}"
    );
    assert_eq!(report["errors"], serde_json::json!(0));
    assert!(
        codes(&report).is_empty(),
        "a closed, manifold box should be silent, got {:?}",
        codes(&report)
    );
}

#[test]
fn a_non_manifold_mesh_is_flagged() {
    let (_tmp, assets) = store();
    // Three triangles fanned around one shared edge: a classic non-manifold "T".
    let mesh = MeshData {
        positions: vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
            [0.0, -1.0, 0.0],
        ],
        normals: vec![[0.0, 0.0, 1.0]; 5],
        uvs: vec![[0.0, 0.0]; 5],
        indices: vec![0, 1, 2, 0, 1, 3, 0, 1, 4],
    };
    let (mut project, _doc) = scene_with_mesh(&assets, &mesh);

    let report = validate(&mut project, &assets, 4096);
    assert_eq!(report["ok"], serde_json::json!(false));
    assert!(
        codes(&report).contains(&"non-manifold".to_string()),
        "expected a non-manifold finding, got {:?}",
        codes(&report)
    );
    let finding = report["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["code"] == "non-manifold")
        .unwrap();
    assert_eq!(finding["severity"], "error");
    assert_eq!(finding["target"], "msh_test");
    assert!(
        finding["detail"]
            .as_str()
            .unwrap()
            .contains("three or more"),
        "detail should say what is wrong: {finding}"
    );
}

#[test]
fn degenerate_triangles_are_reported() {
    let (_tmp, assets) = store();
    let mesh = MeshData {
        positions: vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [2.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
        ],
        normals: vec![[0.0, 0.0, 1.0]; 4],
        uvs: vec![[0.0, 0.0]; 4],
        // The first triangle is collinear, so it has no area at all.
        indices: vec![0, 1, 2, 0, 1, 3],
    };
    let (mut project, _doc) = scene_with_mesh(&assets, &mesh);
    let report = validate(&mut project, &assets, 4096);
    let found = codes(&report);
    assert!(
        found.contains(&"degenerate-triangles".to_string()),
        "got {found:?}"
    );
    assert!(
        found.contains(&"open-surface".to_string()),
        "two loose triangles are an open surface: {found:?}"
    );
}

#[test]
fn a_bound_texture_without_uvs_is_an_error() {
    let (_tmp, assets) = store();
    let mesh = MeshData {
        positions: vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
        normals: vec![[0.0, 0.0, 1.0]; 3],
        uvs: Vec::new(),
        indices: vec![0, 1, 2],
    };
    let (mut project, doc) = scene_with_mesh(&assets, &mesh);
    let png = assets.put(TINY_PNG, "png").unwrap();
    {
        let model = project.model_mut(&doc).unwrap();
        let mut mat = Material::new("mat_skin".into(), "skin");
        mat.textures.push(TextureBinding {
            slot: TextureSlot::BaseColor,
            source: TextureSource::Asset { asset: png },
            scale: 1.0,
            uv_set: 0,
        });
        model.materials.push(mat);
        model.node_mut(&"nd_test".into()).unwrap().material = Some("mat_skin".into());
    }
    let report = validate(&mut project, &assets, 4096);
    assert!(
        codes(&report).contains(&"missing-uv".to_string()),
        "got {:?}",
        codes(&report)
    );
    assert_eq!(report["ok"], serde_json::json!(false));
}

#[test]
fn an_oversized_texture_warns_without_failing_the_document() {
    let (_tmp, assets) = store();
    let (mut project, doc) = model_project();
    let png = assets.put(TINY_PNG, "png").unwrap();
    let model = project.model_mut(&doc).unwrap();
    let mut mat = Material::new("mat_skin".into(), "skin");
    mat.textures.push(TextureBinding {
        slot: TextureSlot::BaseColor,
        source: TextureSource::Asset { asset: png },
        scale: 1.0,
        uv_set: 0,
    });
    model.materials.push(mat);

    // The fixture PNG is 2x2, so a 1px ceiling must trip and a 2px ceiling must not.
    let report = validate(&mut project, &assets, 1);
    assert!(codes(&report).contains(&"texture-oversized".to_string()));
    assert_eq!(
        report["ok"],
        serde_json::json!(true),
        "oversize is a warning, not an error"
    );
    assert_eq!(report["warnings"], serde_json::json!(1));

    let report = validate(&mut project, &assets, 2);
    assert!(!codes(&report).contains(&"texture-oversized".to_string()));
}

#[test]
fn dangling_references_are_named() {
    let (_tmp, assets) = store();
    let (mut project, doc) = model_project();
    {
        let model = project.model_mut(&doc).unwrap();
        let mut node = Node::new("nd_ghost".into(), "ghost");
        node.mesh = Some(MeshId::from("msh_missing"));
        node.material = Some("mat_missing".into());
        model.nodes.push(node);
        let mut mat = Material::new("mat_bad_tex".into(), "bad");
        mat.textures.push(TextureBinding {
            slot: TextureSlot::BaseColor,
            source: TextureSource::Document {
                document: DocId::from("doc_nope"),
            },
            scale: 1.0,
            uv_set: 0,
        });
        model.materials.push(mat);
    }
    let report = validate(&mut project, &assets, 4096);
    let found = codes(&report);
    assert!(found.contains(&"missing-mesh".to_string()), "got {found:?}");
    assert!(
        found.contains(&"missing-material".to_string()),
        "got {found:?}"
    );
    assert!(
        found.contains(&"texture-unresolved".to_string()),
        "got {found:?}"
    );
    assert_eq!(report["ok"], serde_json::json!(false));
}

#[test]
fn an_open_extrusion_reports_boundary_edges_as_information_not_failure() {
    let (_tmp, assets) = store();
    let (mut project, doc) = model_project();
    add_vector_doc(
        &mut project,
        &[("obj_shape", &square_path(2.0))],
        dpaint_core::doc::common::FillRule::Nonzero,
    );
    let model = project.model_mut(&doc).unwrap();
    model.meshes.push(Mesh {
        id: MeshId::from("msh_tube"),
        name: "tube".into(),
        source: MeshSource::Extrude {
            from: path_ref(),
            depth: 1.0,
            bevel: None,
            caps: dpaint_core::doc::model::Caps::None,
            flatten: 0.25,
        },
    });
    let mut node = Node::new("nd_tube".into(), "tube");
    node.mesh = Some(MeshId::from("msh_tube"));
    model.nodes.push(node);

    let report = validate(&mut project, &assets, 4096);
    let found = codes(&report);
    assert_eq!(found, vec!["open-surface".to_string()], "got {found:?}");
    assert_eq!(
        report["ok"],
        serde_json::json!(true),
        "an open tube is legal glTF"
    );
}

#[test]
fn a_mesh_whose_recipe_cannot_build_is_reported_rather_than_panicking() {
    let (_tmp, assets) = store();
    let (mut project, doc) = model_project();
    let model = project.model_mut(&doc).unwrap();
    model.meshes.push(Mesh {
        id: MeshId::from("msh_gone"),
        name: "gone".into(),
        source: MeshSource::Extrude {
            from: dpaint_core::doc::model::PathRef {
                document: DocId::from("doc_missing"),
                object: "obj_missing".into(),
            },
            depth: 1.0,
            bevel: None,
            caps: dpaint_core::doc::model::Caps::Both,
            flatten: 0.25,
        },
    });
    let report = validate(&mut project, &assets, 4096);
    assert!(
        codes(&report).contains(&"mesh-build-failed".to_string()),
        "got {:?}",
        codes(&report)
    );
}
