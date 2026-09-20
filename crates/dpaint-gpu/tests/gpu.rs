//! Behavior of the GPU viewport, including the parity contract with the CPU renderer.
//!
//! Every test that needs an adapter skips when there is none: a machine without a GPU is a
//! supported configuration, so a missing adapter is a skip, never a failure.

use dpaint_core::{Color, MaterialId, MeshId, Project};
use dpaint_gpu::{
    render_scene_offscreen, render_scene_offscreen_with_samples, Camera, CanvasRenderer, Gpu,
    GpuMesh, Lighting, Scene, SceneRenderer, ViewState,
};
use dpaint_render::preview3d;
use tiny_skia::Pixmap;

const TARGET_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// Acquire a device or explain why the test is being skipped.
macro_rules! gpu {
    () => {
        match Gpu::block_new() {
            Some(g) => g,
            None => {
                eprintln!("skipping: no GPU adapter on this machine");
                return;
            }
        }
    };
}

fn box_mesh(size: [f32; 3]) -> dpaint_model3d::MeshData {
    dpaint_model3d::prim::primitive(dpaint_core::doc::model::Primitive::Box, size, 1)
        .expect("box primitive")
}

fn sphere_mesh(size: [f32; 3]) -> dpaint_model3d::MeshData {
    dpaint_model3d::prim::primitive(dpaint_core::doc::model::Primitive::Sphere, size, 24)
        .expect("sphere primitive")
}

/// A one-sided surface: the only geometry where it matters that both renderers light back
/// faces. Its normals all point +Y.
fn plane_mesh(size: [f32; 3]) -> dpaint_model3d::MeshData {
    dpaint_model3d::prim::primitive(dpaint_core::doc::model::Primitive::Plane, size, 4)
        .expect("plane primitive")
}

fn translation(t: [f32; 3]) -> [[f32; 4]; 4] {
    [
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [t[0], t[1], t[2], 1.0],
    ]
}

fn mesh(
    data: dpaint_model3d::MeshData,
    world: [[f32; 4]; 4],
    base: &str,
    metallic: f32,
    roughness: f32,
) -> GpuMesh {
    GpuMesh {
        positions: data.positions,
        normals: data.normals,
        indices: data.indices,
        world,
        base_color: Color::parse(base).expect("color"),
        metallic,
        roughness,
    }
}

/// The same geometry expressed for the CPU rasterizer, so parity compares renderers and not
/// two different scenes.
fn cpu_meshes(scene: &Scene) -> Vec<preview3d::Mesh> {
    scene
        .meshes
        .iter()
        .map(|m| preview3d::Mesh {
            positions: m.positions.clone(),
            normals: m.normals.clone(),
            indices: m.indices.clone(),
            world: m.world,
            base_color: m.base_color,
            metallic: m.metallic,
            roughness: m.roughness,
        })
        .collect()
}

fn to_image(pm: &Pixmap) -> image::RgbaImage {
    let mut img = image::RgbaImage::new(pm.width(), pm.height());
    for (i, px) in pm.pixels().iter().enumerate() {
        let c = px.demultiply();
        let (x, y) = (i as u32 % pm.width(), i as u32 / pm.width());
        img.put_pixel(x, y, image::Rgba([c.red(), c.green(), c.blue(), c.alpha()]));
    }
    img
}

/// Pixels the subject actually covers. Half-covered MSAA edge samples are excluded so a
/// silhouette-area comparison is not measuring antialiasing.
fn covered(pm: &Pixmap) -> Vec<(u32, u32)> {
    pm.pixels()
        .iter()
        .enumerate()
        .filter(|(_, p)| p.alpha() > 127)
        .map(|(i, _)| (i as u32 % pm.width(), i as u32 / pm.width()))
        .collect()
}

fn centroid(pm: &Pixmap) -> Option<(f32, f32)> {
    let px = covered(pm);
    if px.is_empty() {
        return None;
    }
    let n = px.len() as f32;
    Some((
        px.iter().map(|p| p.0 as f32).sum::<f32>() / n,
        px.iter().map(|p| p.1 as f32).sum::<f32>() / n,
    ))
}

/// Mean Rec.709 luma over the covered pixels, for judging how brightly a surface is lit.
fn mean_luma(pm: &Pixmap) -> f32 {
    let px = covered(pm);
    if px.is_empty() {
        return 0.0;
    }
    px.iter()
        .map(|(x, y)| {
            let p = pixel(pm, *x, *y);
            0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32
        })
        .sum::<f32>()
        / px.len() as f32
}

fn pixel(pm: &Pixmap, x: u32, y: u32) -> [u8; 4] {
    let p = pm.pixels()[(y * pm.width() + x) as usize].demultiply();
    [p.red(), p.green(), p.blue(), p.alpha()]
}

/// The parity subject: a box and a sphere, two world matrices, a rough dielectric and a
/// smooth metal.
///
/// The base colors are deliberately dark and the camera is pulled in. Both choices are about
/// making the test able to fail: a bright base color under this rig lands near the lit value,
/// so an unlit shader would produce almost the right image, and a subject floating in an
/// empty frame lets identical background pixels average a real disagreement away.
fn two_material_scene() -> Scene {
    Scene {
        meshes: vec![
            mesh(
                box_mesh([1.6, 1.6, 1.6]),
                translation([-0.9, 0.0, 0.0]),
                "#5a1f12",
                0.1,
                0.35,
            ),
            mesh(
                sphere_mesh([1.5, 1.5, 1.5]),
                translation([1.1, 0.2, 0.3]),
                "#18304f",
                1.0,
                0.15,
            ),
        ],
        camera: Camera {
            zoom: 0.62,
            ..Camera::default()
        },
        lighting: Lighting::default(),
        background: None,
        pan: [0.0, 0.0],
    }
}

/// Crop to the region the subject occupies, so the metric measures the rendered object
/// rather than the two renderers agreeing about empty space.
fn crop_to_subject(a: &Pixmap, b: &Pixmap) -> (image::RgbaImage, image::RgbaImage) {
    let px = covered(a);
    assert!(!px.is_empty(), "the reference render is empty");
    let (x0, y0) = (
        px.iter().map(|p| p.0).min().expect("min x"),
        px.iter().map(|p| p.1).min().expect("min y"),
    );
    let (x1, y1) = (
        px.iter().map(|p| p.0).max().expect("max x"),
        px.iter().map(|p| p.1).max().expect("max y"),
    );
    let (w, h) = (x1 - x0 + 1, y1 - y0 + 1);
    let crop = |pm: &Pixmap| image::imageops::crop_imm(&to_image(pm), x0, y0, w, h).to_image();
    (crop(a), crop(b))
}

/// The contract in `docs/gpu-viewport.md`: a GPU frame and a CPU frame of the same scene are
/// not bit-identical, but they must be recognizably the same image — SSIM >= 0.93 with mean
/// ΔE2000 <= 6, over the whole frame and over the subject alone.
#[test]
fn gpu_and_cpu_renders_of_the_same_scene_agree() {
    let gpu = gpu!();
    let scene = two_material_scene();

    // 300x200 is deliberately not a multiple of 64 pixels: its rows need padding on the way
    // back from the GPU, so a mishandled `bytes_per_row` shears the image and this fails.
    for (w, h) in [(256u32, 256u32), (300, 200)] {
        let cpu = preview3d::render(
            &cpu_meshes(&scene),
            w,
            h,
            scene.camera,
            scene.lighting,
            scene.background,
        )
        .expect("cpu render");
        let gpu_pm = render_scene_offscreen(&gpu, [w, h], &scene).expect("gpu render");

        let full = dpaint_inspect::diff::compare(&to_image(&cpu), &to_image(&gpu_pm))
            .expect("full-frame diff");
        let (ca, cb) = crop_to_subject(&cpu, &gpu_pm);
        let subject = dpaint_inspect::diff::compare(&ca, &cb).expect("subject diff");
        eprintln!(
            "parity {w}x{h}: frame ssim={:.4} mean_dE={:.3} max_dE={:.2} | subject ssim={:.4} mean_dE={:.3} max_dE={:.2}",
            full.ssim,
            full.mean_delta_e,
            full.max_delta_e,
            subject.ssim,
            subject.mean_delta_e,
            subject.max_delta_e
        );
        for (label, d) in [("frame", &full), ("subject", &subject)] {
            assert!(d.ssim >= 0.93, "{label} ssim {:.4} at {w}x{h}", d.ssim);
            assert!(
                d.mean_delta_e <= 6.0,
                "{label} mean dE2000 {:.3} at {w}x{h}",
                d.mean_delta_e
            );
        }
    }
}

#[test]
fn a_cube_renders_shaded_pixels_centred_in_frame() {
    let gpu = gpu!();
    let scene = Scene {
        meshes: vec![mesh(
            box_mesh([1.0, 1.0, 1.0]),
            translation([0.0; 3]),
            "#c0c0c0",
            0.0,
            0.5,
        )],
        ..Scene::default()
    };
    let pm = render_scene_offscreen(&gpu, [256, 256], &scene).expect("render");

    let px = covered(&pm);
    let fraction = px.len() as f32 / (256.0 * 256.0);
    assert!(
        (0.05..0.85).contains(&fraction),
        "cube covers {fraction:.3} of the frame"
    );

    let (cx, cy) = centroid(&pm).expect("cube is visible");
    assert!(
        (cx - 128.0).abs() < 13.0 && (cy - 128.0).abs() < 13.0,
        "cube centroid at ({cx:.1}, {cy:.1}) is not centred"
    );

    // Shaded, not flat-filled: the three visible faces of a cube face the key light at
    // different angles, so an unlit or constant-colour shader collapses this to one value.
    let mut lumas: Vec<u8> = px
        .iter()
        .map(|(x, y)| {
            let p = pixel(&pm, *x, *y);
            ((p[0] as u32 * 54 + p[1] as u32 * 183 + p[2] as u32 * 19) / 256) as u8
        })
        .collect();
    lumas.sort_unstable();
    lumas.dedup();
    assert!(
        lumas.len() >= 3,
        "only {} distinct luminances: the cube is not being shaded",
        lumas.len()
    );
}

#[test]
fn an_empty_scene_yields_only_background() {
    let gpu = gpu!();
    let bg = Color::parse("#123456").expect("color");
    let scene = Scene {
        background: Some(bg),
        ..Scene::default()
    };
    let pm = render_scene_offscreen(&gpu, [64, 48], &scene).expect("render");

    let want = bg.to_rgba8();
    for y in 0..pm.height() {
        for x in 0..pm.width() {
            assert_eq!(pixel(&pm, x, y), want, "pixel ({x}, {y}) is not background");
        }
    }
}

#[test]
fn nearer_geometry_occludes_farther_geometry() {
    let gpu = gpu!();
    // Camera looks down -Z from +Z, so larger Z is nearer. The far mesh is drawn *second*,
    // so without a working depth buffer it would paint over the near one.
    let near_at = |z: f32, color: &str| {
        mesh(
            box_mesh([1.0, 1.0, 1.0]),
            translation([0.0, 0.0, z]),
            color,
            0.0,
            0.9,
        )
    };
    let camera = Camera {
        yaw: 0.0,
        pitch: 0.0,
        ..Camera::default()
    };

    let red_in_front = Scene {
        meshes: vec![near_at(1.5, "#ff0000"), near_at(-1.5, "#0000ff")],
        camera,
        ..Scene::default()
    };
    let pm = render_scene_offscreen(&gpu, [128, 128], &red_in_front).expect("render");
    let p = pixel(&pm, 64, 64);
    assert!(
        p[0] > p[2] + 40,
        "front cube is red but the centre pixel is {p:?}"
    );

    // Swap only the positions, keeping the draw order: the answer must follow depth, not
    // submission order.
    let blue_in_front = Scene {
        meshes: vec![near_at(-1.5, "#ff0000"), near_at(1.5, "#0000ff")],
        camera,
        ..Scene::default()
    };
    let pm = render_scene_offscreen(&gpu, [128, 128], &blue_in_front).expect("render");
    let p = pixel(&pm, 64, 64);
    assert!(
        p[2] > p[0] + 40,
        "front cube is blue but the centre pixel is {p:?}"
    );
}

#[test]
fn a_quarter_turn_changes_the_image_but_not_a_cubes_silhouette() {
    let gpu = gpu!();
    let make = |yaw: f32| Scene {
        meshes: vec![mesh(
            box_mesh([1.0, 1.0, 1.0]),
            translation([0.0; 3]),
            "#b0b0b0",
            0.0,
            0.6,
        )],
        camera: Camera {
            yaw,
            ..Camera::default()
        },
        ..Scene::default()
    };

    let a = render_scene_offscreen(&gpu, [256, 256], &make(0.0)).expect("render");
    let b = render_scene_offscreen(&gpu, [256, 256], &make(90.0)).expect("render");

    // A cube is symmetric under a 90 degree yaw, so its silhouette is the same area...
    let (na, nb) = (covered(&a).len() as f32, covered(&b).len() as f32);
    let drift = (na - nb).abs() / na.max(1.0);
    assert!(
        drift < 0.01,
        "silhouette area moved {:.2}% ({na} -> {nb}) across a quarter turn",
        drift * 100.0
    );

    // ...but the lights do not turn with the camera, so the shading must change.
    let d = dpaint_inspect::diff::compare(&to_image(&a), &to_image(&b)).expect("diff");
    assert!(
        d.changed_fraction > 0.05,
        "a quarter turn changed only {:.4} of the image",
        d.changed_fraction
    );
}

#[test]
fn panning_moves_the_subject_without_touching_the_camera_type() {
    let gpu = gpu!();
    let scene = |pan: [f32; 2]| Scene {
        pan,
        ..two_material_scene()
    };

    let centred = render_scene_offscreen(&gpu, [256, 256], &scene([0.0, 0.0])).expect("render");
    let panned = render_scene_offscreen(&gpu, [256, 256], &scene([0.5, 0.0])).expect("render");

    let (cx, _) = centroid(&centred).expect("subject visible");
    let (px, _) = centroid(&panned).expect("subject visible");
    // Positive pan moves the camera target right, so the subject moves left on screen.
    assert!(
        px < cx - 10.0,
        "pan [0.5, 0] moved the subject from x={cx:.1} to x={px:.1}"
    );
}

#[test]
fn orbiting_a_static_scene_does_not_re_upload_geometry() {
    let gpu = gpu!();
    let mut scene = two_material_scene();
    let texture = gpu.device().create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d {
            width: 128,
            height: 128,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: TARGET_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());

    let mut renderer = SceneRenderer::new(&gpu, TARGET_FORMAT, 1);
    renderer.draw(&gpu, &view, [128, 128], &scene);
    let after_first = renderer.geometry_uploads();
    assert_eq!(after_first, 1, "the first draw must upload the geometry");

    let first = dpaint_gpu::read_texture(&gpu, &texture, [128, 128]).expect("readback");
    for step in 1..=8 {
        scene.camera.yaw = 35.0 + step as f32 * 7.0;
        renderer.draw(&gpu, &view, [128, 128], &scene);
    }
    let last = dpaint_gpu::read_texture(&gpu, &texture, [128, 128]).expect("readback");

    assert_eq!(
        renderer.geometry_uploads(),
        after_first,
        "orbiting re-uploaded geometry"
    );
    let d = dpaint_inspect::diff::compare(&to_image(&first), &to_image(&last)).expect("diff");
    assert!(
        d.changed_fraction > 0.05,
        "the orbit did not change the image, so the camera uniform is not being rebuilt"
    );
}

/// Pan and zoom in the 2D viewport are uniform state. Dragging must not re-upload the
/// document, which is the whole reason this renderer exists.
#[test]
fn canvas_pan_and_zoom_change_the_image_without_re_uploading() {
    let gpu = gpu!();

    let mut doc = Pixmap::new(64, 64).expect("pixmap");
    for y in 0..64u32 {
        for x in 0..64u32 {
            let v = ((x * 4) % 256) as u8;
            doc.pixels_mut()[(y * 64 + x) as usize] =
                tiny_skia::PremultipliedColorU8::from_rgba(v, 255 - v, (y * 4) as u8, 255)
                    .expect("opaque pixel");
        }
    }

    let texture = gpu.device().create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d {
            width: 128,
            height: 128,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: TARGET_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());

    let mut renderer = CanvasRenderer::new(&gpu, TARGET_FORMAT, 1);
    renderer.upload(&gpu, &doc);
    assert_eq!(renderer.uploads(), 1);
    assert_eq!(renderer.image_size(), [64, 64]);

    let shot = |r: &mut CanvasRenderer, vs: ViewState| {
        r.draw(&gpu, &view, [128, 128], vs);
        dpaint_gpu::read_texture(&gpu, &texture, [128, 128]).expect("readback")
    };

    let base = ViewState {
        zoom: 1.0,
        pan: [0.0, 0.0],
        checker: false,
        pixelated: true,
    };
    let centred = shot(&mut renderer, base);
    let panned = shot(
        &mut renderer,
        ViewState {
            pan: [24.0, 0.0],
            ..base
        },
    );
    let zoomed = shot(&mut renderer, ViewState { zoom: 1.8, ..base });

    assert_eq!(
        renderer.uploads(),
        1,
        "pan and zoom re-uploaded the document"
    );

    // Pan is the offset of the image centre from the viewport centre, +x right.
    let (cx, _) = centroid(&centred).expect("image visible");
    let (px, _) = centroid(&panned).expect("image visible");
    assert!(
        (px - (cx + 24.0)).abs() < 1.5,
        "pan [24, 0] moved the image centre from {cx:.1} to {px:.1}"
    );

    // Zoom is device pixels per texel: 64 texels at 1.0 cover 64px, at 1.8 cover ~115.
    assert!(
        (covered(&centred).len() as i64 - 64 * 64).abs() <= 128,
        "at zoom 1.0 a 64x64 document should cover 64x64 pixels, covered {}",
        covered(&centred).len()
    );
    let zoomed_area = covered(&zoomed).len() as f32;
    let expected = (64.0 * 1.8) * (64.0 * 1.8);
    assert!(
        (zoomed_area - expected).abs() / expected < 0.05,
        "at zoom 1.8 expected ~{expected:.0} covered pixels, got {zoomed_area}"
    );
}

#[test]
fn the_canvas_checker_is_drawn_only_where_asked() {
    let gpu = gpu!();
    let mut doc = Pixmap::new(8, 8).expect("pixmap");
    doc.fill(tiny_skia::Color::from_rgba8(255, 0, 0, 255));

    let texture = gpu.device().create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d {
            width: 64,
            height: 64,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: TARGET_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let mut renderer = CanvasRenderer::new(&gpu, TARGET_FORMAT, 1);
    renderer.upload(&gpu, &doc);

    renderer.draw(
        &gpu,
        &view,
        [64, 64],
        ViewState {
            checker: false,
            ..ViewState::default()
        },
    );
    let plain = dpaint_gpu::read_texture(&gpu, &texture, [64, 64]).expect("readback");
    assert_eq!(
        pixel(&plain, 2, 2),
        [0, 0, 0, 0],
        "without the checker, outside the image must stay transparent"
    );

    renderer.draw(&gpu, &view, [64, 64], ViewState::default());
    let checked = dpaint_gpu::read_texture(&gpu, &texture, [64, 64]).expect("readback");
    // 16px squares anchored at the viewport origin: (2,2) is light, (20,2) is dark.
    assert_eq!(pixel(&checked, 2, 2), [207, 212, 218, 255]);
    assert_eq!(pixel(&checked, 20, 2), [154, 162, 172, 255]);
    assert_eq!(pixel(&checked, 20, 20), [207, 212, 218, 255]);
}

#[test]
fn scene_from_document_maps_geometry_and_materials() {
    use dpaint_core::doc::model::{Material, Mesh, MeshSource, Node, Primitive};

    let tmp = tempfile::tempdir().expect("temp dir");
    let assets = dpaint_core::AssetStore::new(tmp.path());
    let doc = dpaint_core::DocId::from("doc_scene");
    let mut project = Project::new(
        "test",
        dpaint_core::doc::Document::Model(dpaint_core::ModelDoc::new(doc.clone(), "scene")),
    );
    {
        let model = project.model_mut(&doc).expect("model");
        model.meshes.push(Mesh {
            id: MeshId::from("msh_b"),
            name: "b".into(),
            source: MeshSource::Primitive {
                shape: Primitive::Box,
                size: [2.0, 2.0, 2.0],
                segments: 1,
            },
        });
        model.materials.push(Material {
            id: MaterialId::from("mat_gold"),
            name: "gold".into(),
            base_color: Color::parse("#ffd700").expect("color"),
            metallic: 0.95,
            roughness: 0.15,
            emissive: None,
            emissive_strength: 0.0,
            double_sided: false,
            alpha_mode: None,
            textures: Vec::new(),
        });
        let mut node = Node::new("nd_a".into(), "a");
        node.translation = [3.0, 0.0, 0.0];
        node.mesh = Some(MeshId::from("msh_b"));
        node.material = Some(MaterialId::from("mat_gold"));
        model.nodes.push(node);
    }

    let scene = dpaint_gpu::scene_from_document(
        &project,
        &doc,
        &assets,
        Camera::default(),
        Lighting::default(),
    )
    .expect("scene");

    assert_eq!(scene.meshes.len(), 1);
    let m = &scene.meshes[0];
    assert_eq!(m.indices.len(), 36, "a box is 12 triangles");
    assert_eq!(m.base_color.to_rgba8(), [255, 215, 0, 255]);
    assert_eq!(m.metallic, 0.95);
    assert_eq!(m.roughness, 0.15);
    // Column-major: the node's translation lives in the last column.
    assert_eq!(
        [m.world[3][0], m.world[3][1], m.world[3][2]],
        [3.0, 0.0, 0.0]
    );

    // The world matrix has to be applied, not ignored: bounds are the node's box, not the
    // mesh's local one.
    let (lo, hi) = scene.bounds();
    assert!(
        (lo[0] - 2.0).abs() < 1e-4 && (hi[0] - 4.0).abs() < 1e-4,
        "x bounds {lo:?}..{hi:?}"
    );
    assert!((scene.fit_radius() - 1.0).abs() < 1e-4);
    assert_eq!(scene.pan, [0.0, 0.0]);
}

#[test]
fn an_empty_scenes_bounds_and_fit_are_finite() {
    let scene = Scene::default();
    assert_eq!(scene.bounds(), ([0.0; 3], [0.0; 3]));
    assert!(scene.fit_radius() > 0.0 && scene.fit_radius().is_finite());
}

#[test]
fn a_gpu_reports_a_plausible_adapter_and_no_backend_means_no_gpu() {
    // The documented no-adapter path: an instance with no backends cannot produce one, and
    // that is an `Option`, not an error and not a panic.
    assert!(
        pollster::block_on(Gpu::with_backends(wgpu::Backends::empty())).is_none(),
        "an instance with no backends must report no GPU"
    );

    let gpu = gpu!();
    let info = gpu.info();
    eprintln!(
        "adapter: backend={} name={} type={}",
        info.backend, info.name, info.device_type
    );
    assert!(
        ["vulkan", "metal", "dx12", "gl", "webgpu"].contains(&info.backend.as_str()),
        "implausible backend {:?}",
        info.backend
    );
    assert!(!info.name.is_empty(), "adapter name is empty");
    assert!(
        ["Other", "IntegratedGpu", "DiscreteGpu", "VirtualGpu", "Cpu"]
            .contains(&info.device_type.as_str()),
        "implausible device type {:?}",
        info.device_type
    );

    // It goes into `doctor` output and a status bar, so it has to survive serialization.
    let json = serde_json::to_string(&info).expect("serialize");
    let back: dpaint_gpu::GpuInfo = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(back, info);
}

#[test]
fn msaa_falls_back_rather_than_failing_when_unsupported() {
    let gpu = gpu!();
    let requested = SceneRenderer::new(&gpu, TARGET_FORMAT, 4).samples();
    let supported = gpu.supports_msaa4(TARGET_FORMAT);
    eprintln!("msaa4 supported={supported} renderer samples={requested}");
    assert_eq!(
        requested,
        if supported { 4 } else { 1 },
        "the renderer must use 4x when available and 1x otherwise"
    );
    // An absurd count is a request, not a contract: it degrades instead of panicking.
    assert_eq!(SceneRenderer::new(&gpu, TARGET_FORMAT, 7).samples(), 1);
    assert_eq!(SceneRenderer::new(&gpu, TARGET_FORMAT, 1).samples(), 1);
}

/// Not an assertion of speed — a record of it. The viewport budget is 16ms per frame and a
/// readback is the one thing in this crate that stalls the pipeline, so its cost is worth
/// printing next to the parity numbers.
#[test]
fn offscreen_readback_is_timed_at_both_sizes() {
    let gpu = gpu!();
    let scene = two_material_scene();
    for size in [512u32, 2048] {
        // Warm the pipeline so the first-run shader compile is not counted.
        let _ = render_scene_offscreen(&gpu, [64, 64], &scene).expect("warmup");
        let t = std::time::Instant::now();
        let pm = render_scene_offscreen(&gpu, [size, size], &scene).expect("render");
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        eprintln!("offscreen {size}x{size}: {ms:.1}ms");
        assert_eq!((pm.width(), pm.height()), (size, size));
        assert!(
            !covered(&pm).is_empty(),
            "nothing rendered at {size}x{size}"
        );
    }
}

/// MSAA is the one place the GPU is allowed to look *better* than the CPU, and antialiased
/// silhouette edges are the only reason the parity threshold has to be loose at all.
///
/// With multisampling off, both renderers rasterize the same triangles with the same pixel
/// centres through the same matrices, so the bar is far higher than the published contract:
/// what remains is perspective-correct vs affine normal interpolation and one ulp of depth.
#[test]
fn parity_holds_with_and_without_multisampling() {
    let gpu = gpu!();
    let scene = two_material_scene();
    let cpu = preview3d::render(
        &cpu_meshes(&scene),
        256,
        256,
        scene.camera,
        scene.lighting,
        scene.background,
    )
    .expect("cpu render");

    for samples in [1u32, 4] {
        let pm = render_scene_offscreen_with_samples(&gpu, [256, 256], &scene, samples)
            .expect("gpu render");
        let (ca, cb) = crop_to_subject(&cpu, &pm);
        let d = dpaint_inspect::diff::compare(&ca, &cb).expect("diff");
        eprintln!(
            "parity samples={samples}: subject ssim={:.4} mean_dE={:.3} max_dE={:.2}",
            d.ssim, d.mean_delta_e, d.max_delta_e
        );
        let (min_ssim, max_de) = if samples == 1 {
            (0.99, 0.5)
        } else {
            (0.93, 6.0)
        };
        assert!(
            d.ssim >= min_ssim,
            "ssim {:.4} at {samples}x, wanted >= {min_ssim}",
            d.ssim
        );
        assert!(
            d.mean_delta_e <= max_de,
            "mean dE2000 {:.3} at {samples}x, wanted <= {max_de}",
            d.mean_delta_e
        );
    }
}

/// `preview3d` has no backface culling: it draws every triangle and flips the interpolated
/// normal toward the camera, so a one-sided surface seen from behind is lit as a surface, not
/// as a hole. A GPU shader that skipped that flip would light the back of this plane from the
/// key light instead of leaving it on ambient, and the closed solids in the parity scene
/// could never notice because their back faces are always occluded.
#[test]
fn a_surface_seen_from_behind_is_lit_the_way_the_cpu_lights_it() {
    let gpu = gpu!();
    let view_from = |pitch: f32| Scene {
        meshes: vec![mesh(
            plane_mesh([3.0, 1.0, 3.0]),
            translation([0.0; 3]),
            "#b4b4b4",
            0.0,
            0.6,
        )],
        camera: Camera {
            yaw: 0.0,
            pitch,
            ..Camera::default()
        },
        ..Scene::default()
    };

    let mut lumas = Vec::new();
    for pitch in [45.0f32, -45.0] {
        let scene = view_from(pitch);
        let cpu = preview3d::render(
            &cpu_meshes(&scene),
            192,
            192,
            scene.camera,
            scene.lighting,
            scene.background,
        )
        .expect("cpu render");
        let pm =
            render_scene_offscreen_with_samples(&gpu, [192, 192], &scene, 1).expect("gpu render");
        let (ca, cb) = crop_to_subject(&cpu, &pm);
        let d = dpaint_inspect::diff::compare(&ca, &cb).expect("diff");
        eprintln!(
            "plane pitch={pitch}: ssim={:.4} mean_dE={:.3} cpu_luma={:.1} gpu_luma={:.1}",
            d.ssim,
            d.mean_delta_e,
            mean_luma(&cpu),
            mean_luma(&pm)
        );
        assert!(d.ssim >= 0.99, "ssim {:.4} at pitch {pitch}", d.ssim);
        assert!(
            d.mean_delta_e <= 0.5,
            "mean dE2000 {:.3} at pitch {pitch}",
            d.mean_delta_e
        );
        lumas.push(mean_luma(&pm));
    }

    // The lights do not flip with the normal, so the far side is ambient-only and much
    // darker. Without that, this test could pass on a shader that ignored the normal.
    let (front, back) = (lumas[0], lumas[1]);
    assert!(
        back < front * 0.6,
        "the back of the plane ({back:.1}) is not markedly darker than the front ({front:.1})"
    );
}
