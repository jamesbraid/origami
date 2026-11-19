//! The SGI tube cube, drawn live: a chrome mirror reflecting an environment,
//! over a granite backdrop, into a host surface. A second view kind beside
//! the guest screen, on the same device and the same surface attach.
//!
//! Like [`View`], a [`CubeView`] spawns no thread and draws only when the
//! host calls [`CubeView::draw`]; the host stops calling when `draw` says the
//! cube is at rest, so a still or hidden cube costs nothing.

use crate::motion::Motion;
use crate::reflect::Reflection;
use crate::{Drawn, Gpu, Host, View};
use std::f32::consts::FRAC_1_SQRT_2;
use std::sync::Arc;
use std::time::{Duration, Instant};

const GLB: &[u8] = include_bytes!("../../../resources/identity/cube.glb");
const GRANITE: &[u8] = include_bytes!("../../../resources/identity/granite-tile.png");

/// Half the view width in model units (mark.svg's framing).
const HALF: f32 = 10.25;
/// Screen right, up and toward the viewer, on the (1,1,1) diagonal.
const VIEW: [[f32; 3]; 3] = [
    [-FRAC_1_SQRT_2, FRAC_1_SQRT_2, 0.0],
    [-0.408_248_3, -0.408_248_3, 0.816_496_6],
    [0.577_350_3, 0.577_350_3, 0.577_350_3],
];
/// The cube's width as a share of the view's shorter side.
const FILL: f32 = 0.9;
/// Multisampling to ask for, best first; the first the device supports for
/// both the colour and depth formats is used.
const SAMPLES: [u32; 3] = [8, 4, 2];
const DEPTH: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
/// At most 60 frames a second, whatever the display's refresh rate; 30 while
/// only the calm spin moves the cube, which looks the same at 10 rpm.
const MIN_FRAME: Duration = Duration::from_millis(12);
const CALM_FRAME: Duration = Duration::from_millis(28);
/// How long one scene takes to fade into the next.
const FADE: Duration = Duration::from_millis(400);
/// A frame after a longer pause advances the animation by no more than this.
const MAX_STEP: f32 = 0.1;

/// The scenes embedded from `resources/identity/scenes/scenes.json`.
pub mod scenes {
    include!(concat!(env!("OUT_DIR"), "/scenes.rs"));

    pub fn count() -> usize {
        SCENES.len()
    }

    /// The index of the scene with this id.
    pub fn find(id: &str) -> Option<usize> {
        SCENES.iter().position(|s| s.id == id)
    }

    /// The scene for a local time: the first, in file order, whose hours hold
    /// it; with none (a gap in the hours) the one that began most recently.
    pub fn for_time(hour: u32, minute: u32) -> usize {
        pick(&hours(), hour as f32 + minute as f32 / 60.0)
    }

    /// The scene after `from` in the manifest's `about.order`, round to the first.
    pub fn after(from: Option<usize>) -> usize {
        next_in(ORDER, from)
    }

    fn hours() -> Vec<(f32, f32)> {
        SCENES.iter().map(|s| (s.from, s.to)).collect()
    }

    pub(crate) fn next_in(order: &[usize], from: Option<usize>) -> usize {
        let at = from.and_then(|f| order.iter().position(|&i| i == f));
        order[at.map_or(0, |a| (a + 1) % order.len())]
    }

    pub(crate) fn pick(hours: &[(f32, f32)], t: f32) -> usize {
        let holds = |&(from, to): &(f32, f32)| {
            if from <= to {
                from <= t && t < to
            } else {
                t >= from || t < to
            }
        };
        if let Some(i) = hours.iter().position(holds) {
            return i;
        }
        // The latest start at or before t, going back round midnight.
        let back = |from: f32| (t - from).rem_euclid(24.0);
        let nearest = hours.iter().map(|h| back(h.0)).fold(f32::MAX, f32::min);
        hours.iter().position(|h| back(h.0) == nearest).unwrap_or(0)
    }
}

/// What is drawn behind the cube.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backdrop {
    /// Nothing: the cube composites over the host's surface where the window
    /// system allows it, so there is no visible view edge.
    None,
    /// The granite tile, in the style's two tones, over the whole view.
    Granite,
}

/// What the chrome reflects.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Env {
    /// The analytic studio, with a horizon band that can take a colour.
    Studio,
    /// A photograph: an index into [`scenes::SCENES`].
    Scene(usize),
    /// The running machine's screen (see [`CubeView::set_reflection`]); the
    /// studio until a picture has arrived.
    Screen,
    /// The classic chrome horizon: a dark ground, a crisp bright band and a
    /// sky from near-white to deep blue. The horizon colour tints the band.
    Horizon,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Style {
    pub env: Env,
    /// The studio's horizon band colour (0 to 1 per channel), or none.
    pub horizon: Option<[f32; 3]>,
    /// The granite's two tones: the tile's light pixels, then its dark ones.
    pub granite: [[f32; 3]; 2],
    /// The lighter chrome studio, for a light window.
    pub light_look: bool,
    /// Turn at 10 rpm; without it the cube is at rest until dragged.
    pub spin: bool,
    /// Draw the still pose and never spin; a drag still turns it.
    pub reduce_motion: bool,
    pub backdrop: Backdrop,
    /// What fills the view where the window system gives no transparency and
    /// there is no granite: the host's own background.
    pub surround: [f32; 3],
}

impl Default for Style {
    fn default() -> Style {
        let tone = |hex: u32| [16, 8, 0].map(|s| ((hex >> s) & 0xff) as f32 / 255.0);
        Style {
            env: Env::Studio,
            horizon: None,
            granite: [tone(0x282a30), tone(0x1f2126)],
            light_look: false,
            spin: false,
            reduce_motion: false,
            backdrop: Backdrop::None,
            surround: [0.0; 3],
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pointer {
    Press,
    Move,
    Release,
    /// Ease back to the mark pose.
    DoubleClick,
}

/// The mesh with smooth normals: position and normal, six floats a vertex.
struct Mesh {
    verts: Vec<[f32; 6]>,
    idx: Vec<u16>,
    reach: f32,
}

type V3 = [f32; 3];

fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn cross(a: V3, b: V3) -> V3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn unit(a: V3) -> V3 {
    let n = (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt().max(1e-20);
    [a[0] / n, a[1] / n, a[2] / n]
}

fn dot(a: V3, b: V3) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

/// Faces meeting at more than this are an edge, not a curve.
const CREASE_DEGREES: f32 = 60.0;

/// Each corner's normal sums the faces at its vertex within [`CREASE_DEGREES`]
/// of its own, by area; a vertex on a crease is split.
fn smooth_split(pos: &[V3], idx: &[u16]) -> (Vec<[f32; 6]>, Vec<u32>) {
    let tris: Vec<[usize; 3]> = idx
        .as_chunks::<3>()
        .0
        .iter()
        .map(|t| t.map(usize::from))
        .collect();
    let area: Vec<V3> = tris
        .iter()
        .map(|&[a, b, c]| cross(sub(pos[b], pos[a]), sub(pos[c], pos[a])))
        .collect();
    let face: Vec<V3> = area.iter().map(|&a| unit(a)).collect();
    let mut at = vec![Vec::new(); pos.len()];
    for (f, t) in tris.iter().enumerate() {
        for &v in t {
            at[v].push(f);
        }
    }
    let cos = CREASE_DEGREES.to_radians().cos();
    let mut made: std::collections::HashMap<(usize, [i32; 3]), u32> = Default::default();
    let mut verts = Vec::new();
    let mut out = Vec::with_capacity(idx.len());
    for (f, t) in tris.iter().enumerate() {
        for &v in t {
            let mut sum = [0.0f32; 3];
            for &g in at[v].iter().filter(|&&g| dot(face[f], face[g]) >= cos) {
                for (s, n) in sum.iter_mut().zip(area[g]) {
                    *s += n;
                }
            }
            let n = unit(sum);
            let key = (v, n.map(|c| (c * 1000.0).round() as i32));
            let i = *made.entry(key).or_insert_with(|| {
                verts.push([pos[v][0], pos[v][1], pos[v][2], n[0], n[1], n[2]]);
                (verts.len() - 1) as u32
            });
            out.push(i);
        }
    }
    (verts, out)
}

impl Mesh {
    /// irix-theme's cube.glb: int16 positions in 8-byte records and u16 indices;
    /// [`smooth_split`] makes the normals.
    fn from_glb(glb: &[u8]) -> Result<Mesh, String> {
        let bad = |what: &str| format!("cube.glb: {what}");
        let u32_at = |o: usize| -> Result<usize, String> {
            let b = glb.get(o..o + 4).ok_or_else(|| bad("truncated"))?;
            Ok(u32::from_le_bytes(b.try_into().unwrap()) as usize)
        };
        let jlen = u32_at(12)?;
        let json: serde_json::Value =
            serde_json::from_slice(glb.get(20..20 + jlen).ok_or_else(|| bad("truncated"))?)
                .map_err(|e| bad(&e.to_string()))?;
        let bin = 20 + jlen + 8;
        let num = |v: &serde_json::Value| v.as_u64().map(|n| n as usize);
        let accessor = |a: usize| -> Result<(usize, usize), String> {
            let i = num(&json["accessors"][a]["bufferView"]).ok_or_else(|| bad("accessor"))?;
            let start = bin + num(&json["bufferViews"][i]["byteOffset"]).unwrap_or(0);
            let count = num(&json["accessors"][a]["count"]).ok_or_else(|| bad("count"))?;
            Ok((start, count))
        };
        let scale = json["nodes"][0]["scale"][0]
            .as_f64()
            .ok_or_else(|| bad("scale"))? as f32
            / 32767.0;
        let (at, count) = accessor(0)?;
        let (iat, icount) = accessor(1)?;
        let data = glb
            .get(at..at + count * 8)
            .zip(glb.get(iat..iat + icount * 2))
            .ok_or_else(|| bad("truncated"))?;
        let (pos_bytes, idx_bytes) = data;
        let pos: Vec<V3> = pos_bytes
            .as_chunks::<8>()
            .0
            .iter()
            .map(|c| {
                let k = |i: usize| f32::from(i16::from_le_bytes([c[i * 2], c[i * 2 + 1]])) * scale;
                [k(0), k(1), k(2)]
            })
            .collect();
        let idx: Vec<u16> = idx_bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes(*c))
            .collect();
        if idx.iter().any(|&i| i as usize >= pos.len()) {
            return Err(bad("index out of range"));
        }
        let reach = pos
            .iter()
            .map(|p| (p[0] * p[0] + p[1] * p[1] + p[2] * p[2]).sqrt())
            .fold(0.0, f32::max);
        let (verts, idx) = smooth_split(&pos, &idx);
        let idx = idx
            .into_iter()
            .map(u16::try_from)
            .collect::<Result<Vec<u16>, _>>()
            .map_err(|_| bad("too many vertices"))?;
        Ok(Mesh { verts, idx, reach })
    }
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    mvp: [[f32; 4]; 4],
    nrm: [[f32; 4]; 4],
    look: [f32; 4],
    tint: [f32; 4],
    mode: [f32; 4],
    dark: [f32; 4],
    light: [f32; 4],
    /// The photograph's mean colour, for the fill.
    fill: [f32; 4],
    /// The photograph fading out: its mean, and the fade's progress.
    fill_prev: [f32; 4],
}

struct Targets {
    size: (u32, u32),
    color: wgpu::TextureView,
    depth: wgpu::TextureView,
}

/// The pipelines, mesh and textures of the cube for one target format.
pub struct CubeRenderer {
    format: wgpu::TextureFormat,
    /// Multisampling in use: 1 when the device offers none.
    samples: u32,
    backdrop: wgpu::RenderPipeline,
    cube: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    vertices: wgpu::Buffer,
    indices: wgpu::Buffer,
    index_count: u32,
    reach: f32,
    uniforms: wgpu::Buffer,
    tile: wgpu::TextureView,
    blank: wgpu::TextureView,
    /// Each loaded photograph and its average colour (linear).
    photos: Vec<Option<(wgpu::TextureView, [f32; 3])>>,
    /// The photo the bind group shows.
    scene: Option<usize>,
    /// The photograph fading out and how far the fade to `scene` has come.
    prev: Option<usize>,
    fade: f32,
    reflection: Option<Arc<Reflection>>,
    bind: Option<wgpu::BindGroup>,
    targets: Option<Targets>,
    /// The target composites over what is behind it, so the cleared
    /// background is transparent.
    pub(crate) transparent: bool,
}

fn decode_granite() -> Result<Vec<u8>, String> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(GRANITE));
    decoder.set_transformations(png::Transformations::EXPAND);
    let mut reader = decoder.read_info().map_err(|e| e.to_string())?;
    let mut buf = vec![0; reader.output_buffer_size().ok_or("granite tile too big")?];
    let info = reader.next_frame(&mut buf).map_err(|e| e.to_string())?;
    if (info.width, info.height) != (256, 256) {
        return Err("granite-tile.png is not 256 by 256".into());
    }
    let channels = info.color_type.samples();
    // Two colours (a grey and a teal): the brighter by luma are the light pixels.
    Ok(buf[..info.buffer_size()]
        .chunks_exact(channels)
        .map(|p| {
            let luma = if channels >= 3 {
                0.299 * f32::from(p[0]) + 0.587 * f32::from(p[1]) + 0.114 * f32::from(p[2])
            } else {
                f32::from(p[0])
            };
            if luma >= 100.0 {
                255
            } else {
                0
            }
        })
        .collect())
}

fn texture_2d(
    gpu: &Gpu,
    label: &str,
    size: (u32, u32),
    format: wgpu::TextureFormat,
    mips: u32,
) -> wgpu::Texture {
    gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width: size.0,
            height: size.1,
            depth_or_array_layers: 1,
        },
        mip_level_count: mips,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    })
}

fn upload(gpu: &Gpu, t: &wgpu::Texture, mip: u32, size: (u32, u32), bpp: u32, data: &[u8]) {
    gpu.queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: t,
            mip_level: mip,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        data,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(size.0 * bpp),
            rows_per_image: Some(size.1),
        },
        wgpu::Extent3d {
            width: size.0,
            height: size.1,
            depth_or_array_layers: 1,
        },
    );
}

fn srgb_to_linear(v: u8) -> f32 {
    let c = f32::from(v) / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_to_srgb(l: f32) -> u8 {
    let c = if l <= 0.003_130_8 {
        l * 12.92
    } else {
        1.055 * l.powf(1.0 / 2.4) - 0.055
    };
    (c.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// Each level the average of 2 by 2 of the one above, down to 1 by 1, taken
/// as light, not as sRGB numbers, so small bright detail does not darken.
fn mip_chain(rgba: Vec<u8>, w: u32, h: u32) -> Vec<((u32, u32), Vec<u8>)> {
    let lut: Vec<f32> = (0..=255).map(srgb_to_linear).collect();
    let mut out = vec![((w, h), rgba)];
    while out.last().is_some_and(|((w, h), _)| *w > 1 || *h > 1) {
        let ((sw, sh), src) = out.last().unwrap();
        let (dw, dh) = ((sw / 2).max(1), (sh / 2).max(1));
        let mut dst = Vec::with_capacity((dw * dh * 4) as usize);
        for y in 0..dh {
            for x in 0..dw {
                for c in 0..4 {
                    let mut sum = 0.0;
                    let mut alpha = 0;
                    for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                        let (sx, sy) = ((x * 2 + dx).min(sw - 1), (y * 2 + dy).min(sh - 1));
                        let v = src[((sy * sw + sx) * 4 + c) as usize];
                        sum += lut[usize::from(v)];
                        alpha += u32::from(v);
                    }
                    dst.push(if c == 3 {
                        ((alpha + 2) / 4) as u8
                    } else {
                        linear_to_srgb(sum / 4.0)
                    });
                }
            }
        }
        let size = (dw, dh);
        out.push((size, dst));
    }
    out
}

/// The most multisampling the device offers for the cube's colour and depth.
fn sample_count(gpu: &Gpu, format: wgpu::TextureFormat) -> u32 {
    let ok = |f: wgpu::TextureFormat, n: u32| {
        gpu.adapter
            .get_texture_format_features(f)
            .flags
            .sample_count_supported(n)
    };
    let resolves = gpu
        .adapter
        .get_texture_format_features(format)
        .flags
        .contains(wgpu::TextureFormatFeatureFlags::MULTISAMPLE_RESOLVE);
    SAMPLES
        .into_iter()
        .find(|&n| resolves && ok(format, n) && ok(DEPTH, n))
        .unwrap_or(1)
}

fn photo_view(gpu: &Gpu, scene: usize) -> Result<(wgpu::TextureView, [f32; 3]), String> {
    let mut decoder =
        image_webp::WebPDecoder::new(std::io::Cursor::new(scenes::SCENES[scene].image))
            .map_err(|e| e.to_string())?;
    let (w, h) = decoder.dimensions();
    let mut raw = vec![0; decoder.output_buffer_size().ok_or("photo too big")?];
    decoder.read_image(&mut raw).map_err(|e| e.to_string())?;
    let rgba: Vec<u8> = if decoder.has_alpha() {
        raw
    } else {
        raw.as_chunks::<3>()
            .0
            .iter()
            .flat_map(|p| [p[0], p[1], p[2], 255])
            .collect()
    };
    Ok(photo_from_rgba(gpu, rgba, w, h))
}

fn photo_from_rgba(gpu: &Gpu, rgba: Vec<u8>, w: u32, h: u32) -> (wgpu::TextureView, [f32; 3]) {
    let chain = mip_chain(rgba, w, h);
    let mut sum = [0.0f64; 3];
    for p in chain[0].1.as_chunks::<4>().0 {
        for k in 0..3 {
            sum[k] += f64::from(srgb_to_linear(p[k]));
        }
    }
    let mean = sum.map(|c| (c / f64::from(w * h)) as f32);
    let t = texture_2d(
        gpu,
        "scene",
        (w, h),
        wgpu::TextureFormat::Rgba8UnormSrgb,
        chain.len() as u32,
    );
    for (level, (size, data)) in chain.iter().enumerate() {
        upload(gpu, &t, level as u32, *size, 4, data);
    }
    (t.create_view(&Default::default()), mean)
}

impl CubeRenderer {
    pub fn new(gpu: &Gpu, format: wgpu::TextureFormat) -> Result<CubeRenderer, String> {
        use wgpu::util::DeviceExt;
        let mesh = Mesh::from_glb(GLB)?;
        let d = &gpu.device;
        let shader = d.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("cube"),
            source: wgpu::ShaderSource::Wgsl(include_str!("cube.wgsl").into()),
        });
        let tex_entry = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let layout = d.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("cube"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                tex_entry(1),
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                tex_entry(3),
                tex_entry(4),
                tex_entry(5),
                tex_entry(6),
            ],
        });
        let pl = d.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("cube"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let target = [Some(wgpu::ColorTargetState {
            format,
            blend: None,
            write_mask: wgpu::ColorWrites::ALL,
        })];
        let samples = sample_count(gpu, format);
        let multisample = wgpu::MultisampleState {
            count: samples,
            mask: !0,
            alpha_to_coverage_enabled: false,
        };
        let pipeline = |label: &str,
                        vs: &str,
                        fs: &str,
                        buffers: &[Option<wgpu::VertexBufferLayout>],
                        write: bool| {
            d.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&pl),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some(vs),
                    compilation_options: Default::default(),
                    buffers,
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some(fs),
                    compilation_options: Default::default(),
                    targets: &target,
                }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: DEPTH,
                    depth_write_enabled: Some(write),
                    depth_compare: Some(if write {
                        wgpu::CompareFunction::Less
                    } else {
                        wgpu::CompareFunction::Always
                    }),
                    stencil: Default::default(),
                    bias: Default::default(),
                }),
                multisample,
                multiview_mask: None,
                cache: None,
            })
        };
        let attrs = wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3];
        let backdrop = pipeline("granite", "bg_vs", "bg_fs", &[], false);
        let cube = pipeline(
            "cube",
            "vs",
            "fs",
            &[Some(wgpu::VertexBufferLayout {
                array_stride: 24,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &attrs,
            })],
            true,
        );
        let vertices = d.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("cube vertices"),
            contents: bytemuck::cast_slice(&mesh.verts),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let indices = d.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("cube indices"),
            contents: bytemuck::cast_slice(&mesh.idx),
            usage: wgpu::BufferUsages::INDEX,
        });
        let uniforms = d.create_buffer(&wgpu::BufferDescriptor {
            label: Some("cube uniforms"),
            size: std::mem::size_of::<Uniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let sampler = d.create_sampler(&wgpu::SamplerDescriptor {
            address_mode_u: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            anisotropy_clamp: 8,
            ..Default::default()
        });
        let tile = texture_2d(gpu, "granite", (256, 256), wgpu::TextureFormat::R8Unorm, 1);
        upload(gpu, &tile, 0, (256, 256), 1, &decode_granite()?);
        let blank = texture_2d(gpu, "blank", (1, 1), wgpu::TextureFormat::Rgba8Unorm, 1);
        upload(gpu, &blank, 0, (1, 1), 4, &[0, 0, 0, 255]);
        Ok(CubeRenderer {
            format,
            samples,
            backdrop,
            cube,
            layout,
            sampler,
            vertices,
            indices,
            index_count: mesh.idx.len() as u32,
            reach: mesh.reach,
            uniforms,
            tile: tile.create_view(&Default::default()),
            blank: blank.create_view(&Default::default()),
            photos: vec![None; scenes::count()],
            scene: None,
            prev: None,
            fade: 1.0,
            reflection: None,
            bind: None,
            targets: None,
            transparent: true,
        })
    }

    /// Show `scene`, or none, decoding and uploading the photograph the first
    /// time it is wanted.
    pub fn set_scene(&mut self, gpu: &Gpu, scene: Option<usize>) -> Result<(), String> {
        if let Some(s) = scene {
            if s >= self.photos.len() {
                return Err(format!("there is no scene {s}"));
            }
            if self.photos[s].is_none() {
                self.photos[s] = Some(photo_view(gpu, s)?);
            }
        }
        if self.scene != scene {
            self.scene = scene;
            self.bind = None;
        }
        Ok(())
    }

    pub fn set_reflection(&mut self, reflection: Option<Arc<Reflection>>) {
        self.reflection = reflection;
        self.bind = None;
    }

    /// Blend `scene` in over `prev` (`t` from 0 to 1); `prev` none is no blend.
    fn set_fade(&mut self, prev: Option<usize>, t: f32) {
        if self.prev != prev {
            self.prev = prev;
            self.bind = None;
        }
        self.fade = if prev.is_some() { t } else { 1.0 };
    }

    fn bind_group(&mut self, gpu: &Gpu) -> wgpu::BindGroup {
        if self.bind.is_none() {
            let screen = self
                .reflection
                .as_ref()
                .map(|r| r.texture().create_view(&Default::default()));
            let mean = self
                .reflection
                .as_ref()
                .map(|r| r.mean().create_view(&Default::default()));
            self.bind = Some(
                gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("cube"),
                    layout: &self.layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: self.uniforms.as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::TextureView(
                                self.scene
                                    .and_then(|s| self.photos[s].as_ref().map(|p| &p.0))
                                    .unwrap_or(&self.blank),
                            ),
                        },
                        wgpu::BindGroupEntry {
                            binding: 2,
                            resource: wgpu::BindingResource::Sampler(&self.sampler),
                        },
                        wgpu::BindGroupEntry {
                            binding: 3,
                            resource: wgpu::BindingResource::TextureView(
                                screen.as_ref().unwrap_or(&self.blank),
                            ),
                        },
                        wgpu::BindGroupEntry {
                            binding: 4,
                            resource: wgpu::BindingResource::TextureView(&self.tile),
                        },
                        wgpu::BindGroupEntry {
                            binding: 6,
                            resource: wgpu::BindingResource::TextureView(
                                self.prev
                                    .and_then(|s| self.photos[s].as_ref().map(|p| &p.0))
                                    .unwrap_or(&self.blank),
                            ),
                        },
                        wgpu::BindGroupEntry {
                            binding: 5,
                            resource: wgpu::BindingResource::TextureView(
                                mean.as_ref().unwrap_or(&self.blank),
                            ),
                        },
                    ],
                }),
            );
        }
        self.bind.clone().unwrap()
    }

    fn targets(&mut self, gpu: &Gpu, size: (u32, u32)) -> &Targets {
        if self.targets.as_ref().is_none_or(|t| t.size != size) {
            let make = |format, label| {
                gpu.device
                    .create_texture(&wgpu::TextureDescriptor {
                        label: Some(label),
                        size: wgpu::Extent3d {
                            width: size.0,
                            height: size.1,
                            depth_or_array_layers: 1,
                        },
                        mip_level_count: 1,
                        sample_count: self.samples,
                        dimension: wgpu::TextureDimension::D2,
                        format,
                        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                        view_formats: &[],
                    })
                    .create_view(&Default::default())
            };
            self.targets = Some(Targets {
                size,
                color: make(self.format, "cube colour"),
                depth: make(DEPTH, "cube depth"),
            });
        }
        self.targets.as_ref().unwrap()
    }

    fn uniforms(&self, size: (u32, u32), scale: f64, style: &Style, motion: &Motion) -> Uniforms {
        let (w, h) = (size.0 as f32, size.1 as f32);
        let side = w.min(h) * FILL;
        let over = (self.reach * 1.02 / HALF).max(1.0);
        let half = HALF * over;
        // Model to screen: the framing's view basis, then the cube's turn.
        let turn = motion.matrix();
        let m: [V3; 3] = std::array::from_fn(|i| {
            std::array::from_fn(|j| (0..3).map(|l| turn[i][l] * VIEW[l][j]).sum())
        });
        let (sx, sy, zr) = (side / w / half, side / h / half, 2.0 * self.reach * 1.02);
        let mut mvp = [[0.0; 4]; 4];
        let mut nrm = [[0.0; 4]; 4];
        for j in 0..3 {
            mvp[j] = [m[0][j] * sx, m[1][j] * sy, -m[2][j] / zr, 0.0];
            nrm[j] = [m[0][j], m[1][j], m[2][j], 0.0];
        }
        mvp[3] = [0.0, 0.0, 0.5, 1.0];
        let ready = self.reflection.as_ref().is_some_and(|r| r.ready());
        let mode = match style.env {
            Env::Scene(s) if self.scene == Some(s) => 1.0,
            Env::Screen if ready => 2.0,
            Env::Horizon => 3.0,
            _ => 0.0,
        };
        let (ground, sky) = if style.light_look {
            ([0.42, 0.015], [0.5, 0.97])
        } else {
            ([0.55, 0.06], [0.62, 1.0])
        };
        let rgba = |c: [f32; 3], a| [c[0], c[1], c[2], a];
        Uniforms {
            mvp,
            nrm,
            look: [ground[0], ground[1], sky[0], sky[1]],
            tint: style.horizon.map_or([0.0; 4], |c| rgba(c, 1.0)),
            mode: [
                mode,
                f32::from(u8::from(self.format.is_srgb())),
                (2.0 * scale).round().max(1.0) as f32,
                0.0,
            ],
            light: rgba(style.granite[0], 1.0),
            fill: rgba(
                self.scene
                    .and_then(|s| self.photos[s].as_ref())
                    .map_or([0.0; 3], |p| p.1),
                1.0,
            ),
            dark: rgba(style.granite[1], 1.0),
            fill_prev: rgba(
                self.prev
                    .and_then(|s| self.photos[s].as_ref())
                    .map_or([0.0; 3], |p| p.1),
                self.fade,
            ),
        }
    }

    /// Record the backdrop and the cube into `enc`, resolved into `target`.
    /// One `encode` per submit: the parameters go to one shared buffer.
    #[allow(clippy::too_many_arguments)]
    pub fn encode(
        &mut self,
        gpu: &Gpu,
        enc: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        size: (u32, u32),
        scale: f64,
        style: &Style,
        motion: &Motion,
        timestamps: Option<wgpu::RenderPassTimestampWrites<'_>>,
    ) {
        let u = self.uniforms(size, scale, style, motion);
        gpu.queue
            .write_buffer(&self.uniforms, 0, bytemuck::bytes_of(&u));
        let bind = self.bind_group(gpu);
        let transparent = self.transparent;
        let multisampled = self.samples > 1;
        let targets = self.targets(gpu, size);
        let clear = if style.backdrop == Backdrop::None && !transparent {
            let [r, g, b] = style.surround.map(f64::from);
            wgpu::Color { r, g, b, a: 1.0 }
        } else {
            wgpu::Color::TRANSPARENT
        };
        let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("cube"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: if multisampled { &targets.color } else { target },
                depth_slice: None,
                resolve_target: multisampled.then_some(target),
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(clear),
                    store: wgpu::StoreOp::Discard,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &targets.depth,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(1.0),
                    store: wgpu::StoreOp::Discard,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: timestamps,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_bind_group(0, &bind, &[]);
        if style.backdrop == Backdrop::Granite {
            pass.set_pipeline(&self.backdrop);
            pass.draw(0..3, 0..1);
        }
        pass.set_pipeline(&self.cube);
        pass.set_vertex_buffer(0, self.vertices.slice(..));
        pass.set_index_buffer(self.indices.slice(..), wgpu::IndexFormat::Uint16);
        pass.draw_indexed(0..self.index_count, 0, 0..1);
    }
}

/// A texture the cube is drawn into and a buffer its pixels are copied to,
/// reused from frame to frame: how a host without a native surface gets the
/// picture.
pub struct Offscreen {
    size: (u32, u32),
    texture: wgpu::Texture,
    buffer: wgpu::Buffer,
    padded: u32,
}

impl Offscreen {
    pub fn new(gpu: &Gpu, format: wgpu::TextureFormat, size: (u32, u32)) -> Offscreen {
        let (w, h) = size;
        let padded = (w * 4).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        Offscreen {
            size,
            texture: gpu.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("cube offscreen"),
                size: wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            }),
            buffer: gpu.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("cube offscreen"),
                size: u64::from(padded) * u64::from(h),
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            }),
            padded,
        }
    }

    /// Draw the cube and copy it into `out` as tight Bgra8 rows. Waits for
    /// the GPU.
    #[allow(clippy::too_many_arguments)]
    pub fn render(
        &self,
        gpu: &Gpu,
        renderer: &mut CubeRenderer,
        scale: f64,
        style: &Style,
        motion: &Motion,
        out: &mut Vec<u8>,
    ) -> Result<(), String> {
        let (w, h) = self.size;
        let mut enc = gpu.device.create_command_encoder(&Default::default());
        renderer.encode(
            gpu,
            &mut enc,
            &self.texture.create_view(&Default::default()),
            self.size,
            scale,
            style,
            motion,
            None,
        );
        enc.copy_texture_to_buffer(
            self.texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &self.buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(self.padded),
                    rows_per_image: None,
                },
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        gpu.queue.submit([enc.finish()]);
        let (tx, rx) = std::sync::mpsc::channel();
        self.buffer
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |r| {
                let _ = tx.send(r);
            });
        gpu.device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(|e| format!("the GPU did not finish the readback: {e}"))?;
        rx.recv()
            .map_err(|e| e.to_string())?
            .map_err(|e| format!("cannot map the readback buffer: {e}"))?;
        let row = (w * 4) as usize;
        let copied = self
            .buffer
            .slice(..)
            .get_mapped_range()
            .map(|mapped| {
                out.clear();
                for line in mapped.chunks(self.padded as usize) {
                    out.extend_from_slice(&line[..row]);
                }
            })
            .map_err(|e| format!("cannot read the readback buffer: {e}"));
        self.buffer.unmap();
        copied?;
        if matches!(
            renderer.format,
            wgpu::TextureFormat::Rgba8Unorm | wgpu::TextureFormat::Rgba8UnormSrgb
        ) {
            out.as_chunks_mut::<4>()
                .0
                .iter_mut()
                .for_each(|p| p.swap(0, 2));
        }
        Ok(())
    }
}

/// Draw the cube as a view of `size` would and copy it to the CPU as tight
/// Bgra8 rows. Waits for the GPU: for self-tests, never the frame path.
pub fn render_image(
    gpu: &Gpu,
    renderer: &mut CubeRenderer,
    size: (u32, u32),
    scale: f64,
    style: &Style,
    motion: &Motion,
) -> Result<Vec<u8>, String> {
    if size.0 == 0 || size.1 == 0 {
        return Err("the view has no size".into());
    }
    let mut out = Vec::new();
    Offscreen::new(gpu, renderer.format, size)
        .render(gpu, renderer, scale, style, motion, &mut out)?;
    Ok(out)
}

/// Where a [`CubeView`] draws.
enum Output {
    Surface(Box<View>),
    /// No native surface: each frame is drawn offscreen and kept for the host
    /// to take with [`CubeView::readback`].
    Offscreen {
        target: Offscreen,
        scale: f64,
        visible: bool,
        pixels: Vec<u8>,
        presents: u64,
        draw_ns: u64,
    },
}

impl Output {
    fn size(&self) -> (u32, u32) {
        match self {
            Output::Surface(v) => v.size(),
            Output::Offscreen { target, .. } => target.size,
        }
    }

    fn scale(&self) -> f64 {
        match self {
            Output::Surface(v) => v.scale(),
            Output::Offscreen { scale, .. } => *scale,
        }
    }

    fn visible(&self) -> bool {
        match self {
            Output::Surface(v) => v.visible(),
            Output::Offscreen { visible, .. } => *visible,
        }
    }
}

/// The cube drawn into one host surface, or offscreen.
pub struct CubeView {
    out: Output,
    renderer: CubeRenderer,
    style: Style,
    motion: Motion,
    /// Something other than motion changed since the last frame.
    dirty: bool,
    last_draw: Option<Instant>,
    last_tick: Option<Instant>,
    /// The scene being faded out, and when the fade began.
    crossfade: Option<(usize, Instant)>,
}

impl CubeView {
    /// Attach to a host's raw handles, `width` by `height` physical pixels at
    /// `scale`.
    ///
    /// # Safety
    /// As [`View::attach`]: the handles stay valid until the view is dropped.
    pub unsafe fn attach(
        gpu: &Gpu,
        host: Host,
        width: u32,
        height: u32,
        scale: f64,
    ) -> Result<CubeView, String> {
        // SAFETY: the caller's promise.
        let view = unsafe { View::attach_to(gpu, host, width, height, scale, true) }?;
        let mut renderer = CubeRenderer::new(gpu, view.format())?;
        renderer.transparent = view.transparent();
        Ok(CubeView::new(Output::Surface(Box::new(view)), renderer))
    }

    /// A cube with no surface, for a toolkit that cannot host one: each
    /// [`CubeView::draw`] renders into memory and [`CubeView::readback`]
    /// returns the frame as Bgra8 rows.
    pub fn offscreen(gpu: &Gpu, width: u32, height: u32, scale: f64) -> Result<CubeView, String> {
        if width == 0 || height == 0 {
            return Err("the view has no size".into());
        }
        let format = wgpu::TextureFormat::Bgra8Unorm;
        let out = Output::Offscreen {
            target: Offscreen::new(gpu, format, (width, height)),
            scale,
            visible: true,
            pixels: Vec::new(),
            presents: 0,
            draw_ns: 0,
        };
        Ok(CubeView::new(out, CubeRenderer::new(gpu, format)?))
    }

    fn new(out: Output, renderer: CubeRenderer) -> CubeView {
        let style = Style::default();
        CubeView {
            out,
            renderer,
            style,
            motion: Motion::new(style.spin, style.reduce_motion),
            dirty: true,
            last_draw: None,
            last_tick: None,
            crossfade: None,
        }
    }

    pub fn resize(&mut self, gpu: &Gpu, width: u32, height: u32, scale: f64) {
        match &mut self.out {
            Output::Surface(v) => v.resize(gpu, width, height, scale),
            Output::Offscreen {
                target,
                scale: s,
                pixels,
                ..
            } => {
                *s = scale;
                if (width, height) != target.size && width > 0 && height > 0 {
                    *target = Offscreen::new(gpu, wgpu::TextureFormat::Bgra8Unorm, (width, height));
                    pixels.clear();
                }
            }
        }
        self.dirty = true;
    }

    /// A hidden cube draws nothing; showing it again restarts the clock so
    /// the time it was hidden does not turn it.
    pub fn set_visible(&mut self, visible: bool) {
        match &mut self.out {
            Output::Surface(v) => v.set_visible(visible),
            Output::Offscreen { visible: v, .. } => *v = visible,
        }
        self.last_tick = None;
        self.dirty = true;
    }
    pub fn set_style(&mut self, gpu: &Gpu, style: Style) {
        self.motion.configure(style.spin, style.reduce_motion);
        self.show(gpu, style.env);
        self.style = Style {
            env: self.style.env,
            ..style
        };
        self.dirty = true;
    }

    /// Switch what the chrome reflects, falling back to the studio when a
    /// photograph cannot be loaded.
    fn show(&mut self, gpu: &Gpu, env: Env) {
        let scene = match env {
            Env::Scene(s) => Some(s),
            _ => None,
        };
        let env = match self.renderer.set_scene(gpu, scene) {
            Ok(()) => env,
            Err(e) => {
                eprintln!("origami: cannot load a scene photograph: {e}");
                Env::Studio
            }
        };
        self.style.env = env;
        self.dirty = true;
    }

    /// Step to the next scene in `about.order`, fading from the one on show.
    pub fn step_scene(&mut self, gpu: &Gpu) {
        self.step_scene_at(gpu, Instant::now());
    }

    /// [`CubeView::step_scene`] at a given time, for tests that step the clock.
    pub fn step_scene_at(&mut self, gpu: &Gpu, now: Instant) {
        if self.style.env == Env::Screen {
            return;
        }
        let from = self.current_scene();
        self.show(gpu, Env::Scene(scenes::after(from)));
        self.crossfade = from
            .filter(|_| self.current_scene() != from)
            .map(|f| (f, now));
    }

    /// The photograph on show, an index into [`scenes::SCENES`].
    pub fn current_scene(&self) -> Option<usize> {
        match self.style.env {
            Env::Scene(s) => Some(s),
            _ => None,
        }
    }

    /// The running machine's picture for [`Env::Screen`], or none.
    pub fn set_reflection(&mut self, reflection: Option<Arc<Reflection>>) {
        self.renderer.set_reflection(reflection);
        self.dirty = true;
    }

    /// A pointer event at `x`, `y` points in the view, `ms` from any steady
    /// clock.
    pub fn pointer(&mut self, phase: Pointer, x: f32, y: f32, ms: u64) {
        let (w, h) = self.out.size();
        let side = w.min(h) as f32 * FILL / self.out.scale() as f32;
        match phase {
            Pointer::Press => self.motion.press(x, y, ms),
            Pointer::Move => self.motion.drag_to(x, y, ms, side),
            Pointer::Release => self.motion.release(ms),
            Pointer::DoubleClick => self.motion.go_home(),
        }
        self.dirty = true;
    }

    /// Draw a frame if the cube changed or is moving, and present it (or, for
    /// an offscreen cube, keep it for [`CubeView::readback`]). The bool says
    /// the cube is still moving: call again on the next display refresh.
    /// `false` means it is at rest and the host should stop; any call that
    /// changes the cube asks the host to draw again.
    pub fn draw(&mut self, gpu: &Gpu) -> Result<(Drawn, bool), String> {
        self.draw_at(gpu, Instant::now())
    }

    /// [`CubeView::draw`] at a given time.
    pub fn draw_at(&mut self, gpu: &Gpu, now: Instant) -> Result<(Drawn, bool), String> {
        let moving = self.motion.moving() || self.crossfade.is_some();
        let (w, h) = self.out.size();
        if !self.out.visible() || w == 0 || h == 0 {
            return Ok((Drawn::Skipped, false));
        }
        if !self.dirty && !moving {
            self.last_tick = None;
            return Ok((Drawn::Skipped, false));
        }
        let gap = if self.motion.coasting() && self.crossfade.is_none() {
            CALM_FRAME
        } else {
            MIN_FRAME
        };
        // Changes ask for a frame too, but never more than the cap allows.
        if self.last_draw.is_some_and(|t| now - t < gap) {
            return Ok((Drawn::Skipped, true));
        }
        if self.style.env == Env::Screen {
            if let Some(r) = &self.renderer.reflection {
                r.refresh(gpu);
            }
        }
        match self.crossfade {
            Some((from, began)) if (now - began) < FADE => {
                let t = (now - began).as_secs_f32() / FADE.as_secs_f32();
                self.renderer.set_fade(Some(from), t * t * (3.0 - 2.0 * t));
            }
            _ => {
                self.crossfade = None;
                self.renderer.set_fade(None, 1.0);
            }
        }
        let dt = self
            .last_tick
            .map_or(0.0, |t| (now - t).as_secs_f32().min(MAX_STEP));
        self.last_tick = Some(now);
        self.motion.step(dt);
        let scale = self.out.scale();
        let (renderer, style, motion) = (&mut self.renderer, &self.style, &self.motion);
        let drawn = match &mut self.out {
            Output::Surface(view) => view.draw_pass(
                gpu,
                false,
                |_| {},
                |_, enc, target, size| {
                    renderer.encode(gpu, enc, target, size, scale, style, motion, None)
                },
            )?,
            Output::Offscreen {
                target,
                pixels,
                presents,
                draw_ns,
                ..
            } => {
                target.render(gpu, renderer, scale, style, motion, pixels)?;
                *presents += 1;
                *draw_ns += now.elapsed().as_nanos() as u64;
                Drawn::Presented
            }
        };
        match drawn {
            // The swapchain was rebuilt or has no image just now: draw again.
            Drawn::Reconfigured | Drawn::Skipped => return Ok((drawn, true)),
            // Nothing was drawn: the change stays pending for when it shows.
            Drawn::Occluded => {
                return Ok((drawn, self.motion.moving() || self.crossfade.is_some()))
            }
            Drawn::Presented => {}
        }
        self.dirty = false;
        self.last_draw = Some(now);
        Ok((drawn, self.motion.moving() || self.crossfade.is_some()))
    }

    /// The cube as Bgra8 rows. A surface cube is drawn again offscreen as it
    /// is now; an offscreen cube returns its last frame (empty before the
    /// first).
    pub fn readback(&mut self, gpu: &Gpu) -> Result<(u32, u32, Vec<u8>), String> {
        let size = self.out.size();
        let pixels = match &self.out {
            Output::Surface(v) => render_image(
                gpu,
                &mut self.renderer,
                size,
                v.scale(),
                &self.style,
                &self.motion,
            )?,
            Output::Offscreen { pixels, .. } => pixels.clone(),
        };
        Ok((size.0, size.1, pixels))
    }

    /// An offscreen cube's last frame, moved out: Bgra8 rows, none if no frame
    /// has been drawn since the last take.
    pub fn take_frame(&mut self) -> Option<(u32, u32, Vec<u8>)> {
        let size = self.out.size();
        match &mut self.out {
            Output::Offscreen { pixels, .. } if !pixels.is_empty() => {
                Some((size.0, size.1, std::mem::take(pixels)))
            }
            _ => None,
        }
    }

    /// Frames presented, and wall nanoseconds drawing and presenting them.
    pub fn counters(&self) -> (u64, u64, u64) {
        match &self.out {
            Output::Surface(v) => (v.presents, v.draw_ns, v.present_ns),
            Output::Offscreen {
                presents, draw_ns, ..
            } => (*presents, *draw_ns, 0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gpu() -> Option<Gpu> {
        match Gpu::new() {
            Ok(g) => Some(g),
            Err(e) => {
                eprintln!("skipping: no GPU adapter in this environment ({e})");
                None
            }
        }
    }

    fn pixel(img: &[u8], w: u32, x: u32, y: u32) -> [u8; 3] {
        let o = ((y * w + x) * 4) as usize;
        [img[o + 2], img[o + 1], img[o]]
    }

    #[test]
    fn the_mesh_loads_with_its_vertices_and_faces() {
        let m = Mesh::from_glb(GLB).unwrap();
        assert_eq!(m.idx.len(), 10584);
        // A few vertices on sharp edges are split; the rest are shared.
        assert!(
            m.verts.len() >= 1764 && m.verts.len() < 2000,
            "{}",
            m.verts.len()
        );
        assert!(m.reach < 2.0 * HALF);
        assert!(Mesh::from_glb(&GLB[..100]).is_err());
    }

    #[test]
    fn normals_are_unit_and_smooth_along_the_tubes_but_split_at_sharp_edges() {
        let m = Mesh::from_glb(GLB).unwrap();
        for v in &m.verts {
            let n = unit([v[3], v[4], v[5]]);
            assert!((dot(n, n) - 1.0).abs() < 1e-3);
        }
        // Two squares meeting at a right angle share no normal at their edge.
        let pos = [
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
        ];
        let (verts, _) = smooth_split(&pos, &[0, 1, 2, 0, 3, 1]);
        assert_eq!(verts.len(), 6, "the shared edge is split");
        // A gentle bend keeps its vertices.
        let flat = [
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.5, -1.0, 0.3],
        ];
        let (verts, _) = smooth_split(&flat, &[0, 1, 2, 0, 3, 1]);
        assert_eq!(verts.len(), 4);
    }

    #[test]
    fn a_mip_chain_ends_at_one_texel_with_the_average() {
        let chain = mip_chain([255, 0, 0, 255, 0, 0, 0, 255].repeat(2), 2, 2);
        assert_eq!(chain.len(), 2);
        // Half the light of full red is 188 encoded, not 128.
        assert_eq!(chain[1].1, vec![188, 0, 0, 255]);
    }

    /// Mean error in 4 by 4 blocks of our cube against irix-theme's still of
    /// the web cube (dark chrome, mark.svg's pose), over one backdrop.
    fn error_against_still(gpu: &Gpu, style: &Style) -> f64 {
        let still = include_bytes!("../../../resources/identity/cube-dark-256.webp");
        let mut d = image_webp::WebPDecoder::new(std::io::Cursor::new(&still[..])).unwrap();
        let mut theirs = vec![0; d.output_buffer_size().unwrap()];
        d.read_image(&mut theirs).unwrap();
        let n = d.dimensions().0;
        let mut r = CubeRenderer::new(gpu, wgpu::TextureFormat::Bgra8Unorm).unwrap();
        if let Env::Scene(s) = style.env {
            r.set_scene(gpu, Some(s)).unwrap();
        }
        // The still fills its frame; our cube is FILL of the view.
        let side = (n as f32 / FILL).round() as u32;
        let ours = render_image(
            gpu,
            &mut r,
            (side, side),
            1.0,
            style,
            &Motion::new(false, false),
        )
        .unwrap();
        let (off, back) = ((side - n) / 2, [0x1fu32, 0x21, 0x26]);
        let blocks = |at: &dyn Fn(u32, u32, usize) -> u32| -> Vec<u32> {
            (0..(n / 4) * (n / 4) * 3)
                .map(|i| {
                    let (c, b) = ((i % 3) as usize, i / 3);
                    let (bx, by) = (b % (n / 4), b / (n / 4));
                    (0..16)
                        .map(|k| at(bx * 4 + k % 4, by * 4 + k / 4, c))
                        .sum::<u32>()
                        / 16
                })
                .collect()
        };
        let a = blocks(&|x, y, c| {
            let o = (((y + off) * side + x + off) * 4) as usize;
            // Premultiplied Bgra over the backdrop.
            u32::from(ours[o + 2 - c]) + back[c] * (255 - u32::from(ours[o + 3])) / 255
        });
        let b = blocks(&|x, y, c| {
            let p = &theirs[((y * n + x) * 4) as usize..];
            (u32::from(p[c]) * u32::from(p[3]) + back[c] * (255 - u32::from(p[3]))) / 255
        });
        let sum: u32 = a.iter().zip(&b).map(|(x, y)| x.abs_diff(*y)).sum();
        f64::from(sum) / a.len() as f64
    }

    #[test]
    fn the_studio_cube_matches_the_web_cubes_still() {
        let Some(gpu) = gpu() else { return };
        let mae = error_against_still(&gpu, &Style::default());
        eprintln!("MEASURE mean block error against the still: {mae:.2} of 255");
        assert!(
            mae < 12.5,
            "differs from the web cube's still: mean error {mae:.2}"
        );
        // The check can fail: a photograph is far from the still.
        let dusk = Style {
            env: Env::Scene(scenes::find("vancouver-dusk").unwrap()),
            ..Style::default()
        };
        let far = error_against_still(&gpu, &dusk);
        eprintln!("MEASURE mean block error of a photograph against the still: {far:.2}");
        assert!(far > 1.4 * mae, "{far:.2} against {mae:.2}");
    }

    /// A red patch in the environment at longitude `u` (in 32nds, 45 degrees
    /// wide, 10 to 60 above the horizon): its pixel count and mean x on the cube.
    fn marker_at(gpu: &Gpu, u: usize) -> (f64, f64) {
        let (w, h) = (256usize, 128usize);
        let mut rgba = vec![[40u8, 40, 40, 255]; w * h].concat();
        for y in (h * 3 / 16)..(h * 7 / 16) {
            for x in (w * u / 32 + w - w * 4 / 32)..(w * u / 32 + w + w * 4 / 32) {
                rgba[(y * w + x % w) * 4..][..4].copy_from_slice(&[255, 0, 0, 255]);
            }
        }
        let mut r = CubeRenderer::new(gpu, wgpu::TextureFormat::Bgra8Unorm).unwrap();
        r.photos[0] = Some(photo_from_rgba(gpu, rgba, w as u32, h as u32));
        r.scene = Some(0);
        let style = Style {
            env: Env::Scene(0),
            ..Style::default()
        };
        let side = 192usize;
        let size = (side as u32, side as u32);
        let img = render_image(gpu, &mut r, size, 1.0, &style, &Motion::new(false, false)).unwrap();
        let (mut sx, mut n) = (0.0, 0.0);
        for (i, p) in img.as_chunks::<4>().0.iter().enumerate() {
            // Bgra: red well above blue and green.
            if p[2] > 120 && p[2] > p[0].saturating_mul(2) && p[2] > p[1].saturating_mul(2) {
                sx += (i % side) as f64;
                n += 1.0;
            }
        }
        (n, sx / n.max(1.0))
    }

    /// Blender puts +X (camera right) at u = 0.5 and -X at 0, so +X reflects
    /// further right.
    #[test]
    fn a_marker_in_the_environment_reflects_where_blender_puts_it() {
        let Some(gpu) = gpu() else { return };
        let (right_n, right_x) = marker_at(&gpu, 16);
        let (left_n, left_x) = marker_at(&gpu, 0);
        assert!(
            right_n > 30.0 && left_n > 30.0,
            "a marker is not reflected: {right_n}, {left_n}"
        );
        assert!(
            right_x > left_x + 8.0,
            "+X at x {right_x}, -X at x {left_x}"
        );
    }

    /// Where the cube shows red from a machine screen that is red on its right
    /// or left half: mean x of the red pixels.
    fn screen_marker(gpu: &Gpu, red_right: bool) -> f64 {
        let update = origami_engine::FrameUpdate {
            seq: 1,
            width: 64,
            height: 48,
            runs: vec![(0, 48)],
            resized: false,
        };
        // Bgra: red, or grey.
        let pixels: Vec<u8> = (0..64 * 48)
            .flat_map(|i| {
                if (i % 64 >= 32) == red_right {
                    [0, 0, 255, 255]
                } else {
                    [40, 40, 40, 255]
                }
            })
            .collect();
        let head = crate::HeadTexture::apply(gpu, None, &update, &pixels);
        let reflection = Reflection::new(gpu);
        let mut enc = gpu.device.create_command_encoder(&Default::default());
        reflection.encode(gpu, &mut enc, &head.texture);
        gpu.queue.submit([enc.finish()]);
        reflection.done();
        let mut r = CubeRenderer::new(gpu, wgpu::TextureFormat::Bgra8Unorm).unwrap();
        r.set_reflection(Some(reflection));
        let style = Style {
            env: Env::Screen,
            ..Style::default()
        };
        let side = 192usize;
        let size = (side as u32, side as u32);
        let img = render_image(gpu, &mut r, size, 1.0, &style, &Motion::new(false, false)).unwrap();
        let (mut sx, mut n) = (0.0, 0.0);
        for (i, p) in img.as_chunks::<4>().0.iter().enumerate() {
            if p[2] > 120 && p[2] > p[0].saturating_mul(2) && p[2] > p[1].saturating_mul(2) {
                sx += (i % side) as f64;
                n += 1.0;
            }
        }
        assert!(n > 30.0, "the screen is not reflected: {n} red pixels");
        sx / n
    }

    /// The right half of the screen reflects on the cube's right, as the
    /// photographs' +X does.
    #[test]
    fn the_machines_screen_reflects_its_right_half_on_the_right() {
        let Some(gpu) = gpu() else { return };
        let (right, left) = (screen_marker(&gpu, true), screen_marker(&gpu, false));
        assert!(
            right > left + 5.0,
            "red right at x {right}, red left at x {left}"
        );
    }

    /// The horizon environment is chrome-blue overhead and takes the tint on
    /// its band; the studio is grey.
    #[test]
    fn the_horizon_environment_is_blue_above_and_takes_the_tint() {
        let Some(gpu) = gpu() else { return };
        let mut r = CubeRenderer::new(&gpu, wgpu::TextureFormat::Bgra8Unorm).unwrap();
        let draw = |r: &mut CubeRenderer, s: Style| {
            render_image(&gpu, r, (128, 128), 1.0, &s, &Motion::new(false, false)).unwrap()
        };
        // Bgra: blue minus red over the cube's opaque pixels.
        let blueness = |img: &[u8]| {
            img.as_chunks::<4>()
                .0
                .iter()
                .filter(|p| p[3] == 255)
                .map(|p| i64::from(p[0]) - i64::from(p[2]))
                .sum::<i64>()
        };
        let studio = draw(&mut r, Style::default());
        let plain = Style {
            env: Env::Horizon,
            ..Style::default()
        };
        let horizon = draw(&mut r, plain);
        assert!(blueness(&horizon) > blueness(&studio) + 2000);
        let tinted = draw(
            &mut r,
            Style {
                horizon: Some([0.9, 0.1, 0.1]),
                ..plain
            },
        );
        assert_ne!(tinted, horizon, "the colour reaches the band");
        assert!(blueness(&tinted) < blueness(&horizon), "and it is red");
    }

    #[test]
    fn the_cube_covers_the_middle_over_granite_and_turns() {
        let Some(gpu) = gpu() else { return };
        let mut r = CubeRenderer::new(&gpu, wgpu::TextureFormat::Bgra8Unorm).unwrap();
        let (w, h) = (128, 96);
        let style = Style {
            backdrop: Backdrop::Granite,
            ..Style::default()
        };
        let mut motion = Motion::new(true, false);
        let a = render_image(&gpu, &mut r, (w, h), 1.0, &style, &motion).unwrap();
        // The corner is granite: one of its two tones.
        let tone = |c: [f32; 3]| c.map(|v| (v * 255.0).round() as u8);
        let granite = [tone(style.granite[0]), tone(style.granite[1])];
        assert!(granite.contains(&pixel(&a, w, 0, 0)));
        let cover = (0..h)
            .flat_map(|y| (0..w).map(move |x| (x, y)))
            .filter(|&(x, y)| !granite.contains(&pixel(&a, w, x, y)))
            .count();
        assert!(
            cover > (w * h) as usize / 5,
            "the cube covers {cover} pixels"
        );
        for _ in 0..30 {
            motion.step(0.05);
        }
        let b = render_image(&gpu, &mut r, (w, h), 1.0, &style, &motion).unwrap();
        assert_ne!(a, b);
        gpu.device.poll(wgpu::PollType::wait_indefinitely()).ok();
        assert_eq!(gpu.errors.count(), 0);
    }

    #[test]
    fn with_no_backdrop_the_view_is_transparent_around_the_cube() {
        let Some(gpu) = gpu() else { return };
        let mut r = CubeRenderer::new(&gpu, wgpu::TextureFormat::Bgra8Unorm).unwrap();
        let (w, h) = (96, 96);
        let motion = Motion::new(false, false);
        let img = render_image(&gpu, &mut r, (w, h), 1.0, &Style::default(), &motion).unwrap();
        // Bgra, premultiplied: the corner has no colour and no alpha, the
        // cube is opaque somewhere.
        assert_eq!(&img[..4], &[0, 0, 0, 0]);
        assert!(img.as_chunks::<4>().0.iter().any(|p| p[3] == 255));
        // Where the window system gives no alpha, the host's background fills.
        r.transparent = false;
        let style = Style {
            surround: [1.0, 0.0, 0.0],
            ..Style::default()
        };
        let img = render_image(&gpu, &mut r, (w, h), 1.0, &style, &motion).unwrap();
        assert_eq!(pixel(&img, w, 0, 0), [255, 0, 0]);
    }

    #[test]
    fn the_horizon_tint_reaches_the_chrome_and_the_photo_replaces_the_studio() {
        let Some(gpu) = gpu() else { return };
        let mut r = CubeRenderer::new(&gpu, wgpu::TextureFormat::Bgra8Unorm).unwrap();
        r.set_scene(&gpu, scenes::find("garibaldi-lake")).unwrap();
        let (w, h) = (96, 96);
        let motion = Motion::new(false, false);
        let plain = Style::default();
        let tinted = Style {
            horizon: Some([0.0, 0.8, 0.0]),
            ..plain
        };
        let photo = Style {
            env: Env::Scene(scenes::find("garibaldi-lake").unwrap()),
            ..plain
        };
        let draw = |r: &mut CubeRenderer, s: &Style| {
            render_image(&gpu, r, (w, h), 1.0, s, &motion).unwrap()
        };
        let studio = draw(&mut r, &plain);
        let green = |img: &[u8]| {
            img.as_chunks::<4>()
                .0
                .iter()
                .filter(|p| p[1] > p[0].saturating_add(30) && p[1] > p[2].saturating_add(30))
                .count()
        };
        assert!(green(&draw(&mut r, &tinted)) > green(&studio));
        let blue = draw(&mut r, &photo);
        assert_ne!(blue, studio);
        // Glacial water and snow: the photo's chrome is bluer than the studio's grey.
        // The rows are Bgra, so blue minus red is byte 0 minus byte 2.
        let blueness = |img: &[u8]| {
            img.as_chunks::<4>()
                .0
                .iter()
                .map(|p| i64::from(p[0]) - i64::from(p[2]))
                .sum::<i64>()
        };
        assert!(blueness(&blue) > blueness(&studio));
    }

    /// GPU time of the cube pass, from timestamp queries. Prints MEASURE
    /// lines: `cargo test -p origami-render --release gpu_time -- --ignored --nocapture`.
    #[test]
    #[ignore = "measures; needs a GPU with timestamp queries"]
    fn gpu_time_per_frame() {
        let Some(gpu) = gpu() else { return };
        if !gpu
            .device
            .features()
            .contains(wgpu::Features::TIMESTAMP_QUERY)
        {
            eprintln!("skipping: no timestamp queries");
            return;
        }
        let mut r = CubeRenderer::new(&gpu, wgpu::TextureFormat::Bgra8Unorm).unwrap();
        r.set_scene(&gpu, scenes::find("garibaldi-lake")).unwrap();
        let queries = gpu.device.create_query_set(&wgpu::QuerySetDescriptor {
            label: None,
            ty: wgpu::QueryType::Timestamp,
            count: 2,
        });
        let buffer = |usage| {
            gpu.device.create_buffer(&wgpu::BufferDescriptor {
                label: None,
                size: 16,
                usage,
                mapped_at_creation: false,
            })
        };
        let resolve = buffer(wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC);
        let read = buffer(wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ);
        let period = f64::from(gpu.queue.get_timestamp_period());
        let mut motion = Motion::new(true, false);
        for (name, side, scale, env) in [
            ("96 pt at 2x, studio", 192, 2.0, Env::Studio),
            (
                "160 pt at 2x, scene",
                320,
                2.0,
                Env::Scene(scenes::find("garibaldi-lake").unwrap()),
            ),
            (
                "160 pt at 1x, scene",
                160,
                1.0,
                Env::Scene(scenes::find("garibaldi-lake").unwrap()),
            ),
        ] {
            let target = gpu.device.create_texture(&wgpu::TextureDescriptor {
                label: None,
                size: wgpu::Extent3d {
                    width: side,
                    height: side,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Bgra8Unorm,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            });
            let style = Style {
                env,
                ..Style::default()
            };
            let mut us = Vec::new();
            for _ in 0..300 {
                motion.step(0.016);
                let mut enc = gpu.device.create_command_encoder(&Default::default());
                r.encode(
                    &gpu,
                    &mut enc,
                    &target.create_view(&Default::default()),
                    (side, side),
                    scale,
                    &style,
                    &motion,
                    Some(wgpu::RenderPassTimestampWrites {
                        query_set: &queries,
                        beginning_of_pass_write_index: Some(0),
                        end_of_pass_write_index: Some(1),
                    }),
                );
                enc.resolve_query_set(&queries, 0..2, &resolve, 0);
                enc.copy_buffer_to_buffer(&resolve, 0, &read, 0, 16);
                gpu.queue.submit([enc.finish()]);
                read.slice(..).map_async(wgpu::MapMode::Read, |_| {});
                gpu.device.poll(wgpu::PollType::wait_indefinitely()).ok();
                let t: Vec<u64> =
                    bytemuck::pod_collect_to_vec(&read.slice(..).get_mapped_range().unwrap());
                read.unmap();
                us.push((t[1] - t[0]) as f64 * period / 1e3);
            }
            us.sort_by(|a, b| a.partial_cmp(b).unwrap());
            eprintln!(
                "MEASURE GPU {name}: median {:.0} us, p95 {:.0} us, max {:.0} us per frame over 300 frames",
                us[150], us[285], us[299]
            );
        }
    }

    #[test]
    fn the_cube_is_smoother_at_its_edges_than_without_multisampling() {
        let Some(gpu) = gpu() else { return };
        let mut r = CubeRenderer::new(&gpu, wgpu::TextureFormat::Bgra8Unorm).unwrap();
        let (levels, err) = aliasing(&gpu, &mut r, &Style::default(), 160);
        eprintln!(
            "MEASURE silhouette levels {levels}, error {err:.3}, samples {}",
            r.samples
        );
        // Hard-edged rendering has no partial coverage at all.
        assert!(
            r.samples == 1 || levels >= (r.samples - 1) as usize,
            "{levels} levels"
        );
        assert!(err < 4.5, "mean error against 4x supersampling {err:.3}");
    }

    /// Distinct partial-coverage levels along the silhouette, and the mean error
    /// of a 1x render against the same cube drawn at 4x and box-filtered down.
    fn aliasing(gpu: &Gpu, r: &mut CubeRenderer, style: &Style, side: u32) -> (usize, f64) {
        let m = Motion::new(false, false);
        let one = render_image(gpu, r, (side, side), 1.0, style, &m).unwrap();
        let big = render_image(gpu, r, (side * 4, side * 4), 4.0, style, &m).unwrap();
        let levels: std::collections::BTreeSet<u8> = one
            .as_chunks::<4>()
            .0
            .iter()
            .map(|p| p[3])
            .filter(|&a| a > 0 && a < 255)
            .collect();
        let mut sum = 0u64;
        for y in 0..side {
            for x in 0..side {
                for c in 0..4 {
                    let mut acc = 0u32;
                    for k in 0..16 {
                        let (bx, by) = (x * 4 + k % 4, y * 4 + k / 4);
                        acc += u32::from(big[((by * side * 4 + bx) * 4 + c) as usize]);
                    }
                    let want = (acc as f64 / 16.0).round() as i64;
                    sum +=
                        (i64::from(one[((y * side + x) * 4 + c) as usize]) - want).unsigned_abs();
                }
            }
        }
        (levels.len(), sum as f64 / f64::from(side * side * 4))
    }

    /// Night 21 to 7, day 7 to 17, dusk 19 to 21, sunset 17 to 19, and a
    /// second day scene, in a shuffled file order.
    const FOUR: [(f32, f32); 5] = [
        (21.0, 7.0),
        (7.0, 17.0),
        (19.0, 21.0),
        (17.0, 19.0),
        (7.0, 17.0),
    ];

    #[test]
    fn the_time_of_day_picks_its_scene_at_the_boundaries() {
        let pick = |h: u32, m: u32| scenes::pick(&FOUR, h as f32 + m as f32 / 60.0);
        let (night, day, dusk, sunset) = (0, 1, 2, 3);
        assert_eq!(pick(6, 59), night);
        assert_eq!(pick(7, 0), day, "a range starts at its first minute");
        assert_eq!(pick(16, 59), day);
        assert_eq!(pick(17, 0), sunset);
        assert_eq!(pick(18, 59), sunset);
        assert_eq!(pick(19, 0), dusk);
        assert_eq!(pick(20, 59), dusk);
        assert_eq!(pick(21, 0), night);
        assert_eq!(pick(0, 0), night, "night wraps past midnight");
        assert_eq!(pick(12, 0), day, "the first of two day scenes wins");
        // A gap in the hours: the scene that began last.
        let two = [(7.0, 17.0), (19.0, 21.0)];
        assert_eq!(scenes::pick(&two, 17.5), 0);
        assert_eq!(scenes::pick(&two, 22.0), 1);
        assert_eq!(scenes::pick(&two, 3.0), 1);
    }

    #[test]
    fn the_manifest_maps_each_time_of_day_to_its_scene() {
        let at = |h, m| scenes::SCENES[scenes::for_time(h, m)].id;
        assert_eq!(at(4, 59), "lions-gate-night");
        assert_eq!(at(5, 0), "okanagan-dawn");
        assert_eq!(at(6, 59), "okanagan-dawn");
        assert_eq!(at(7, 0), "howe-sound");
        assert_eq!(at(15, 59), "howe-sound");
        assert_eq!(at(16, 0), "english-bay-golden");
        assert_eq!(at(17, 59), "english-bay-golden");
        assert_eq!(
            at(18, 0),
            "lions-gate-sunset",
            "ties go to the first in file order"
        );
        assert_eq!(at(19, 0), "vancouver-dusk");
        assert_eq!(at(20, 59), "vancouver-dusk");
        assert_eq!(at(21, 0), "lions-gate-night");
        assert_eq!(at(0, 0), "lions-gate-night");
    }

    #[test]
    fn stepping_goes_through_every_scene_once_in_the_manifests_order() {
        assert_eq!(scenes::next_in(&[3, 1, 2], Some(2)), 3);
        let mut at = None;
        let walk: Vec<usize> = (0..scenes::count() + 1)
            .map(|_| {
                at = Some(scenes::after(at));
                at.unwrap()
            })
            .collect();
        let mut once = walk[..scenes::count()].to_vec();
        once.sort_unstable();
        assert_eq!(once, (0..scenes::count()).collect::<Vec<_>>());
        assert_eq!(walk[0], walk[scenes::count()], "round to where it began");
        // Time of day order: dawn, day, golden hour, sunset, dusk, night.
        let times: Vec<&str> = scenes::ORDER
            .iter()
            .map(|&i| scenes::SCENES[i].time)
            .collect();
        let want = [
            "dawn", "day", "day", "day", "day", "day", "golden", "sunset", "sunset", "dusk",
            "night",
        ];
        assert_eq!(times, want);
    }

    fn frame_of(v: &mut CubeView, gpu: &Gpu, at: Instant) -> (Vec<u8>, bool) {
        let moving = v.draw_at(gpu, at).unwrap().1;
        (v.readback(gpu).unwrap().2, moving)
    }

    #[test]
    fn stepping_fades_from_one_scene_to_the_next_and_stops() {
        let Some(gpu) = gpu() else { return };
        let mut v = CubeView::offscreen(&gpu, 96, 96, 1.0).unwrap();
        let day = scenes::ORDER[0];
        v.set_style(
            &gpu,
            Style {
                env: Env::Scene(day),
                ..Style::default()
            },
        );
        // The clock is stepped by hand, so a loaded machine cannot skip the fade.
        let t0 = Instant::now();
        let frame = |v: &mut CubeView, ms: u64| frame_of(v, &gpu, t0 + Duration::from_millis(ms));
        let (first, moving) = frame(&mut v, 0);
        assert!(!moving, "a still scene is at rest");
        v.step_scene_at(&gpu, t0 + Duration::from_millis(100));
        assert_eq!(v.current_scene(), Some(scenes::ORDER[1]));
        let (mid, moving) = frame(&mut v, 130);
        assert!(moving, "a fade keeps the host drawing");
        let (half, _) = frame(&mut v, 300);
        let (last, moving) = frame(&mut v, 700);
        assert!(!moving, "the fade ends");
        let far = |a: &[u8], b: &[u8]| {
            a.iter()
                .zip(b)
                .map(|(x, y)| u64::from(x.abs_diff(*y)))
                .sum::<u64>()
        };
        // Halfway is between: neither the old picture nor the new one.
        assert!(far(&first, &last) > 0);
        assert!(far(&half, &first) > 0 && far(&half, &last) > 0);
        assert!(
            far(&mid, &first) < far(&half, &first),
            "the fade starts near the old scene"
        );
        // A finished fade is the new scene alone, as a fresh cube draws it.
        let mut fresh = CubeView::offscreen(&gpu, 96, 96, 1.0).unwrap();
        fresh.set_style(
            &gpu,
            Style {
                env: Env::Scene(scenes::ORDER[1]),
                ..Style::default()
            },
        );
        let (alone, _) = frame_of(&mut fresh, &gpu, t0 + Duration::from_millis(900));
        assert_eq!(last, alone, "the faded-in scene matches a fresh render");
    }
}
