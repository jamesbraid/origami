use super::Gpu;
use crate::core::FrameUpdate;

/// A head's picture on the GPU. It belongs to the device, not to a window,
/// so a machine can move between windows without copying anything.
pub struct HeadTexture {
    pub texture: wgpu::Texture,
    pub width: u32,
    pub height: u32,
    /// Moves whenever the picture changes, here or in the engine's own count.
    pub seq: u64,
    /// Moves when `texture` is replaced.
    pub generation: u64,
    pub rows_uploaded: u64,
    pub bytes_uploaded: u64,
}

fn create(gpu: &Gpu, width: u32, height: u32) -> wgpu::Texture {
    gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("head"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        // QEMU's pixels are blue, green, red, unused: sampled as they are.
        format: wgpu::TextureFormat::Bgra8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    })
}

impl HeadTexture {
    /// Apply an update to `current` (or make the first texture). Only the
    /// rows `update` names are written, unless the size changed and the
    /// texture is replaced.
    pub fn apply(
        gpu: &Gpu,
        current: Option<HeadTexture>,
        update: &FrameUpdate,
        pixels: &[u8],
    ) -> HeadTexture {
        let mut h = match current {
            Some(h) if (h.width, h.height) == (update.width, update.height) => h,
            old => HeadTexture {
                texture: create(gpu, update.width, update.height),
                width: update.width,
                height: update.height,
                seq: 0,
                generation: old.as_ref().map_or(1, |o| o.generation + 1),
                rows_uploaded: old.as_ref().map_or(0, |o| o.rows_uploaded),
                bytes_uploaded: old.as_ref().map_or(0, |o| o.bytes_uploaded),
            },
        };
        let mut at = 0usize;
        for &(y0, y1) in &update.runs {
            let rows = y1 - y0;
            let bytes = rows as usize * update.width as usize * 4;
            if rows == 0 || y1 > update.height || at + bytes > pixels.len() {
                break;
            }
            gpu.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &h.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d { x: 0, y: y0, z: 0 },
                    aspect: wgpu::TextureAspect::All,
                },
                &pixels[at..at + bytes],
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(update.width * 4),
                    rows_per_image: None,
                },
                wgpu::Extent3d {
                    width: update.width,
                    height: rows,
                    depth_or_array_layers: 1,
                },
            );
            h.rows_uploaded += u64::from(rows);
            h.bytes_uploaded += bytes as u64;
            at += bytes;
        }
        h.seq = update.seq;
        h
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gfx::{cursor_texture, Blit, CursorDraw, Presenter, ScaleMode};

    /// A GPU for the tests, or None where the machine has no adapter at all
    /// (a software one is enough).
    fn gpu() -> Option<Gpu> {
        match Gpu::new() {
            Ok(g) => Some(g),
            Err(e) => {
                eprintln!("skipping: {e}");
                None
            }
        }
    }

    fn update(w: u32, h: u32, y0: u32, y1: u32, seq: u64) -> FrameUpdate {
        FrameUpdate {
            seq,
            width: w,
            height: h,
            runs: vec![(y0, y1)],
            resized: false,
        }
    }

    #[test]
    fn rows_land_in_the_texture_and_a_resize_replaces_it() {
        let Some(gpu) = gpu() else { return };
        let px = |rows: u32, w: u32| vec![0x40u8; (rows * w * 4) as usize];
        let t = HeadTexture::apply(&gpu, None, &update(8, 4, 0, 4, 1), &px(4, 8));
        assert_eq!(
            (t.texture.width(), t.texture.height(), t.generation),
            (8, 4, 1)
        );
        assert_eq!((t.rows_uploaded, t.seq), (4, 1));
        // Two rows change: same texture, two more rows uploaded.
        let t = HeadTexture::apply(&gpu, Some(t), &update(8, 4, 1, 3, 2), &px(2, 8));
        assert_eq!((t.generation, t.rows_uploaded, t.seq), (1, 6, 2));
        // Two runs in one update: both are uploaded from the one buffer.
        let two = FrameUpdate {
            seq: 3,
            width: 8,
            height: 4,
            runs: vec![(0, 1), (2, 4)],
            resized: false,
        };
        let t = HeadTexture::apply(&gpu, Some(t), &two, &px(3, 8));
        assert_eq!((t.rows_uploaded, t.seq), (9, 3));
        // The size changes: a new texture of the new size, a new generation.
        let t = HeadTexture::apply(&gpu, Some(t), &update(16, 8, 0, 8, 3), &px(8, 16));
        assert_eq!(
            (t.texture.width(), t.texture.height(), t.generation),
            (16, 8, 2)
        );
        assert_eq!(t.rows_uploaded, 17);
        // Pixels that do not match the rows named are not uploaded.
        let t = HeadTexture::apply(&gpu, Some(t), &update(16, 8, 0, 4, 4), &px(1, 16));
        assert_eq!((t.rows_uploaded, t.seq), (17, 4));
        gpu.device.poll(wgpu::PollType::wait_indefinitely()).ok();
        assert_eq!(gpu.errors.count(), 0);
    }

    #[test]
    fn the_pass_runs_only_when_the_picture_or_mode_changed() {
        let Some(gpu) = gpu() else { return };
        let blit = Blit::new(&gpu);
        let mut pres = Presenter::default();
        let px = vec![0x80u8; 8 * 4 * 4];
        let head = HeadTexture::apply(&gpu, None, &update(8, 4, 0, 4, 1), &px);
        assert!(
            !pres.run(&gpu, &blit, &head, ScaleMode::Integer, None),
            "no output yet"
        );
        assert!(pres.resize(&gpu, 64, 32).is_some());
        assert!(
            pres.resize(&gpu, 64, 32).is_none(),
            "same area, same texture"
        );
        assert!(pres.run(&gpu, &blit, &head, ScaleMode::Integer, None));
        assert!(
            !pres.run(&gpu, &blit, &head, ScaleMode::Integer, None),
            "idle: nothing to do"
        );
        assert!(
            pres.run(&gpu, &blit, &head, ScaleMode::Fit, None),
            "a new mode draws again"
        );
        let next = HeadTexture::apply(&gpu, Some(head), &update(8, 4, 0, 2, 2), &px[..8 * 2 * 4]);
        assert!(
            pres.run(&gpu, &blit, &next, ScaleMode::Fit, None),
            "a new frame draws again"
        );
        let t = pres
            .resize(&gpu, 80, 40)
            .expect("a new area makes a new output");
        assert_eq!((t.width(), t.height()), (80, 40));
        assert!(
            pres.run(&gpu, &blit, &next, ScaleMode::Fit, None),
            "and draws into it"
        );
        assert_eq!((pres.passes, pres.skipped), (4, 1));
        gpu.device.poll(wgpu::PollType::wait_indefinitely()).ok();
        assert_eq!(gpu.errors.count(), 0);
    }

    /// The output texture's pixels, row by row.
    fn read_back(gpu: &Gpu, t: &wgpu::Texture) -> Vec<u8> {
        let (w, h) = (t.width(), t.height());
        let buf = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: u64::from(w * 4 * h),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = gpu.device.create_command_encoder(&Default::default());
        enc.copy_texture_to_buffer(
            t.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buf,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(w * 4),
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
        buf.slice(..).map_async(wgpu::MapMode::Read, |_| {});
        gpu.device.poll(wgpu::PollType::wait_indefinitely()).ok();
        let data = buf.slice(..).get_mapped_range().expect("mapped").to_vec();
        data
    }

    #[test]
    fn the_guest_cursor_is_drawn_into_the_picture_and_moving_it_draws_again() {
        let Some(gpu) = gpu() else { return };
        let blit = Blit::new(&gpu);
        let mut pres = Presenter::default();
        // An 8x4 grey picture shown 8 times larger.
        let px = vec![0x40u8; 8 * 4 * 4];
        let head = HeadTexture::apply(&gpu, None, &update(8, 4, 0, 4, 1), &px);
        pres.resize(&gpu, 64, 32);
        // A 2x2 opaque white cursor, in the 64x64 texture's top left.
        let ct = cursor_texture(&gpu);
        let white = vec![0xFFu8; 2 * 4];
        for y in 0..2 {
            gpu.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &ct,
                    mip_level: 0,
                    origin: wgpu::Origin3d { x: 0, y, z: 0 },
                    aspect: wgpu::TextureAspect::All,
                },
                &white,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(8),
                    rows_per_image: None,
                },
                wgpu::Extent3d {
                    width: 2,
                    height: 1,
                    depth_or_array_layers: 1,
                },
            );
        }
        let at = |x| CursorDraw {
            texture: &ct,
            x,
            y: 1,
            width: 2,
            height: 2,
            gen: 1,
        };
        assert!(pres.run(&gpu, &blit, &head, ScaleMode::Integer, None));
        assert!(pres.run(&gpu, &blit, &head, ScaleMode::Integer, Some(&at(2))));
        assert!(
            !pres.run(&gpu, &blit, &head, ScaleMode::Integer, Some(&at(2))),
            "a still cursor draws nothing"
        );
        let img = read_back(&gpu, pres.out_texture().unwrap());
        let px_at = |x: usize, y: usize| &img[(y * 64 + x) * 4..(y * 64 + x) * 4 + 3];
        // Head pixel (2,1) is output (16..24, 8..16): white there, grey beside it.
        assert_eq!(px_at(20, 12), [255, 255, 255]);
        assert_eq!(px_at(4, 4), [0x40, 0x40, 0x40]);
        assert!(
            pres.run(&gpu, &blit, &head, ScaleMode::Integer, Some(&at(5))),
            "a moved cursor draws again"
        );
        let img = read_back(&gpu, pres.out_texture().unwrap());
        let px_at = |x: usize, y: usize| &img[(y * 64 + x) * 4..(y * 64 + x) * 4 + 3];
        assert_eq!(px_at(20, 12), [0x40, 0x40, 0x40], "gone from the old place");
        assert_eq!(px_at(44, 12), [255, 255, 255]);
        assert!(
            pres.run(&gpu, &blit, &head, ScaleMode::Integer, None),
            "hiding it draws again"
        );
        assert_eq!(gpu.errors.count(), 0);
    }
}
