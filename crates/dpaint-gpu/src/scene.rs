//! Forward renderer for model documents.

use crate::camera::Frame;
use crate::{Gpu, Scene};
use wgpu::util::DeviceExt;

const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Vertex {
    position: [f32; 3],
    normal: [f32; 3],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct CameraUniform {
    view_proj: [[f32; 4]; 4],
    forward: [f32; 4],
    key: [f32; 4],
    fill: [f32; 4],
    half_dir: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct MeshUniform {
    world: [[f32; 4]; 4],
    base_color: [f32; 4],
    /// x: shininess, y: specular scale, z/w: padding.
    params: [f32; 4],
}

/// Identity of the geometry currently living in the vertex and index buffers.
///
/// Allocation identity plus length, which is what a rebuilt `Scene` changes. Mutating a
/// mesh's `Vec` contents in place without changing its allocation or length will not be
/// noticed — rebuild the scene (which is what `scene_from_document` does) or call
/// [`SceneRenderer::invalidate_geometry`].
#[derive(PartialEq, Eq, Default)]
struct GeometryKey(Vec<[usize; 6]>);

impl GeometryKey {
    fn of(scene: &Scene) -> Self {
        GeometryKey(
            scene
                .meshes
                .iter()
                .map(|m| {
                    [
                        m.positions.as_ptr() as usize,
                        m.positions.len(),
                        m.normals.as_ptr() as usize,
                        m.normals.len(),
                        m.indices.as_ptr() as usize,
                        m.indices.len(),
                    ]
                })
                .collect(),
        )
    }
}

struct DrawRange {
    indices: std::ops::Range<u32>,
    base_vertex: i32,
    /// Index into `Scene::meshes`, so the uniform slot and the draw cannot drift apart.
    mesh: usize,
}

/// Forward PBR renderer: `MeshData` to buffers, WGSL metallic-roughness, depth buffer, MSAA
/// where the adapter allows it.
///
/// Nothing about the scene is captured at construction. Every `draw` rebuilds the camera and
/// material uniforms from the `Scene` it is handed, so orbiting is just mutating
/// `scene.camera` between frames.
pub struct SceneRenderer {
    pipeline: wgpu::RenderPipeline,
    camera_buf: wgpu::Buffer,
    camera_bg: wgpu::BindGroup,
    mesh_layout: wgpu::BindGroupLayout,
    mesh_buf: Option<wgpu::Buffer>,
    mesh_bg: Option<wgpu::BindGroup>,
    mesh_stride: u32,
    vertex_buf: Option<wgpu::Buffer>,
    index_buf: Option<wgpu::Buffer>,
    draws: Vec<DrawRange>,
    geometry: GeometryKey,
    geometry_uploads: u64,
    depth: Option<wgpu::Texture>,
    msaa: Option<wgpu::Texture>,
    target_size: [u32; 2],
    format: wgpu::TextureFormat,
    samples: u32,
}

impl SceneRenderer {
    /// `samples` is a request. An adapter that cannot render `format` at that count gets 1x
    /// instead of an error; ask [`SceneRenderer::samples`] what you actually got.
    pub fn new(gpu: &Gpu, format: wgpu::TextureFormat, samples: u32) -> Self {
        let samples = usable_samples(gpu, format, samples);
        let device = gpu.device();

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("dpaint-gpu scene"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/scene.wgsl").into()),
        });

        let camera_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("dpaint-gpu camera"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: wgpu::BufferSize::new(
                        std::mem::size_of::<CameraUniform>() as u64
                    ),
                },
                count: None,
            }],
        });
        let mesh_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("dpaint-gpu mesh"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: true,
                    min_binding_size: wgpu::BufferSize::new(
                        std::mem::size_of::<MeshUniform>() as u64
                    ),
                },
                count: None,
            }],
        });

        let camera_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("dpaint-gpu camera"),
            size: std::mem::size_of::<CameraUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let camera_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("dpaint-gpu camera"),
            layout: &camera_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: camera_buf.as_entire_binding(),
            }],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("dpaint-gpu scene"),
            bind_group_layouts: &[Some(&camera_layout), Some(&mesh_layout)],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("dpaint-gpu scene"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Vertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3],
                })],
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                // The CPU rasterizer draws every triangle and flips the normal toward the
                // camera instead of culling, so an inside-out or open mesh looks the same in
                // both renderers. Culling here would be a parity bug, not an optimisation.
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(true),
                depth_compare: Some(wgpu::CompareFunction::Less),
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: wgpu::MultisampleState {
                count: samples,
                ..Default::default()
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });

        let align = device.limits().min_uniform_buffer_offset_alignment;
        let mesh_stride = (std::mem::size_of::<MeshUniform>() as u32).div_ceil(align) * align;

        Self {
            pipeline,
            camera_buf,
            camera_bg,
            mesh_layout,
            mesh_buf: None,
            mesh_bg: None,
            mesh_stride,
            vertex_buf: None,
            index_buf: None,
            draws: Vec::new(),
            geometry: GeometryKey::default(),
            geometry_uploads: 0,
            depth: None,
            msaa: None,
            target_size: [0, 0],
            format,
            samples,
        }
    }

    /// The sample count in use, which may be lower than the one requested.
    pub fn samples(&self) -> u32 {
        self.samples
    }

    /// How many times geometry has been uploaded to the GPU. Orbiting a static scene must not
    /// move this.
    pub fn geometry_uploads(&self) -> u64 {
        self.geometry_uploads
    }

    /// Force the next `draw` to re-upload geometry. Needed only by callers that mutate a
    /// mesh's vectors in place instead of rebuilding the `Scene`.
    pub fn invalidate_geometry(&mut self) {
        self.geometry = GeometryKey(vec![[usize::MAX; 6]]);
    }

    pub fn draw(&mut self, gpu: &Gpu, view: &wgpu::TextureView, size: [u32; 2], scene: &Scene) {
        let size = [size[0].max(1), size[1].max(1)];
        self.ensure_targets(gpu, size);
        self.ensure_geometry(gpu, scene);
        self.write_uniforms(gpu, scene, size);

        let device = gpu.device();
        let depth_view = self
            .depth
            .as_ref()
            .expect("depth target exists after ensure_targets")
            .create_view(&wgpu::TextureViewDescriptor::default());
        let msaa_view = self
            .msaa
            .as_ref()
            .map(|t| t.create_view(&wgpu::TextureViewDescriptor::default()));

        let clear = match scene.background {
            // The CPU path fills the background with the color's sRGB bytes and does not
            // convert it to linear light, so neither do we; the target is a linear-unorm
            // format carrying sRGB values.
            Some(c) => {
                let v = c.to_rgba8();
                wgpu::Color {
                    r: v[0] as f64 / 255.0,
                    g: v[1] as f64 / 255.0,
                    b: v[2] as f64 / 255.0,
                    a: v[3] as f64 / 255.0,
                }
            }
            None => wgpu::Color::TRANSPARENT,
        };

        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("dpaint-gpu scene"),
        });
        {
            let (attachment, resolve) = match msaa_view.as_ref() {
                Some(ms) => (ms, Some(view)),
                None => (view, None),
            };
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("dpaint-gpu scene"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: attachment,
                    resolve_target: resolve,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(clear),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });

            if let (Some(vb), Some(ib), Some(bg)) =
                (&self.vertex_buf, &self.index_buf, &self.mesh_bg)
            {
                pass.set_pipeline(&self.pipeline);
                pass.set_bind_group(0, &self.camera_bg, &[]);
                pass.set_vertex_buffer(0, vb.slice(..));
                pass.set_index_buffer(ib.slice(..), wgpu::IndexFormat::Uint32);
                for (i, d) in self.draws.iter().enumerate() {
                    pass.set_bind_group(1, bg, &[i as u32 * self.mesh_stride]);
                    pass.draw_indexed(d.indices.clone(), d.base_vertex, 0..1);
                }
            }
        }
        gpu.queue().submit(Some(enc.finish()));
    }

    fn ensure_targets(&mut self, gpu: &Gpu, size: [u32; 2]) {
        if self.target_size == size && self.depth.is_some() {
            return;
        }
        let extent = wgpu::Extent3d {
            width: size[0],
            height: size[1],
            depth_or_array_layers: 1,
        };
        self.depth = Some(gpu.device().create_texture(&wgpu::TextureDescriptor {
            label: Some("dpaint-gpu depth"),
            size: extent,
            mip_level_count: 1,
            sample_count: self.samples,
            dimension: wgpu::TextureDimension::D2,
            format: DEPTH_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        }));
        self.msaa = (self.samples > 1).then(|| {
            gpu.device().create_texture(&wgpu::TextureDescriptor {
                label: Some("dpaint-gpu msaa"),
                size: extent,
                mip_level_count: 1,
                sample_count: self.samples,
                dimension: wgpu::TextureDimension::D2,
                format: self.format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            })
        });
        self.target_size = size;
    }

    fn ensure_geometry(&mut self, gpu: &Gpu, scene: &Scene) {
        let key = GeometryKey::of(scene);
        if key == self.geometry && self.vertex_buf.is_some() {
            return;
        }

        let mut vertices: Vec<Vertex> = Vec::new();
        let mut indices: Vec<u32> = Vec::new();
        let mut draws: Vec<DrawRange> = Vec::new();
        for (mesh_index, m) in scene.meshes.iter().enumerate() {
            let base_vertex = vertices.len() as i32;
            let start = indices.len() as u32;
            for (i, p) in m.positions.iter().enumerate() {
                vertices.push(Vertex {
                    position: *p,
                    // `preview3d` substitutes +Y for a missing normal.
                    normal: m.normals.get(i).copied().unwrap_or([0.0, 1.0, 0.0]),
                });
            }
            let limit = m.positions.len() as u32;
            indices.extend(
                m.indices
                    .chunks_exact(3)
                    .filter(|t| t.iter().all(|&i| i < limit))
                    .flatten()
                    .copied(),
            );
            let end = indices.len() as u32;
            if end > start {
                draws.push(DrawRange {
                    indices: start..end,
                    base_vertex,
                    mesh: mesh_index,
                });
            }
        }

        let device = gpu.device();
        if vertices.is_empty() || indices.is_empty() {
            self.vertex_buf = None;
            self.index_buf = None;
            self.draws.clear();
        } else {
            self.vertex_buf = Some(
                device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("dpaint-gpu vertices"),
                    contents: bytemuck::cast_slice(&vertices),
                    usage: wgpu::BufferUsages::VERTEX,
                }),
            );
            self.index_buf = Some(
                device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("dpaint-gpu indices"),
                    contents: bytemuck::cast_slice(&indices),
                    usage: wgpu::BufferUsages::INDEX,
                }),
            );
            self.draws = draws;
        }

        // One uniform slot per mesh, addressed with a dynamic offset.
        let slots = self.draws.len().max(1) as u64;
        let needed = slots * self.mesh_stride as u64;
        let grow = match self.mesh_buf.as_ref() {
            Some(b) => b.size() < needed,
            None => true,
        };
        if grow {
            let buf = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("dpaint-gpu mesh uniforms"),
                size: needed,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            self.mesh_bg = Some(device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("dpaint-gpu mesh uniforms"),
                layout: &self.mesh_layout,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: &buf,
                        offset: 0,
                        size: wgpu::BufferSize::new(std::mem::size_of::<MeshUniform>() as u64),
                    }),
                }],
            }));
            self.mesh_buf = Some(buf);
        }

        self.geometry = key;
        self.geometry_uploads += 1;
    }

    fn write_uniforms(&mut self, gpu: &Gpu, scene: &Scene, size: [u32; 2]) {
        let frame = Frame::build(scene, size);
        let cam = CameraUniform {
            view_proj: frame.view_proj,
            forward: [frame.forward[0], frame.forward[1], frame.forward[2], 0.0],
            key: [frame.key[0], frame.key[1], frame.key[2], frame.ambient],
            fill: [frame.fill[0], frame.fill[1], frame.fill[2], 0.0],
            half_dir: [frame.half_dir[0], frame.half_dir[1], frame.half_dir[2], 0.0],
        };
        gpu.queue()
            .write_buffer(&self.camera_buf, 0, bytemuck::bytes_of(&cam));

        let Some(buf) = self.mesh_buf.as_ref() else {
            return;
        };
        // Drawables and uniform slots are built in the same order, so slot `i` belongs to
        // `draws[i]`; meshes with no drawable triangles occupy neither.
        for (i, d) in self.draws.iter().enumerate() {
            let m = &scene.meshes[d.mesh];
            let base = m.base_color.to_linear();
            // `preview3d`: shininess = clamp(2/roughness^4 - 2, 1, 4096) over roughness
            // clamped to [0.03, 1], and the specular scale folds metallic and roughness.
            let r = m.roughness.clamp(0.03, 1.0);
            let shininess = (2.0 / r.powi(4) - 2.0).clamp(1.0, 4096.0);
            let spec_scale = (0.04 + 0.96 * m.metallic) * (1.0 - m.roughness * 0.7);
            let u = MeshUniform {
                world: m.world,
                base_color: [base[0], base[1], base[2], 1.0],
                params: [shininess, spec_scale, 0.0, 0.0],
            };
            gpu.queue().write_buffer(
                buf,
                i as u64 * self.mesh_stride as u64,
                bytemuck::bytes_of(&u),
            );
        }
    }
}

fn usable_samples(gpu: &Gpu, format: wgpu::TextureFormat, requested: u32) -> u32 {
    if requested <= 1 {
        return 1;
    }
    let ok = |f: wgpu::TextureFormat| {
        gpu.adapter()
            .get_texture_format_features(f)
            .flags
            .sample_count_supported(requested)
    };
    if ok(format) && ok(DEPTH_FORMAT) {
        requested
    } else {
        1
    }
}
