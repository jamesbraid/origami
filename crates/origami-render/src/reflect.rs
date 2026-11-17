//! A small GPU copy of a head's picture for the cube to reflect. It is drawn
//! from the head's texture, which already holds the composed picture even
//! when the frame arrived as an imported buffer, so no CPU copy is involved.

use crate::Gpu;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Texels on a side of the reflected picture.
pub const SIZE: u32 = 256;
/// The picture is refreshed at most this often.
const EVERY: Duration = Duration::from_millis(100);

pub struct Reflection {
    texture: wgpu::Texture,
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    last: Mutex<Option<Instant>>,
    /// The head's texture, as the presenting view last saw it.
    source: Mutex<Option<wgpu::Texture>>,
    ready: AtomicBool,
}

impl Reflection {
    pub fn new(gpu: &Gpu) -> Arc<Reflection> {
        let d = &gpu.device;
        let shader = d.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("reflect"),
            source: wgpu::ShaderSource::Wgsl(include_str!("reflect.wgsl").into()),
        });
        let layout = d.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("reflect"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let pl = d.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("reflect"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = d.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("reflect"),
            layout: Some(&pl),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
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
        let texture = d.create_texture(&wgpu::TextureDescriptor {
            label: Some("reflection"),
            size: wgpu::Extent3d {
                width: SIZE,
                height: SIZE,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let sampler = d.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        Arc::new(Reflection {
            texture,
            pipeline,
            layout,
            sampler,
            last: Mutex::new(None),
            source: Mutex::new(None),
            ready: AtomicBool::new(false),
        })
    }

    /// True, and the clock restarts, when a refresh is due: the first one, or
    /// one 100 ms after the last.
    pub fn take_due(&self) -> bool {
        let mut last = self.last.lock().unwrap_or_else(|e| e.into_inner());
        if last.is_some_and(|t| t.elapsed() < EVERY) {
            return false;
        }
        *last = Some(Instant::now());
        true
    }

    /// The presenting view's current head texture, for [`Reflection::refresh`].
    pub fn set_source(&self, head: &wgpu::Texture) {
        *self.source.lock().unwrap_or_else(|e| e.into_inner()) = Some(head.clone());
    }

    /// Copy the head's latest picture now, in its own submit, when a refresh
    /// is due and a source is known: it catches the last frame of a burst and
    /// a head that has gone still.
    pub fn refresh(&self, gpu: &Gpu) {
        let source = self
            .source
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if let Some(head) = source {
            if self.take_due() {
                let mut enc = gpu.device.create_command_encoder(&Default::default());
                self.encode(gpu, &mut enc, &head);
                gpu.queue.submit([enc.finish()]);
                self.done();
            }
        }
    }

    /// Record the downsample of `head` into `enc`; call [`Reflection::done`]
    /// once the encoder is submitted.
    pub fn encode(&self, gpu: &Gpu, enc: &mut wgpu::CommandEncoder, head: &wgpu::Texture) {
        let group = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("reflect"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(
                        &head.create_view(&Default::default()),
                    ),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });
        let target = self.texture.create_view(&Default::default());
        let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("reflect"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &target,
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
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &group, &[]);
        pass.draw(0..3, 0..1);
    }

    /// The encoder that held [`Reflection::encode`] was submitted, so the
    /// texture holds a picture from here on.
    pub fn done(&self) {
        self.ready.store(true, Ordering::Release);
    }

    /// The texture has held a picture at least once.
    pub fn ready(&self) -> bool {
        self.ready.load(Ordering::Acquire)
    }

    pub fn texture(&self) -> &wgpu::Texture {
        &self.texture
    }
}
