//! Our own wgpu pass for a machine view: the head texture to a texture the
//! size of the view's area, run in Slint's before-rendering step. Slint then
//! draws that texture 1:1.
//!
//! Slint's rendering notifier hands over the instance, device and queue but
//! not the window's swapchain texture, so the pass cannot draw into the frame
//! itself. It renders into its own output texture: one pass and one
//! view-sized texture per view.

use super::{Gpu, HeadTexture};
use std::sync::Mutex;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ScaleMode {
    /// The largest whole-number scale that fits, nearest filtering; a view
    /// too small for 1x fits the picture instead.
    #[default]
    Integer,
    /// Fill the view keeping the aspect ratio, sharp bilinear: guest pixels
    /// stay flat and blend over one output pixel at their edges.
    Fit,
}

/// Where the picture lands in the view, in the view's physical pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Layout {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    /// Physical pixels per guest pixel.
    pub scale: f32,
}

/// Where `blit.wgsl` draws a picture of `src` pixels in `area`: the shader
/// takes its origin from here, so a host maps the pointer through the same numbers.
pub fn layout(area: (u32, u32), src: (u32, u32), mode: ScaleMode) -> Layout {
    let (area, src) = ((area.0 as f32, area.1 as f32), (src.0 as f32, src.1 as f32));
    let fit = (area.0 / src.0).min(area.1 / src.1);
    let whole = match mode {
        ScaleMode::Fit => false,
        ScaleMode::Integer => fit >= 1.0,
    };
    let scale = if whole { fit.floor() } else { fit };
    // A fill can come out a hair over the area, which would floor the origin to -1.
    let (width, height) = ((src.0 * scale).min(area.0), (src.1 * scale).min(area.1));
    Layout {
        x: ((area.0 - width) * 0.5).floor(),
        y: ((area.1 - height) * 0.5).floor(),
        width,
        height,
        scale,
    }
}

/// The guest's cursor to draw into the picture: its image, where its top left
/// falls in head pixels, and which version of the image this is.
pub struct CursorDraw<'a> {
    pub texture: &'a wgpu::Texture,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub gen: u32,
}

pub struct Blit {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    linear: wgpu::Sampler,
    params: wgpu::Buffer,
    /// Bound when there is no cursor.
    blank: wgpu::Texture,
    /// The bind group of the textures last drawn: they change only on a
    /// resize or a new cursor image, not per frame.
    cached: Mutex<Option<(wgpu::Texture, Option<wgpu::Texture>, wgpu::BindGroup)>>,
    /// The parameters last written; most frames repeat them.
    written: Mutex<Option<[u32; 12]>>,
}

impl Blit {
    pub fn new(gpu: &Gpu) -> Blit {
        Blit::with_format(gpu, wgpu::TextureFormat::Rgba8Unorm)
    }

    /// A pass that renders into targets of `format`. The view-sized textures
    /// of the Slint path are Rgba8Unorm; a window's surface offers what its
    /// platform does.
    pub fn with_format(gpu: &Gpu, format: wgpu::TextureFormat) -> Blit {
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
                entry(
                    3,
                    wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
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
                    format,
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
            size: 48,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let blank = cursor_texture(gpu);
        Blit {
            pipeline,
            layout,
            linear,
            params,
            blank,
            cached: Mutex::new(None),
            written: Mutex::new(None),
        }
    }

    /// Draw `head` into `target`, an `area`-sized view of a texture in this
    /// pass's format: scaled by `mode`, with the guest cursor over it.
    pub fn draw(
        &self,
        gpu: &Gpu,
        target: &wgpu::TextureView,
        area: (u32, u32),
        head: &HeadTexture,
        mode: ScaleMode,
        cursor: Option<&CursorDraw>,
    ) {
        let mut enc = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("blit"),
            });
        self.encode(gpu, &mut enc, target, area, head, mode, cursor);
        gpu.queue.submit([enc.finish()]);
    }

    /// [`Blit::draw`] recorded into `enc`, which the caller submits. One
    /// pass per submit: the parameters are written to one buffer.
    #[allow(clippy::too_many_arguments)]
    pub fn encode(
        &self,
        gpu: &Gpu,
        enc: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        area: (u32, u32),
        head: &HeadTexture,
        mode: ScaleMode,
        cursor: Option<&CursorDraw>,
    ) {
        let l = layout(area, (head.width, head.height), mode);
        let params: [f32; 12] = [
            area.0 as f32,
            area.1 as f32,
            head.width as f32,
            head.height as f32,
            f32::from_bits(match mode {
                ScaleMode::Fit => 0,
                ScaleMode::Integer => 1,
            }),
            f32::from_bits(u32::from(cursor.is_some())),
            l.x,
            l.y,
            cursor.map_or(0.0, |c| c.x as f32),
            cursor.map_or(0.0, |c| c.y as f32),
            cursor.map_or(0.0, |c| c.width as f32),
            cursor.map_or(0.0, |c| c.height as f32),
        ];
        let bits = params.map(f32::to_bits);
        let mut written = self.written.lock().unwrap_or_else(|e| e.into_inner());
        if *written != Some(bits) {
            gpu.queue
                .write_buffer(&self.params, 0, bytemuck::cast_slice(&params));
            *written = Some(bits);
        }
        drop(written);
        let mut cached = self.cached.lock().unwrap_or_else(|e| e.into_inner());
        let hit = cached
            .as_ref()
            .is_some_and(|(h, c, _)| *h == head.texture && c.as_ref() == cursor.map(|c| c.texture));
        if !hit {
            let sv = head.texture.create_view(&Default::default());
            let cv = cursor
                .map_or(&self.blank, |c| c.texture)
                .create_view(&Default::default());
            let group = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("blit"),
                layout: &self.layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&sv),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&self.linear),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: self.params.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::TextureView(&cv),
                    },
                ],
            });
            *cached = Some((
                head.texture.clone(),
                cursor.map(|c| c.texture.clone()),
                group,
            ));
        }
        let group = &cached.as_ref().expect("set above").2;
        let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("blit"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
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
        pass.set_bind_group(0, group, &[]);
        pass.draw(0..3, 0..1);
    }
}

/// A texture for a cursor image: the largest cursor the protocol has.
pub fn cursor_texture(gpu: &Gpu) -> wgpu::Texture {
    gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("cursor"),
        size: wgpu::Extent3d {
            width: 64,
            height: 64,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        // 0xAARRGGBB little-endian words: blue, green, red, alpha in memory.
        format: wgpu::TextureFormat::Bgra8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    })
}

/// What an output texture shows: the head's sequence and generation, the
/// scale mode, and the cursor's place and version.
type Shown = (u64, u64, ScaleMode, Option<(i32, i32, u32, u32, u32)>);

/// One machine's output texture, and what it last showed.
#[derive(Default)]
pub struct Presenter {
    out: Option<wgpu::Texture>,
    area: (u32, u32),
    shown: Option<Shown>,
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

    /// The output texture, for the tests to read back.
    #[cfg(test)]
    pub fn out_texture(&self) -> Option<&wgpu::Texture> {
        self.out.as_ref()
    }

    fn state(head: &HeadTexture, mode: ScaleMode, cursor: Option<&CursorDraw>) -> Shown {
        (
            head.seq,
            head.generation,
            mode,
            cursor.map(|c| (c.x, c.y, c.width, c.height, c.gen)),
        )
    }

    /// Whether the output already shows this head, as it is, in this mode,
    /// with the cursor where it is.
    pub fn current(
        &self,
        head: &HeadTexture,
        mode: ScaleMode,
        cursor: Option<&CursorDraw>,
    ) -> bool {
        self.out.is_some() && self.shown == Some(Self::state(head, mode, cursor))
    }

    /// Run the pass if anything it depends on changed. Returns whether it ran.
    pub fn run(
        &mut self,
        gpu: &Gpu,
        blit: &Blit,
        head: &HeadTexture,
        mode: ScaleMode,
        cursor: Option<&CursorDraw>,
    ) -> bool {
        let Some(out) = self.out.clone() else {
            return false;
        };
        if self.current(head, mode, cursor) {
            self.skipped += 1;
            return false;
        }
        let ov = out.create_view(&Default::default());
        blit.draw(gpu, &ov, self.area, head, mode, cursor);
        self.shown = Some(Self::state(head, mode, cursor));
        self.passes += 1;
        true
    }
}

#[cfg(test)]
mod layout_tests {
    use super::*;

    #[test]
    fn fit_fills_at_any_scale_and_centres_the_rest() {
        // 1.5 device pixels per guest pixel stays 1.5: no stepping down.
        let l = layout((1920, 1636), (1280, 1024), ScaleMode::Fit);
        assert_eq!(
            (l.scale, l.width, l.height, l.y),
            (1.5, 1920.0, 1536.0, 50.0)
        );
        let l = layout((1000, 3000), (1000, 1000), ScaleMode::Fit);
        assert_eq!((l.scale, l.y), (1.0, 1000.0));
    }

    #[test]
    fn integer_and_fit_keep_their_old_scales() {
        assert_eq!(
            layout((1400, 1000), (400, 300), ScaleMode::Integer).scale,
            3.0
        );
        assert_eq!(
            layout((600, 300), (400, 300), ScaleMode::Integer).scale,
            1.0
        );
        assert_eq!(layout((600, 450), (400, 300), ScaleMode::Fit).scale, 1.5);
    }
}
