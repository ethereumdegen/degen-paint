// Each test binary compiles this module and uses a different subset of it.
#![allow(dead_code)]

//! A scratch project on disk with one document of each kind the viewport can show.

use dpaint_core::doc::common::Paint;
use dpaint_core::doc::model::{Material, Mesh, MeshSource, Node, Primitive};
use dpaint_core::doc::raster::{Layer, LayerKind};
use dpaint_core::doc::vector::{VKind, VObject};
use dpaint_core::doc::Document;
use dpaint_core::{
    Color, DocId, MaterialId, MeshId, ModelDoc, NodeId, Project, RasterDoc, VectorDoc, Workspace,
};
use std::path::{Path, PathBuf};

pub const MODEL_DOC: &str = "doc_scene";
pub const RASTER_DOC: &str = "doc_canvas";
pub const VECTOR_DOC: &str = "doc_art";

/// Build the demo project under `root`.
///
/// The model document holds a box and a sphere in two different materials, which is
/// enough geometry to tell a correct render from a broken one: a flipped normal, a wrong
/// matrix convention or an unlit material all show up on it.
pub fn write_project(root: &Path) -> Workspace {
    let mut raster = RasterDoc::new(DocId::from(RASTER_DOC), "canvas", 320, 200);
    raster.background = Some(Color::rgb8(30, 34, 46));
    raster.layers.push(Layer::new(
        "lay_disc".into(),
        "disc",
        LayerKind::Shape {
            d: "M 40 40 L 280 40 L 280 160 L 40 160 Z".into(),
            fill: Paint::Solid {
                color: Color::rgb8(232, 96, 64),
            },
            stroke: None,
            fill_rule: Default::default(),
        },
    ));
    let mut project = Project::new("viewport scratch", Document::Raster(raster));

    let mut vector = VectorDoc::new(DocId::from(VECTOR_DOC), "art", 240.0, 240.0);
    let mut disc = VObject::new(
        "obj_disc".into(),
        "disc",
        VKind::Ellipse {
            center: [120.0, 120.0],
            radius: [90.0, 70.0],
        },
    );
    disc.fill = Paint::Solid {
        color: Color::rgb8(80, 190, 220),
    };
    vector.objects.push(disc);
    project.add_document(Document::Vector(vector));

    let mut model = ModelDoc::new(DocId::from(MODEL_DOC), "scene");
    model.materials.push(Material {
        base_color: Color::rgb8(220, 180, 90),
        metallic: 0.1,
        roughness: 0.45,
        ..Material::new(MaterialId::from("mat_gold"), "gold")
    });
    model.materials.push(Material {
        base_color: Color::rgb8(90, 120, 220),
        roughness: 0.8,
        ..Material::new(MaterialId::from("mat_blue"), "blue")
    });
    model.meshes.push(Mesh {
        id: MeshId::from("msh_box"),
        name: "box".into(),
        source: MeshSource::Primitive {
            shape: Primitive::Box,
            size: [1.4, 1.4, 1.4],
            segments: 1,
        },
    });
    model.meshes.push(Mesh {
        id: MeshId::from("msh_ball"),
        name: "ball".into(),
        source: MeshSource::Primitive {
            shape: Primitive::Sphere,
            size: [1.0, 1.0, 1.0],
            segments: 24,
        },
    });
    model.nodes.push(Node {
        mesh: Some(MeshId::from("msh_box")),
        material: Some(MaterialId::from("mat_gold")),
        translation: [-1.0, 0.0, 0.0],
        ..Node::new(NodeId::from("nod_box"), "box")
    });
    model.nodes.push(Node {
        mesh: Some(MeshId::from("msh_ball")),
        material: Some(MaterialId::from("mat_blue")),
        translation: [1.1, 0.3, 0.0],
        ..Node::new(NodeId::from("nod_ball"), "ball")
    });
    project.add_document(Document::Model(model));

    Workspace::create(root, project).expect("create workspace")
}

/// A project in a fresh temp directory.
pub fn scratch() -> (tempfile::TempDir, PathBuf, Workspace) {
    let tmp = tempfile::tempdir().expect("temp dir");
    let root = tmp.path().join("scratch.dpaint");
    let ws = write_project(&root);
    (tmp, root, ws)
}
