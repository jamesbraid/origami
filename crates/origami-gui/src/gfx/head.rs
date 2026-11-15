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
        let rows = update.y1 - update.y0;
        if rows > 0 && pixels.len() == rows as usize * update.width as usize * 4 {
            gpu.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &h.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: 0,
                        y: update.y0,
                        z: 0,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                pixels,
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
            h.bytes_uploaded += pixels.len() as u64;
        }
        h.seq = update.seq;
        h
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gfx::{Blit, Presenter, ScaleMode};

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
            y0,
            y1,
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
        // The size changes: a new texture of the new size, a new generation.
        let t = HeadTexture::apply(&gpu, Some(t), &update(16, 8, 0, 8, 3), &px(8, 16));
        assert_eq!(
            (t.texture.width(), t.texture.height(), t.generation),
            (16, 8, 2)
        );
        assert_eq!(t.rows_uploaded, 14);
        // Pixels that do not match the rows named are not uploaded.
        let t = HeadTexture::apply(&gpu, Some(t), &update(16, 8, 0, 4, 4), &px(1, 16));
        assert_eq!((t.rows_uploaded, t.seq), (14, 4));
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
            !pres.run(&gpu, &blit, &head, ScaleMode::Integer),
            "no output yet"
        );
        assert!(pres.resize(&gpu, 64, 32).is_some());
        assert!(
            pres.resize(&gpu, 64, 32).is_none(),
            "same area, same texture"
        );
        assert!(pres.run(&gpu, &blit, &head, ScaleMode::Integer));
        assert!(
            !pres.run(&gpu, &blit, &head, ScaleMode::Integer),
            "idle: nothing to do"
        );
        assert!(
            pres.run(&gpu, &blit, &head, ScaleMode::Fit),
            "a new mode draws again"
        );
        let next = HeadTexture::apply(&gpu, Some(head), &update(8, 4, 0, 2, 2), &px[..8 * 2 * 4]);
        assert!(
            pres.run(&gpu, &blit, &next, ScaleMode::Fit),
            "a new frame draws again"
        );
        let t = pres
            .resize(&gpu, 80, 40)
            .expect("a new area makes a new output");
        assert_eq!((t.width(), t.height()), (80, 40));
        assert!(
            pres.run(&gpu, &blit, &next, ScaleMode::Fit),
            "and draws into it"
        );
        assert_eq!((pres.passes, pres.skipped), (4, 1));
        gpu.device.poll(wgpu::PollType::wait_indefinitely()).ok();
        assert_eq!(gpu.errors.count(), 0);
    }
}
