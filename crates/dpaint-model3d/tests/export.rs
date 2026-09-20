//! Export: the GLB must re-parse, and what comes back must be what was authored.

mod common;

use common::*;
use dpaint_core::doc::model::{
    AlphaMode, AnimChannel, AnimKey, AnimPath, Animation, Camera, Interpolation, Light, LightKind,
    Material, Mesh, MeshSource, Node, Primitive, TextureBinding, TextureSlot, TextureSource,
    UpAxis,
};
use dpaint_core::{AssetStore, Color, DocId, MeshId, Project};
use dpaint_model3d::{export, GltfOut};

fn stub_textures(bytes: &'static [u8]) -> impl Fn(&DocId) -> dpaint_core::Result<Vec<u8>> {
    move |_| Ok(bytes.to_vec())
}

/// A scene with two meshes, two materials, a light, a camera and a parent/child pair.
fn authored_scene(assets: &AssetStore) -> (Project, DocId) {
    let (mut project, doc) = model_project();
    let png = assets.put(TINY_PNG, "png").unwrap();
    let raster = DocId::from("doc_tex");
    project.add_document(dpaint_core::doc::Document::Raster(
        dpaint_core::RasterDoc::new(raster.clone(), "tex", 8, 8),
    ));

    let model = project.model_mut(&doc).unwrap();
    model.meshes.push(Mesh {
        id: MeshId::from("msh_box"),
        name: "crate".into(),
        source: MeshSource::Primitive {
            shape: Primitive::Box,
            size: [2.0, 1.0, 1.0],
            segments: 1,
        },
    });
    model.meshes.push(Mesh {
        id: MeshId::from("msh_ball"),
        name: "ball".into(),
        source: MeshSource::Primitive {
            shape: Primitive::Sphere,
            size: [1.0, 1.0, 1.0],
            segments: 12,
        },
    });

    let mut metal = Material::new("mat_metal".into(), "metal");
    metal.base_color = Color::parse("#808080").unwrap();
    metal.metallic = 1.0;
    metal.roughness = 0.25;
    metal.double_sided = true;
    metal.alpha_mode = Some(AlphaMode::Blend);
    metal.emissive = Some(Color::parse("#ff0000").unwrap());
    metal.emissive_strength = 3.0;
    metal.textures.push(TextureBinding {
        slot: TextureSlot::BaseColor,
        source: TextureSource::Asset { asset: png },
        scale: 1.0,
        uv_set: 0,
    });
    model.materials.push(metal);

    let mut painted = Material::new("mat_painted".into(), "painted");
    painted.textures.push(TextureBinding {
        slot: TextureSlot::Normal,
        source: TextureSource::Document { document: raster },
        scale: 0.5,
        uv_set: 0,
    });
    model.materials.push(painted);

    let mut parent = Node::new("nd_rig".into(), "rig");
    parent.translation = [1.0, 2.0, 3.0];
    parent.children = vec!["nd_box".into(), "nd_ball".into()];
    let mut boxn = Node::new("nd_box".into(), "box");
    boxn.mesh = Some(MeshId::from("msh_box"));
    boxn.material = Some("mat_metal".into());
    let mut ball = Node::new("nd_ball".into(), "ball");
    ball.mesh = Some(MeshId::from("msh_ball"));
    ball.material = Some("mat_painted".into());
    ball.translation = [0.0, 4.0, 0.0];
    model.nodes.push(parent);
    model.nodes.push(boxn);
    model.nodes.push(ball);

    model.lights.push(Light {
        id: "lgt_key".into(),
        name: "key".into(),
        kind: LightKind::Spot,
        color: Color::WHITE,
        intensity: 800.0,
        range: Some(20.0),
    });
    let mut light_node = Node::new("nd_key".into(), "key");
    light_node.light = Some("lgt_key".into());
    model.nodes.push(light_node);

    model.cameras.push(Camera {
        id: "cam_hero".into(),
        name: "hero".into(),
        yfov: 0.7,
        znear: 0.05,
        zfar: Some(120.0),
    });
    let mut cam_node = Node::new("nd_hero".into(), "hero");
    cam_node.camera = Some("cam_hero".into());
    model.nodes.push(cam_node);

    (project, doc)
}

fn export_scene(project: &Project, doc: &DocId, assets: &AssetStore) -> GltfOut {
    export(project, doc, assets, &stub_textures(TINY_PNG)).unwrap()
}

#[test]
fn the_exported_glb_reparses_and_matches_what_was_authored() {
    let (_tmp, assets) = store();
    let (project, doc) = authored_scene(&assets);
    let out = export_scene(&project, &doc, &assets);

    // The gltf crate validates on import, so parsing at all is the conformance check.
    let g = gltf::Gltf::from_slice(&out.glb).expect("GLB must parse with zero errors");
    let doc_g = &g.document;
    let blob = g.blob.clone().expect("GLB carries a BIN chunk");

    // Meshes: one glTF mesh per (geometry, material) pair actually drawn.
    let names: Vec<String> = doc_g
        .meshes()
        .map(|m| m.name().unwrap_or_default().to_string())
        .collect();
    assert!(
        names.contains(&"crate".to_string()),
        "meshes were {names:?}"
    );
    assert!(names.contains(&"ball".to_string()), "meshes were {names:?}");

    let crate_mesh = doc_g.meshes().find(|m| m.name() == Some("crate")).unwrap();
    let prim = crate_mesh.primitives().next().unwrap();
    let reader = prim.reader(|_| Some(&blob));
    let positions: Vec<[f32; 3]> = reader.read_positions().unwrap().collect();
    let indices: Vec<u32> = reader.read_indices().unwrap().into_u32().collect();
    assert_eq!(
        positions.len(),
        24,
        "box keeps its 24 corners through export"
    );
    assert_eq!(indices.len(), 36, "12 triangles");
    assert!(reader.read_normals().is_some());
    assert!(reader.read_tex_coords(0).is_some());
    let (lo, hi) = positions
        .iter()
        .fold(([f32::MAX; 3], [f32::MIN; 3]), |(mut lo, mut hi), p| {
            for i in 0..3 {
                lo[i] = lo[i].min(p[i]);
                hi[i] = hi[i].max(p[i]);
            }
            (lo, hi)
        });
    assert_eq!(
        [hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2]],
        [2.0, 1.0, 1.0]
    );

    // Accessor min/max must be present and truthful for POSITION.
    let pos_accessor = prim.get(&gltf::Semantic::Positions).unwrap();
    let min: Vec<f32> = serde_json::from_value(pos_accessor.min().unwrap()).unwrap();
    let max: Vec<f32> = serde_json::from_value(pos_accessor.max().unwrap()).unwrap();
    assert_eq!(min, lo.to_vec());
    assert_eq!(max, hi.to_vec());

    // Materials.
    let metal = doc_g
        .materials()
        .find(|m| m.name() == Some("metal"))
        .expect("metal material survived");
    let pbr = metal.pbr_metallic_roughness();
    assert_eq!(pbr.metallic_factor(), 1.0);
    assert_eq!(pbr.roughness_factor(), 0.25);
    assert!(metal.double_sided());
    assert_eq!(metal.alpha_mode(), gltf::material::AlphaMode::Blend);
    // #808080 is sRGB; glTF factors are linear, so 0.502 must have become ~0.216.
    let base = pbr.base_color_factor();
    assert!(
        (base[0] - 0.2159).abs() < 1e-3,
        "base color should be linear-light, got {base:?}"
    );
    assert!(
        pbr.base_color_texture().is_some(),
        "the PNG binding survived"
    );
    assert!((metal.emissive_strength().unwrap_or(1.0) - 3.0).abs() < 1e-6);

    let painted = doc_g
        .materials()
        .find(|m| m.name() == Some("painted"))
        .unwrap();
    let normal = painted.normal_texture().expect("normal map bound");
    assert!((normal.scale() - 0.5).abs() < 1e-6);

    // A normal-mapped primitive must carry tangents.
    let ball_mesh = doc_g.meshes().find(|m| m.name() == Some("ball")).unwrap();
    let ball_prim = ball_mesh.primitives().next().unwrap();
    assert!(
        ball_prim.get(&gltf::Semantic::Tangents).is_some(),
        "normal-mapped geometry needs TANGENT"
    );

    // Images are embedded in the buffer, not left as dangling URIs.
    assert_eq!(doc_g.images().count(), 2);
    for img in doc_g.images() {
        match img.source() {
            gltf::image::Source::View { mime_type, .. } => assert_eq!(mime_type, "image/png"),
            gltf::image::Source::Uri { .. } => panic!("GLB images must live in the buffer"),
        }
    }

    // Node hierarchy.
    let scene = doc_g.default_scene().expect("a default scene");
    let roots: Vec<String> = scene
        .nodes()
        .map(|n| n.name().unwrap_or_default().to_string())
        .collect();
    assert!(roots.contains(&"rig".to_string()), "roots were {roots:?}");
    let rig = doc_g.nodes().find(|n| n.name() == Some("rig")).unwrap();
    assert_eq!(rig.transform().decomposed().0, [1.0, 2.0, 3.0]);
    let kids: Vec<String> = rig
        .children()
        .map(|n| n.name().unwrap_or_default().to_string())
        .collect();
    assert_eq!(kids, vec!["box".to_string(), "ball".to_string()]);

    // Light and camera.
    let lights: Vec<_> = doc_g.lights().expect("KHR_lights_punctual").collect();
    assert_eq!(lights.len(), 1);
    assert_eq!(lights[0].name(), Some("key"));
    assert!((lights[0].intensity() - 800.0).abs() < 1e-3);
    assert!(matches!(
        lights[0].kind(),
        gltf::khr_lights_punctual::Kind::Spot { .. }
    ));
    let cam = doc_g.cameras().next().expect("a camera");
    match cam.projection() {
        gltf::camera::Projection::Perspective(p) => {
            assert!((p.yfov() - 0.7).abs() < 1e-6);
            assert_eq!(p.zfar(), Some(120.0));
        }
        _ => panic!("expected a perspective camera"),
    }
}

#[test]
fn the_glb_container_is_byte_correct() {
    let (_tmp, assets) = store();
    let (project, doc) = authored_scene(&assets);
    let out = export_scene(&project, &doc, &assets);
    let glb = &out.glb;

    let u32_at = |i: usize| u32::from_le_bytes([glb[i], glb[i + 1], glb[i + 2], glb[i + 3]]);
    assert_eq!(&glb[0..4], b"glTF");
    assert_eq!(u32_at(4), 2, "glTF container version");
    assert_eq!(
        u32_at(8) as usize,
        glb.len(),
        "header length covers the file"
    );
    assert_eq!(glb.len() % 4, 0, "the whole container is 4-byte aligned");

    let json_len = u32_at(12) as usize;
    assert_eq!(u32_at(16), 0x4E4F_534A, "chunk 0 is JSON");
    assert_eq!(json_len % 4, 0, "JSON chunk is padded to 4 bytes");
    let json = &glb[20..20 + json_len];
    let pad = json.len() - json.iter().rposition(|b| *b != b' ').unwrap() - 1;
    assert!(pad < 4, "JSON padding is minimal");
    assert!(
        json[json.len() - pad..].iter().all(|b| *b == b' '),
        "JSON must be padded with spaces"
    );
    serde_json::from_slice::<serde_json::Value>(json).expect("JSON chunk parses");

    let bin_off = 20 + json_len;
    let bin_len = u32_at(bin_off) as usize;
    assert_eq!(u32_at(bin_off + 4), 0x004E_4942, "chunk 1 is BIN");
    assert_eq!(bin_len % 4, 0, "BIN chunk is padded to 4 bytes");
    let bin = &glb[bin_off + 8..bin_off + 8 + bin_len];
    assert_eq!(
        bin_len - out.bin.len(),
        0,
        "the BIN chunk is the exported buffer"
    );
    assert_eq!(bin, out.bin.as_slice());
    assert_eq!(bin_off + 8 + bin_len, glb.len(), "nothing trailing");
}

#[test]
fn the_gltf_json_points_at_a_sidecar_buffer_while_the_glb_does_not() {
    let (_tmp, assets) = store();
    let (project, doc) = authored_scene(&assets);
    let out = export_scene(&project, &doc, &assets);

    let json: serde_json::Value = serde_json::from_str(&out.json).unwrap();
    let buffer = &json["buffers"][0];
    assert_eq!(buffer["uri"], serde_json::json!("doc_scene.bin"));
    assert_eq!(
        buffer["byteLength"].as_u64().unwrap() as usize,
        out.bin.len()
    );
    assert_eq!(GltfOut::buffer_name(&doc), "doc_scene.bin");

    let json_len =
        u32::from_le_bytes([out.glb[12], out.glb[13], out.glb[14], out.glb[15]]) as usize;
    let glb_json: serde_json::Value = serde_json::from_slice(&out.glb[20..20 + json_len]).unwrap();
    assert!(
        glb_json["buffers"][0].get("uri").is_none(),
        "a GLB's buffer must not carry a uri"
    );

    // Every buffer view stays inside the buffer and starts on a 4-byte boundary.
    for view in json["bufferViews"].as_array().unwrap() {
        let off = view["byteOffset"].as_u64().unwrap_or(0) as usize;
        let len = view["byteLength"].as_u64().unwrap() as usize;
        assert_eq!(off % 4, 0, "buffer view offset {off} is not aligned");
        assert!(off + len <= out.bin.len(), "view runs past the buffer");
    }
}

#[test]
fn animation_keys_survive_a_round_trip_with_their_interpolation() {
    let (_tmp, assets) = store();
    let (mut project, doc) = authored_scene(&assets);
    {
        let model = project.model_mut(&doc).unwrap();
        model.animations.push(Animation {
            id: "anm_spin".into(),
            name: "spin".into(),
            channels: vec![
                AnimChannel {
                    node: "nd_box".into(),
                    path: AnimPath::Translation,
                    interpolation: Interpolation::Linear,
                    keys: vec![
                        AnimKey {
                            t: 0.0,
                            v: vec![0.0, 0.0, 0.0],
                        },
                        AnimKey {
                            t: 1.5,
                            v: vec![1.0, 2.0, 3.0],
                        },
                    ],
                },
                AnimChannel {
                    node: "nd_ball".into(),
                    path: AnimPath::Scale,
                    interpolation: Interpolation::Step,
                    keys: vec![
                        AnimKey {
                            t: 0.0,
                            v: vec![1.0, 1.0, 1.0],
                        },
                        AnimKey {
                            t: 2.0,
                            v: vec![2.0, 2.0, 2.0],
                        },
                    ],
                },
                AnimChannel {
                    node: "nd_ball".into(),
                    path: AnimPath::Rotation,
                    interpolation: Interpolation::CubicSpline,
                    keys: vec![
                        AnimKey {
                            t: 0.0,
                            v: vec![
                                0.0, 0.0, 0.0, 0.0, // in-tangent
                                0.0, 0.0, 0.0, 1.0, // value
                                0.0, 0.0, 0.0, 0.0, // out-tangent
                            ],
                        },
                        AnimKey {
                            t: 1.0,
                            v: vec![0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
                        },
                    ],
                },
            ],
        });
    }
    let out = export_scene(&project, &doc, &assets);
    let g = gltf::Gltf::from_slice(&out.glb).expect("GLB parses");
    let blob = g.blob.clone().unwrap();
    let anim = g.document.animations().next().expect("one animation");
    assert_eq!(anim.name(), Some("spin"));

    let mut seen = Vec::new();
    for channel in anim.channels() {
        let node = channel
            .target()
            .node()
            .name()
            .unwrap_or_default()
            .to_string();
        let property = channel.target().property();
        let interpolation = channel.sampler().interpolation();
        let reader = channel.reader(|_| Some(&blob));
        let times: Vec<f32> = reader.read_inputs().unwrap().collect();
        let outputs = reader.read_outputs().unwrap();
        seen.push((node.clone(), property, interpolation, times.clone()));

        match (property, outputs) {
            (
                gltf::animation::Property::Translation,
                gltf::animation::util::ReadOutputs::Translations(t),
            ) => {
                let v: Vec<[f32; 3]> = t.collect();
                assert_eq!(v, vec![[0.0, 0.0, 0.0], [1.0, 2.0, 3.0]]);
                assert_eq!(times, vec![0.0, 1.5]);
                assert_eq!(interpolation, gltf::animation::Interpolation::Linear);
            }
            (gltf::animation::Property::Scale, gltf::animation::util::ReadOutputs::Scales(s)) => {
                let v: Vec<[f32; 3]> = s.collect();
                assert_eq!(v, vec![[1.0, 1.0, 1.0], [2.0, 2.0, 2.0]]);
                assert_eq!(interpolation, gltf::animation::Interpolation::Step);
            }
            (
                gltf::animation::Property::Rotation,
                gltf::animation::util::ReadOutputs::Rotations(r),
            ) => {
                let v: Vec<[f32; 4]> = r.into_f32().collect();
                assert_eq!(v.len(), 6, "cubic spline stores three values per key");
                assert_eq!(v[1], [0.0, 0.0, 0.0, 1.0], "the middle group is the value");
                assert_eq!(interpolation, gltf::animation::Interpolation::CubicSpline);
            }
            (p, _) => panic!("unexpected channel {p:?} on {node}"),
        }
    }
    assert_eq!(seen.len(), 3, "all three tracks made it: {seen:?}");
}

#[test]
fn a_z_up_scene_is_rotated_into_gltf_y_up_on_export() {
    let (_tmp, assets) = store();
    let (mut project, doc) = authored_scene(&assets);
    project.model_mut(&doc).unwrap().up_axis = UpAxis::Z;
    let out = export_scene(&project, &doc, &assets);
    let g = gltf::Gltf::from_slice(&out.glb).unwrap();
    let scene = g.document.default_scene().unwrap();
    let roots: Vec<_> = scene.nodes().collect();
    assert_eq!(roots.len(), 1, "a single correction root");
    assert_eq!(roots[0].name(), Some("z-up-correction"));
    let (_, rot, _) = roots[0].transform().decomposed();
    // -90 degrees about X maps authored +Z onto glTF +Y.
    let z_up = [0.0f32, 0.0, 1.0];
    let mapped = rotate(rot, z_up);
    assert!(
        (mapped[1] - 1.0).abs() < 1e-5 && mapped[0].abs() < 1e-5 && mapped[2].abs() < 1e-5,
        "+Z should map to +Y, got {mapped:?}"
    );
    assert!(roots[0].children().count() >= 3);
}

fn rotate(q: [f32; 4], v: [f32; 3]) -> [f32; 3] {
    let [x, y, z, w] = q;
    let u = [x, y, z];
    let cross = |a: [f32; 3], b: [f32; 3]| {
        [
            a[1] * b[2] - a[2] * b[1],
            a[2] * b[0] - a[0] * b[2],
            a[0] * b[1] - a[1] * b[0],
        ]
    };
    let t = cross(u, v);
    [
        v[0] + 2.0 * (w * t[0] + cross(u, t)[0]),
        v[1] + 2.0 * (w * t[1] + cross(u, t)[1]),
        v[2] + 2.0 * (w * t[2] + cross(u, t)[2]),
    ]
}

#[test]
fn a_texture_resolver_failure_surfaces_instead_of_exporting_a_broken_image() {
    let (_tmp, assets) = store();
    let (project, doc) = authored_scene(&assets);
    let failing = |d: &DocId| -> dpaint_core::Result<Vec<u8>> {
        Err(dpaint_core::Error::Invalid(format!("cannot render {d}")))
    };
    let err = export(&project, &doc, &assets, &failing).unwrap_err();
    assert!(err.to_string().contains("cannot render"), "got {err}");

    // Non-image bytes are rejected rather than written into the buffer as garbage.
    let junk = |_: &DocId| Ok(b"not an image".to_vec());
    let err = export(&project, &doc, &assets, &junk).unwrap_err();
    assert!(
        matches!(err, dpaint_core::Error::UnsupportedFormat(_)),
        "got {err:?}"
    );
}

#[test]
fn exporting_the_same_document_twice_is_byte_identical() {
    let (_tmp, assets) = store();
    let (project, doc) = authored_scene(&assets);
    let a = export_scene(&project, &doc, &assets);
    let b = export_scene(&project, &doc, &assets);
    assert_eq!(a.glb, b.glb, "export must be deterministic");
    assert_eq!(a.json, b.json);
}
