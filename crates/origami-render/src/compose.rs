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
use wgpu::util::DeviceExt;

pub struct Compose {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
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
        Compose { pipeline, layout }
    }

    /// Draw the lines `frame` names into `texture`. Returns the lines drawn,
    /// or None when the frame names memory that is not one of ours or a plane
    /// that does not fit its buffer.
    pub fn draw(&self, gpu: &Gpu, texture: &wgpu::Texture, frame: &ImportedUpdate) -> Option<u32> {
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
        let params = gpu
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("compose params"),
                contents: bytemuck::bytes_of(&Params {
                    width: u.width,
                    height: u.height,
                    stride_words: frame.stride / 4,
                    bottom_up: u32::from(frame.bottom_up),
                }),
                usage: wgpu::BufferUsages::UNIFORM,
            });
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
                    resource: params.as_entire_binding(),
                },
            ],
        });
        let view = texture.create_view(&Default::default());
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        let mut lines = 0;
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
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
            pass.set_bind_group(0, &group, &[]);
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
        gpu.queue.submit([encoder.finish()]);
        Some(lines)
    }
}
