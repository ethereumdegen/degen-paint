# GPU viewport build contract (internal, for parallel implementation)

Read `docs/gpu-viewport.md` first — it holds the policy. This file holds the signatures.

`wgpu 30.0.1` is verified working on this machine: `backend=Metal name=Apple A18 Pro
type=IntegratedGpu`, `max_texture_dimension_2d=8192`. Note the 30.x API shape, which moved
since older examples:

```rust
let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle()); // by value, no ::default()
let adapter = instance.request_adapter(&wgpu::RequestAdapterOptions::default()).await?;
let (device, queue) = adapter.request_device(&wgpu::DeviceDescriptor::default()).await?;
```

## Disk

The workspace target directory reached 32 GB and filled the volume. `cargo clean` was run;
there is ~32 GB free now. Do not build the Tauri app or the wasm target unless your task
needs it, and do not leave stray probe crates behind.

## `crates/dpaint-gpu` — the API everyone else codes against

```rust
/// Device handle. `new()` returns None when no adapter exists; every caller degrades to
/// the CPU renderer rather than failing.
pub struct Gpu { /* instance, adapter, device, queue, info */ }
impl Gpu {
    pub async fn new() -> Option<Gpu>;
    pub fn block_new() -> Option<Gpu>;           // pollster, for native callers
    pub fn info(&self) -> GpuInfo;               // { backend, name, device_type } — all strings, serializable
    pub fn device(&self) -> &wgpu::Device;
    pub fn queue(&self) -> &wgpu::Queue;
}

/// What to draw. Built from a document by the caller, so the renderer never touches
/// project state and can live in a render loop.
pub struct Scene {
    pub meshes: Vec<GpuMesh>,                    // positions/normals/indices + world matrix + material
    pub camera: Camera,                          // same fields as dpaint_render::preview3d::Camera
    pub lighting: Lighting,                      // same fields as dpaint_render::preview3d::Lighting
    pub background: Option<dpaint_core::Color>,
    /// Camera target offset along camera-right/up, in units of the fitted radius. Eye and
    /// target move together, so `[0.0, 0.0]` is exactly the CPU path's framing — which is
    /// what the parity test renders. Pan lives here rather than on `Camera` because the CPU
    /// renderer is untouched and must not gain a field it does not use.
    pub pan: [f32; 2],
}

pub struct SceneRenderer { /* pipeline, depth, msaa */ }
impl SceneRenderer {
    pub fn new(gpu: &Gpu, format: wgpu::TextureFormat, samples: u32) -> Self;
    pub fn draw(&mut self, gpu: &Gpu, view: &wgpu::TextureView, size: [u32; 2], scene: &Scene);
}

/// 2D viewport: one engine-rendered pixmap, then pan and zoom are GPU state.
pub struct CanvasRenderer { /* quad pipeline, sampler, texture */ }
impl CanvasRenderer {
    pub fn new(gpu: &Gpu, format: wgpu::TextureFormat, samples: u32) -> Self;
    pub fn upload(&mut self, gpu: &Gpu, pixmap: &tiny_skia::Pixmap);   // only when the document changes
    pub fn draw(&mut self, gpu: &Gpu, view: &wgpu::TextureView, size: [u32; 2], view_state: ViewState);
}

#[derive(Clone, Copy)]
pub struct ViewState { pub zoom: f32, pub pan: [f32; 2], pub checker: bool, pub pixelated: bool }

/// Offscreen convenience: render and read back, for tests and for `--gpu` renders.
pub fn render_scene_offscreen(gpu: &Gpu, size: [u32; 2], scene: &Scene)
    -> dpaint_core::Result<tiny_skia::Pixmap>;

/// Build a Scene from a model document, reusing dpaint_model3d::scene_meshes.
pub fn scene_from_document(
    project: &dpaint_core::Project,
    doc: &dpaint_core::DocId,
    assets: &dpaint_core::AssetStore,
    camera: Camera,
    lighting: Lighting,
) -> dpaint_core::Result<Scene>;
```

`Camera` and `Lighting` must be the *same types* as `dpaint_render::preview3d`'s — re-export
them rather than declaring parallel structs, so a field added on one side cannot drift.

## Rules

1. `dpaint-gpu` depends on `dpaint-core`, `dpaint-model3d`, `dpaint-render`, `wgpu`,
   `bytemuck`, `pollster` (native only). It must **not** depend on `dpaint-cli`,
   `dpaint-studio` or `dpaint-ai`.
2. **The CPU path is not touched.** `dpaint_render::preview3d` stays exactly as it is and
   remains what `render.image`, `render.turntable`, goldens and digests use. If you find
   yourself editing it, stop and message Main.
3. No GPU requirement leaks anywhere: every entry point degrades to CPU when `Gpu::new()`
   returns `None`, and says so rather than failing.
4. Column-major world matrices, glTF convention `m[col][row]`, matching
   `dpaint_model3d::scene_meshes`.
5. Tests assert observable behavior. No `todo!()`, no stubs, no placeholder shaders.
6. Run only `cargo test -p <your crate>`. Do not run workspace-wide builds, `cargo fmt` or
   `cargo clippy` — the tree is now fmt-clean and clippy-clean and Main gates it at the end.
   Do keep your own code formatted (`cargo fmt -p <your crate>`) and warning-free.
7. `cargo test --workspace` is currently **457 passed, 0 failed**. That is the baseline.

## Ownership

| Area | Owner |
|---|---|
| `crates/dpaint-gpu/**` | GpuCore |
| `crates/dpaint-view/**` (native interactive window) | ViewNative |
| `crates/dpaint-wasm/**`, `crates/dpaint-studio/ui/**` (canvas + WebGPU path) | ViewWeb |
| root `Cargo.toml`, docs, integration, final gates | Main |

Nobody edits another owner's files. Root manifest edits go through Main: post the exact lines
you need over hub and Main applies them.
