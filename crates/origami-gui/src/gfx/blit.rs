//! Our own wgpu pass for a machine view: the head texture to a texture the
//! size of the view's area, run in Slint's before-rendering step. Slint then
//! draws that texture 1:1.
//!
//! Slint's rendering notifier hands over the instance, device and queue but
//! not the window's swapchain texture, so the pass cannot draw into the frame
//! itself. It renders into its own output texture: one pass and one
//! view-sized texture per view.

use super::{Gpu, HeadTexture};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ScaleMode {
    /// The largest whole-number scale that fits, nearest filtering; a view
    /// too small for 1x fits the picture instead.
    #[default]
    Integer,
    /// Fill the view keeping the aspect ratio, linear filtering.
    Fit,
}

pub struct Blit {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    linear: wgpu::Sampler,
    params: wgpu::Buffer,
}

impl Blit {
    pub fn new(gpu: &Gpu) -> Blit {
        let d = &gpu.device;
        let entry = |binding, ty| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty,
            count: None,
        };
        let layout = d.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("blit"),
            entries: &[
                entry(
                    0,
                    wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                ),
                entry(
                    1,
                    wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                ),
                entry(
                    2,
                    wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                ),
            ],
        });
        let module = d.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("blit"),
            source: wgpu::ShaderSource::Wgsl(include_str!("blit.wgsl").into()),
        });
        let pl = d.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("blit"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = d.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("blit"),
            layout: Some(&pl),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        let linear = d.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let params = d.create_buffer(&wgpu::BufferDescriptor {
            label: Some("blit params"),
            size: 32,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Blit {
            pipeline,
            layout,
            linear,
            params,
        }
    }
}

/// One machine's output texture, and what it last showed.
#[derive(Default)]
pub struct Presenter {
    out: Option<wgpu::Texture>,
    area: (u32, u32),
    shown: Option<(u64, u64, ScaleMode)>,
    pub passes: u64,
    pub skipped: u64,
}

impl Presenter {
    /// Size the output to the view's area in physical pixels. Returns the new
    /// texture when it changed (a blank one: the next pass must run).
    pub fn resize(&mut self, gpu: &Gpu, w: u32, h: u32) -> Option<wgpu::Texture> {
        if (w, h) == self.area && self.out.is_some() {
            return None;
        }
        if w == 0 || h == 0 {
            return None;
        }
        let t = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("view out"),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        self.out = Some(t.clone());
        self.area = (w, h);
        self.shown = None;
        Some(t)
    }

    /// Whether the output already shows this head, as it is, in this mode.
    pub fn current(&self, head: &HeadTexture, mode: ScaleMode) -> bool {
        self.out.is_some() && self.shown == Some((head.seq, head.generation, mode))
    }

    /// Run the pass if anything it depends on changed. Returns whether it ran.
    pub fn run(&mut self, gpu: &Gpu, blit: &Blit, head: &HeadTexture, mode: ScaleMode) -> bool {
        let Some(out) = self.out.clone() else {
            return false;
        };
        if self.current(head, mode) {
            self.skipped += 1;
            return false;
        }
        let params: [f32; 8] = [
            self.area.0 as f32,
            self.area.1 as f32,
            head.width as f32,
            head.height as f32,
            f32::from_bits(match mode {
                ScaleMode::Fit => 0,
                ScaleMode::Integer => 1,
            }),
            0.0,
            0.0,
            0.0,
        ];
        gpu.queue
            .write_buffer(&blit.params, 0, bytemuck::cast_slice(&params));
        let sv = head.texture.create_view(&Default::default());
        let group = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("blit"),
            layout: &blit.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&sv),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&blit.linear),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: blit.params.as_entire_binding(),
                },
            ],
        });
        let ov = out.create_view(&Default::default());
        let mut enc = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("blit"),
            });
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("blit"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &ov,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&blit.pipeline);
            pass.set_bind_group(0, &group, &[]);
            pass.draw(0..3, 0..1);
        }
        gpu.queue.submit([enc.finish()]);
        self.shown = Some((head.seq, head.generation, mode));
        self.passes += 1;
        true
    }
}
