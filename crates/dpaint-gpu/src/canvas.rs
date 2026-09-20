//! 2D viewport: the engine renders the document once, this puts it on screen.
//!
//! The division of labour is the point. Rasterizing a vector document is the engine's job and
//! is expensive; panning and zooming the result is not, and must not pay for it. So
//! [`CanvasRenderer::upload`] is called when the document changes and [`CanvasRenderer::draw`]
//! is called per frame, and a drag only ever rewrites a 32-byte uniform.

use crate::{Gpu, ViewState};

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct CanvasUniform {
    centre: [f32; 2],
    half_extent: [f32; 2],
    viewport: [f32; 2],
    flags: [f32; 2],
}

/// A textured quad with a checkerboard behind it.
pub struct CanvasRenderer {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    uniform: wgpu::Buffer,
    linear: wgpu::Sampler,
    nearest: wgpu::Sampler,
    texture: wgpu::Texture,
    texture_size: [u32; 2],
    bind_linear: wgpu::BindGroup,
    bind_nearest: wgpu::BindGroup,
    msaa: Option<wgpu::Texture>,
    target_size: [u32; 2],
    format: wgpu::TextureFormat,
    samples: u32,
    uploads: u64,
}

impl CanvasRenderer {
    pub fn new(gpu: &Gpu, format: wgpu::TextureFormat, samples: u32) -> Self {
        let samples = if samples > 1
            && gpu
                .adapter()
                .get_texture_format_features(format)
                .flags
                .sample_count_supported(samples)
        {
            samples
        } else {
            1
        };
        let device = gpu.device();

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("dpaint-gpu canvas"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/canvas.wgsl").into()),
        });

        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("dpaint-gpu canvas"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(
                            std::mem::size_of::<CanvasUniform>() as u64,
                        ),
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("dpaint-gpu canvas"),
            size: std::mem::size_of::<CanvasUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let sampler = |filter| {
            device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("dpaint-gpu canvas"),
                address_mode_u: wgpu::AddressMode::ClampToEdge,
                address_mode_v: wgpu::AddressMode::ClampToEdge,
                address_mode_w: wgpu::AddressMode::ClampToEdge,
                mag_filter: filter,
                min_filter: filter,
                mipmap_filter: wgpu::MipmapFilterMode::Nearest,
                ..Default::default()
            })
        };
        let linear = sampler(wgpu::FilterMode::Linear);
        let nearest = sampler(wgpu::FilterMode::Nearest);

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("dpaint-gpu canvas"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("dpaint-gpu canvas"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
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

        // A 1x1 transparent placeholder, so drawing before the first upload shows the
        // checker instead of failing.
        let (texture, bind_linear, bind_nearest) =
            Self::make_texture(device, &layout, &uniform, &linear, &nearest, [1, 1]);

        Self {
            pipeline,
            layout,
            uniform,
            linear,
            nearest,
            texture,
            texture_size: [0, 0],
            bind_linear,
            bind_nearest,
            msaa: None,
            target_size: [0, 0],
            format,
            samples,
            uploads: 0,
        }
    }

    fn make_texture(
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        uniform: &wgpu::Buffer,
        linear: &wgpu::Sampler,
        nearest: &wgpu::Sampler,
        size: [u32; 2],
    ) -> (wgpu::Texture, wgpu::BindGroup, wgpu::BindGroup) {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("dpaint-gpu canvas"),
            size: wgpu::Extent3d {
                width: size[0].max(1),
                height: size[1].max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            // The pixmap is premultiplied sRGB and is sampled through unchanged, so the view
            // must not apply a transfer function of its own.
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let group = |sampler: &wgpu::Sampler, label: &str| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some(label),
                layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: uniform.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::Sampler(sampler),
                    },
                ],
            })
        };
        let bind_linear = group(linear, "dpaint-gpu canvas linear");
        let bind_nearest = group(nearest, "dpaint-gpu canvas nearest");
        (texture, bind_linear, bind_nearest)
    }

    /// Put a rendered document on the GPU. Call this when the document changes — never per
    /// frame, and never while dragging.
    pub fn upload(&mut self, gpu: &Gpu, pixmap: &tiny_skia::Pixmap) {
        let size = [pixmap.width(), pixmap.height()];
        if size != self.texture_size {
            let (texture, bind_linear, bind_nearest) = Self::make_texture(
                gpu.device(),
                &self.layout,
                &self.uniform,
                &self.linear,
                &self.nearest,
                size,
            );
            self.texture = texture;
            self.bind_linear = bind_linear;
            self.bind_nearest = bind_nearest;
            self.texture_size = size;
        }
        gpu.queue().write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            pixmap.data(),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(size[0] * 4),
                rows_per_image: Some(size[1]),
            },
            wgpu::Extent3d {
                width: size[0],
                height: size[1],
                depth_or_array_layers: 1,
            },
        );
        self.uploads += 1;
    }

    /// How many times a pixmap has been uploaded. Pan and zoom must never move this.
    pub fn uploads(&self) -> u64 {
        self.uploads
    }

    /// Size of the uploaded image in texels, or `[0, 0]` before the first upload.
    pub fn image_size(&self) -> [u32; 2] {
        self.texture_size
    }

    pub fn samples(&self) -> u32 {
        self.samples
    }

    pub fn draw(
        &mut self,
        gpu: &Gpu,
        view: &wgpu::TextureView,
        size: [u32; 2],
        view_state: ViewState,
    ) {
        let size = [size[0].max(1), size[1].max(1)];
        self.ensure_targets(gpu, size);

        let zoom = if view_state.zoom.is_finite() && view_state.zoom > 0.0 {
            view_state.zoom
        } else {
            1.0
        };
        let pan = [
            if view_state.pan[0].is_finite() {
                view_state.pan[0]
            } else {
                0.0
            },
            if view_state.pan[1].is_finite() {
                view_state.pan[1]
            } else {
                0.0
            },
        ];
        let image = [self.texture_size[0] as f32, self.texture_size[1] as f32];
        let u = CanvasUniform {
            centre: [size[0] as f32 * 0.5 + pan[0], size[1] as f32 * 0.5 + pan[1]],
            half_extent: [image[0] * zoom * 0.5, image[1] * zoom * 0.5],
            viewport: [size[0] as f32, size[1] as f32],
            flags: [if view_state.checker { 1.0 } else { 0.0 }, 0.0],
        };
        gpu.queue()
            .write_buffer(&self.uniform, 0, bytemuck::bytes_of(&u));

        let msaa_view = self
            .msaa
            .as_ref()
            .map(|t| t.create_view(&wgpu::TextureViewDescriptor::default()));
        let mut enc = gpu
            .device()
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("dpaint-gpu canvas"),
            });
        {
            let (attachment, resolve) = match msaa_view.as_ref() {
                Some(ms) => (ms, Some(view)),
                None => (view, None),
            };
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("dpaint-gpu canvas"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: attachment,
                    resolve_target: resolve,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            let bind = if view_state.pixelated {
                &self.bind_nearest
            } else {
                &self.bind_linear
            };
            pass.set_bind_group(0, bind, &[]);
            pass.draw(0..6, 0..1);
        }
        gpu.queue().submit(Some(enc.finish()));
    }

    fn ensure_targets(&mut self, gpu: &Gpu, size: [u32; 2]) {
        if self.target_size == size {
            return;
        }
        self.msaa = (self.samples > 1).then(|| {
            gpu.device().create_texture(&wgpu::TextureDescriptor {
                label: Some("dpaint-gpu canvas msaa"),
                size: wgpu::Extent3d {
                    width: size[0],
                    height: size[1],
                    depth_or_array_layers: 1,
                },
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
}
