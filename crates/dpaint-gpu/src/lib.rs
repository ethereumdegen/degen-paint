//! GPU viewport renderer for degen-paint.
//!
//! This crate exists for one job the CPU renderer cannot do: drive an interactive viewport.
//! It is **not** authoritative. `dpaint_render::preview3d` still produces every
//! `render.image`, `render.turntable`, golden, digest and diff, because those need
//! byte-identical output on a laptop, in CI and in a container with no display, and a driver
//! cannot promise that.
//!
//! So the split is a policy: CPU for anything an agent compares, GPU for anything a human
//! drags. The two must nonetheless show the same object, which is enforced by a parity test
//! that renders both and requires SSIM >= 0.93 with mean ΔE2000 <= 6.
//!
//! Nothing here is required. [`Gpu::new`] returns `None` when no adapter exists and every
//! caller falls back to the CPU path; a missing GPU costs frames per second, never a feature.

mod camera;
mod canvas;
mod scene;

pub use canvas::CanvasRenderer;
pub use scene::SceneRenderer;

/// Re-exported, not redeclared: a field added to the CPU camera cannot drift away from the
/// GPU camera because there is only one type.
pub use dpaint_render::preview3d::{Camera, Lighting};

use dpaint_core::{AssetStore, Color, DocId, Error, Project, Result};
use tiny_skia::Pixmap;

/// A device handle: instance, adapter, device and queue, created once and shared.
///
/// Cloning is not offered because wgpu's own handles are already reference-counted inside;
/// share a `&Gpu` or wrap it in an `Arc` at the call site.
pub struct Gpu {
    instance: wgpu::Instance,
    adapter: wgpu::Adapter,
    device: wgpu::Device,
    queue: wgpu::Queue,
    info: GpuInfo,
}

/// What adapter we ended up on, in a form that can go into `dpaint doctor` output or a
/// status bar. All strings: the point is to report, not to branch on.
#[derive(
    Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct GpuInfo {
    pub backend: String,
    pub name: String,
    pub device_type: String,
}

impl Gpu {
    /// Acquire a device. `None` means no adapter — not an error, and never a panic.
    pub async fn new() -> Option<Gpu> {
        Self::with_backends(wgpu::Backends::all()).await
    }

    /// Acquire a device restricted to `backends`. Used by the browser path to ask for WebGPU
    /// only, and by the tests to exercise the no-adapter branch with `Backends::empty()`.
    pub async fn with_backends(backends: wgpu::Backends) -> Option<Gpu> {
        let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
        desc.backends = backends;
        let instance = wgpu::Instance::new(desc);
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions::default())
            .await
            .ok()?;
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor::default())
            .await
            .ok()?;
        let a = adapter.get_info();
        let info = GpuInfo {
            backend: a.backend.to_string(),
            name: a.name,
            device_type: format!("{:?}", a.device_type),
        };
        Some(Gpu {
            instance,
            adapter,
            device,
            queue,
            info,
        })
    }

    /// Blocking constructor for native callers that have no executor yet.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn block_new() -> Option<Gpu> {
        pollster::block_on(Self::new())
    }

    pub fn info(&self) -> GpuInfo {
        self.info.clone()
    }

    pub fn device(&self) -> &wgpu::Device {
        &self.device
    }

    pub fn queue(&self) -> &wgpu::Queue {
        &self.queue
    }

    /// Needed by window and canvas hosts to call `create_surface`.
    pub fn instance(&self) -> &wgpu::Instance {
        &self.instance
    }

    /// Needed by surface hosts to call `get_capabilities` and choose a format.
    pub fn adapter(&self) -> &wgpu::Adapter {
        &self.adapter
    }

    /// Whether `format` can be rendered at 4x MSAA here. Callers pass the answer straight to
    /// [`SceneRenderer::new`] instead of guessing; a renderer asked for an unsupported count
    /// silently falls back to 1x and reports it through [`SceneRenderer::samples`].
    pub fn supports_msaa4(&self, format: wgpu::TextureFormat) -> bool {
        self.adapter
            .get_texture_format_features(format)
            .flags
            .sample_count_supported(4)
    }
}

/// One drawable: geometry, where it is, and what it is made of.
#[derive(Debug, Clone)]
pub struct GpuMesh {
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub indices: Vec<u32>,
    /// Column-major world matrix, glTF `m[col][row]`, as produced by
    /// `dpaint_model3d::scene_meshes`.
    pub world: [[f32; 4]; 4],
    pub base_color: Color,
    pub metallic: f32,
    pub roughness: f32,
}

/// What to draw. Built from a document by the caller so the renderer never touches project
/// state and can sit inside a render loop.
#[derive(Debug, Clone)]
pub struct Scene {
    pub meshes: Vec<GpuMesh>,
    pub camera: Camera,
    pub lighting: Lighting,
    pub background: Option<Color>,
    /// Viewport pan, in units of the fitted radius, along camera right and up. Offsets the
    /// orbit *target*; eye and target move together, so this pans rather than orbits.
    ///
    /// It lives here and not on [`Camera`] on purpose: `preview3d` has no use for it and must
    /// not carry a field it never reads. `[0.0, 0.0]` is the framing the CPU path produces,
    /// and is what the parity test asserts.
    pub pan: [f32; 2],
}

impl Default for Scene {
    fn default() -> Self {
        Self {
            meshes: Vec::new(),
            camera: Camera::default(),
            lighting: Lighting::default(),
            background: None,
            pan: [0.0, 0.0],
        }
    }
}

impl Scene {
    /// World-space axis-aligned bounds of everything drawable, or `([0; 3], [0; 3])` when the
    /// scene is empty. Same traversal and same degenerate case as `preview3d::bounds`.
    ///
    /// Use this rather than walking [`GpuMesh::positions`] yourself: it is guaranteed to stay
    /// in lockstep with the fit the renderer actually applies.
    pub fn bounds(&self) -> ([f32; 3], [f32; 3]) {
        let mut lo = [f32::INFINITY; 3];
        let mut hi = [f32::NEG_INFINITY; 3];
        for m in &self.meshes {
            for p in &m.positions {
                let w = camera::xform_point(&m.world, *p);
                for i in 0..3 {
                    lo[i] = lo[i].min(w[i]);
                    hi[i] = hi[i].max(w[i]);
                }
            }
        }
        if lo[0] > hi[0] {
            ([0.0; 3], [0.0; 3])
        } else {
            (lo, hi)
        }
    }

    /// The radius the camera fit uses: the largest half-extent of [`Scene::bounds`], floored
    /// at `1e-4` so a degenerate scene still produces a finite camera.
    pub fn fit_radius(&self) -> f32 {
        let (lo, hi) = self.bounds();
        (0..3)
            .map(|i| (hi[i] - lo[i]) * 0.5)
            .fold(0.0f32, f32::max)
            .max(1e-4)
    }
}

/// Pan, zoom and display flags for the 2D viewport.
///
/// `zoom` is device pixels per texture pixel (1.0 = one texel per physical pixel) and `pan`
/// is the device-pixel offset of the image centre from the viewport centre, +x right and
/// +y down. `[0, 0]` centres the image.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ViewState {
    pub zoom: f32,
    pub pan: [f32; 2],
    /// Draw the 16px transparency checker behind the document, clamped to the document's
    /// own rect: it marks where the *document* is transparent, so a fully transparent
    /// document still reads as a document and not as empty space. A 1px dark edge is drawn
    /// around the rect regardless, which is what keeps the boundary legible.
    pub checker: bool,
    /// Nearest-neighbour sampling instead of linear.
    pub pixelated: bool,
}

impl Default for ViewState {
    fn default() -> Self {
        Self {
            zoom: 1.0,
            pan: [0.0, 0.0],
            checker: true,
            pixelated: false,
        }
    }
}

/// Build a [`Scene`] from a model document.
///
/// Geometry and material defaults come from the same place the CPU path takes them
/// (`dpaint_model3d::scene_meshes`, then the document's materials), so a mesh missing a
/// material renders `#cccccc` at metallic 0.0 and roughness 0.5 on both renderers.
pub fn scene_from_document(
    project: &Project,
    doc: &DocId,
    assets: &AssetStore,
    camera: Camera,
    lighting: Lighting,
) -> Result<Scene> {
    let model = project.model(doc)?;
    let drawables = dpaint_model3d::scene_meshes(project, doc, assets)?;
    let default_base = Color::parse("#cccccc").unwrap_or(Color::WHITE);
    let meshes = drawables
        .into_iter()
        .map(|(_node, data, material, world)| {
            let m = material.and_then(|id| model.material(&id));
            GpuMesh {
                positions: data.positions,
                normals: data.normals,
                indices: data.indices,
                world,
                base_color: m.map(|m| m.base_color).unwrap_or(default_base),
                metallic: m.map(|m| m.metallic).unwrap_or(0.0),
                roughness: m.map(|m| m.roughness).unwrap_or(0.5),
            }
        })
        .collect();
    Ok(Scene {
        meshes,
        camera,
        lighting,
        background: None,
        pan: [0.0, 0.0],
    })
}

/// Render a scene to a texture and read it back.
///
/// Used by the tests and by `--gpu` renders. Uses 4x MSAA when the adapter supports it, so
/// what comes back is what the viewport shows rather than a second, quieter code path.
pub fn render_scene_offscreen(gpu: &Gpu, size: [u32; 2], scene: &Scene) -> Result<Pixmap> {
    let samples = if gpu.supports_msaa4(OFFSCREEN_FORMAT) {
        4
    } else {
        1
    };
    render_scene_offscreen_with_samples(gpu, size, scene, samples)
}

const OFFSCREEN_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// [`render_scene_offscreen`] with an explicit sample count, so a caller (or a test measuring
/// what MSAA costs in parity) can pin it.
pub fn render_scene_offscreen_with_samples(
    gpu: &Gpu,
    size: [u32; 2],
    scene: &Scene,
    samples: u32,
) -> Result<Pixmap> {
    let (w, h) = (size[0].max(1), size[1].max(1));
    let limit = gpu.device().limits().max_texture_dimension_2d;
    if w > limit || h > limit {
        return Err(Error::Invalid(format!(
            "offscreen size {w}x{h} exceeds the adapter's max texture dimension {limit}"
        )));
    }

    let target = gpu.device().create_texture(&wgpu::TextureDescriptor {
        label: Some("dpaint-gpu offscreen"),
        size: wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: OFFSCREEN_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = target.create_view(&wgpu::TextureViewDescriptor::default());

    let mut renderer = SceneRenderer::new(gpu, OFFSCREEN_FORMAT, samples);
    renderer.draw(gpu, &view, [w, h], scene);

    read_texture(gpu, &target, [w, h])
}

/// Copy a texture into a `Pixmap`, unpadding wgpu's 256-byte row alignment and premultiplying
/// to match tiny-skia's storage.
///
/// Public because a viewport host wants it too: this is how you screenshot what the window
/// just drew without going back through the CPU renderer. The texture must have been created
/// with `COPY_SRC` and an `Rgba8Unorm`-shaped format.
pub fn read_texture(gpu: &Gpu, texture: &wgpu::Texture, size: [u32; 2]) -> Result<Pixmap> {
    let (w, h) = (size[0], size[1]);
    let unpadded = w * 4;
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let padded = unpadded.div_ceil(align) * align;

    let buffer = gpu.device().create_buffer(&wgpu::BufferDescriptor {
        label: Some("dpaint-gpu readback"),
        size: (padded as u64) * (h as u64),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });

    let mut enc = gpu
        .device()
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("dpaint-gpu readback"),
        });
    enc.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded),
                rows_per_image: Some(h),
            },
        },
        wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
    );
    gpu.queue().submit(Some(enc.finish()));

    let slice = buffer.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    gpu.device()
        .poll(wgpu::PollType::wait_indefinitely())
        .map_err(|e| Error::Invalid(format!("gpu readback never completed: {e}")))?;

    let mut pm =
        Pixmap::new(w, h).ok_or_else(|| Error::Invalid("readback size must be positive".into()))?;
    {
        let mapped = slice
            .get_mapped_range()
            .map_err(|e| Error::Invalid(format!("gpu readback range unavailable: {e}")))?;
        let pixels = pm.data_mut();
        for y in 0..h as usize {
            let row = &mapped[y * padded as usize..y * padded as usize + unpadded as usize];
            let out = &mut pixels[y * unpadded as usize..(y + 1) * unpadded as usize];
            // The shader writes straight alpha; tiny-skia stores premultiplied. Opaque
            // pixels — geometry, and every pixel when the background is opaque — are the
            // same in both layouts, so they copy; only antialiased silhouette edges and a
            // translucent background pay for the multiply.
            for (p, o) in row.chunks_exact(4).zip(out.chunks_exact_mut(4)) {
                match p[3] {
                    255 => o.copy_from_slice(p),
                    0 => o.fill(0),
                    a => {
                        let a = a as u32;
                        for i in 0..3 {
                            o[i] = ((p[i] as u32 * a + 127) / 255) as u8;
                        }
                        o[3] = p[3];
                    }
                }
            }
        }
    }
    buffer.unmap();
    Ok(pm)
}
