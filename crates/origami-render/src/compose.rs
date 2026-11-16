//! Drawing a head's changed lines straight from memory QEMU wrote.
//!
//! The region is a storage buffer on the device (imported, or a mapping
//! wrapped without a copy), so a fragment pass reads the lines from it and
//! writes them into the head's texture. Nothing is copied by the CPU and no
//! staging buffer exists. The GPU reads while QEMU may be writing, with no
//! fence: that is how the hardware scans out, and QEMU stores a line before
//! it marks it, so a line drawn here is a coherent one or is marked again.

use super::{Gpu, HeadTexture};
use origami_engine::ImportedUpdate;
use std::sync::Mutex;

pub struct Compose {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    params: wgpu::Buffer,
    /// The bind groups of the planes drawn lately: a plane stays put for many
    /// frames, and a producer's slots take turns.
    cached: Mutex<Cache>,
}

#[derive(Default)]
struct Cache {
    /// What `Compose::params` holds now.
    params: Option<Params>,
    groups: Vec<Cached>,
}

/// Slots a producer rotates through is at most a handful.
const CACHED_GROUPS: usize = 8;

struct Cached {
    buffer: wgpu::Buffer,
    offset: u64,
    bytes: u64,
    group: wgpu::BindGroup,
}

#[repr(C)]
#[derive(Clone, Copy, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
struct Params {
    width: u32,
    height: u32,
    stride_words: u32,
    bottom_up: u32,
}

impl Compose {
    pub fn new(gpu: &Gpu) -> Compose {
        let device = &gpu.device;
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("compose"),
            source: wgpu::ShaderSource::Wgsl(include_str!("compose.wgsl").into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("compose"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("compose"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("compose"),
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
                    format: HeadTexture::FORMAT,
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
        let params = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("compose params"),
            size: std::mem::size_of::<Params>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Compose {
            pipeline,
            layout,
            params,
            cached: Mutex::new(Cache::default()),
        }
    }

    /// Draw the lines `frame` names into `texture`. Returns the lines drawn,
    /// or None when the frame names memory that is not one of ours or a plane
    /// that does not fit its buffer.
    pub fn draw(&self, gpu: &Gpu, texture: &wgpu::Texture, frame: &ImportedUpdate) -> Option<u32> {
        let mut enc = gpu.device.create_command_encoder(&Default::default());
        let lines = self.encode(gpu, &mut enc, texture, frame)?;
        gpu.queue.submit([enc.finish()]);
        Some(lines)
    }

    /// [`Compose::draw`] recorded into `enc`, which the caller submits. One
    /// `encode` per submit: the parameters go to one shared buffer.
    pub fn encode(
        &self,
        gpu: &Gpu,
        enc: &mut wgpu::CommandEncoder,
        texture: &wgpu::Texture,
        frame: &ImportedUpdate,
    ) -> Option<u32> {
        let region = frame
            .memory
            .as_any()
            .downcast_ref::<super::ImportedBuffer>()?;
        let u = &frame.update;
        // The last row needs only its pixels, not the stride's padding.
        let plane_bytes = u64::from(frame.stride) * u64::from(u.height.saturating_sub(1))
            + u64::from(u.width) * 4;
        let end = frame.offset.checked_add(plane_bytes)?;
        let limits = gpu.device.limits();
        if end > region.buffer.size()
            || !frame.stride.is_multiple_of(4)
            || u64::from(frame.stride) < u64::from(u.width) * 4
            || !frame
                .offset
                .is_multiple_of(u64::from(limits.min_storage_buffer_offset_alignment))
            || plane_bytes > limits.max_storage_buffer_binding_size
        {
            return None;
        }
        let params = Params {
            width: u.width,
            height: u.height,
            stride_words: frame.stride / 4,
            bottom_up: u32::from(frame.bottom_up),
        };
        let mut cache = self.cached.lock().unwrap_or_else(|e| e.into_inner());
        if cache.params != Some(params) {
            gpu.queue
                .write_buffer(&self.params, 0, bytemuck::bytes_of(&params));
            cache.params = Some(params);
        }
        let at = cache.groups.iter().position(|c| {
            c.buffer == region.buffer && c.offset == frame.offset && c.bytes == plane_bytes
        });
        let at = at.unwrap_or_else(|| {
            let group = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("compose"),
                layout: &self.layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                            buffer: &region.buffer,
                            offset: frame.offset,
                            size: wgpu::BufferSize::new(plane_bytes),
                        }),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: self.params.as_entire_binding(),
                    },
                ],
            });
            if cache.groups.len() >= CACHED_GROUPS {
                cache.groups.remove(0);
            }
            cache.groups.push(Cached {
                buffer: region.buffer.clone(),
                offset: frame.offset,
                bytes: plane_bytes,
                group,
            });
            cache.groups.len() - 1
        });
        let group = &cache.groups[at].group;
        let view = texture.create_view(&Default::default());
        let mut lines = 0;
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("compose"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, group, &[]);
            for &(y0, y1) in &u.runs {
                if y0 >= y1 || y1 > u.height {
                    continue;
                }
                // Only these lines are drawn; the rest of the texture stays.
                pass.set_scissor_rect(0, y0, u.width, y1 - y0);
                pass.draw(0..3, 0..1);
                lines += y1 - y0;
            }
        }
        Some(lines)
    }
}
