//! Surface attach: a native host (a Cocoa view, a Win32 window, an X11 or
//! Wayland surface) gives the renderer a window handle, and a head is drawn
//! straight into that window's swapchain. The Slint path instead renders into
//! a texture Slint owns ([`crate::Presenter`]); both run the same pass.
//!
//! The API is plain values in, plain values out, so a C function table can
//! wrap it one function to one method: create is [`View::attach`], resize is
//! [`View::resize`], present is [`View::draw`], destroy is dropping the view.
//! A view does no work of its own: it never spawns a thread and draws only
//! when the host asks, so a hidden view costs nothing per frame.

use crate::{Blit, CursorDraw, Gpu, HeadTexture, ScaleMode};
use raw_window_handle::{RawDisplayHandle, RawWindowHandle};

/// What a host gives the renderer to draw into.
#[derive(Clone, Copy, Debug)]
pub struct HostSurface {
    /// The display connection, where the platform has one the surface needs
    /// (Wayland, X11). `None` for Cocoa (`CAMetalLayer`'s `NSView`) and Win32.
    pub display: Option<RawDisplayHandle>,
    /// The window: an `NSView` or `CAMetalLayer` pointer, an `HWND`, an X11
    /// window or a `wl_surface`.
    pub window: RawWindowHandle,
}

/// How a [`View::draw`] ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Drawn {
    /// The head was drawn and presented.
    Presented,
    /// Nothing was drawn: the view is hidden, has no size, or the platform
    /// has no image to give just now (a minimised or covered window). The
    /// caller draws again when it next has something to show.
    Skipped,
    /// The swapchain no longer matched the window and was rebuilt. Nothing
    /// was presented; draw again.
    Reconfigured,
}

/// A head drawn into one host surface.
pub struct View {
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    blit: Blit,
    scale: f64,
    visible: bool,
    /// Presents since attach, for the host's statistics.
    pub presents: u64,
}

impl View {
    /// Attach to a host's raw handles, sized `width` by `height` physical
    /// pixels at `scale` (physical per logical, 2.0 on a Retina display).
    ///
    /// # Safety
    /// `host`'s handles must be valid, and stay valid, until the returned
    /// view is dropped. The renderer holds no reference to the window, so the
    /// host owns that ordering.
    pub unsafe fn attach(
        gpu: &Gpu,
        host: HostSurface,
        width: u32,
        height: u32,
        scale: f64,
    ) -> Result<View, String> {
        // SAFETY: the caller's promise above.
        let surface = unsafe {
            gpu.instance
                .create_surface_unsafe(wgpu::SurfaceTargetUnsafe::RawHandle {
                    raw_display_handle: host.display,
                    raw_window_handle: host.window,
                })
        }
        .map_err(|e| format!("cannot create a surface: {e}"))?;
        View::from_surface(gpu, surface, width, height, scale)
    }

    /// Attach to a window object that keeps itself alive as long as the
    /// surface needs it (an `Arc` of a winit window, say).
    pub fn attach_window(
        gpu: &Gpu,
        window: impl Into<wgpu::SurfaceTarget<'static>>,
        width: u32,
        height: u32,
        scale: f64,
    ) -> Result<View, String> {
        let surface = gpu
            .instance
            .create_surface(window)
            .map_err(|e| format!("cannot create a surface: {e}"))?;
        View::from_surface(gpu, surface, width, height, scale)
    }

    fn from_surface(
        gpu: &Gpu,
        surface: wgpu::Surface<'static>,
        width: u32,
        height: u32,
        scale: f64,
    ) -> Result<View, String> {
        if !gpu.adapter.is_surface_supported(&surface) {
            return Err(format!(
                "the GPU ({}) cannot draw to this surface",
                gpu.describe()
            ));
        }
        let caps = surface.get_capabilities(&gpu.adapter);
        let format = pick_format(&caps.formats).ok_or("the surface offers no pixel format")?;
        let mut config = surface
            .get_default_config(&gpu.adapter, width.max(1), height.max(1))
            .ok_or("the surface has no usable configuration")?;
        config.format = format;
        config.usage = wgpu::TextureUsages::RENDER_ATTACHMENT;
        let mut view = View {
            surface,
            config,
            blit: Blit::with_format(gpu, format),
            scale,
            visible: true,
            presents: 0,
        };
        view.resize(gpu, width, height, scale);
        Ok(view)
    }

    /// The window changed size or moved to a display of another scale. A
    /// zero size (a minimised window) is remembered and draws nothing until a
    /// real one arrives.
    pub fn resize(&mut self, gpu: &Gpu, width: u32, height: u32, scale: f64) {
        self.scale = scale;
        if width == 0 || height == 0 {
            self.config.width = 0;
            self.config.height = 0;
            return;
        }
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&gpu.device, &self.config);
    }

    /// A hidden view draws nothing and takes no image from the swapchain.
    pub fn set_visible(&mut self, visible: bool) {
        self.visible = visible;
    }

    pub fn visible(&self) -> bool {
        self.visible
    }

    /// Physical pixels per logical pixel, as last given.
    pub fn scale(&self) -> f64 {
        self.scale
    }

    /// The surface size in physical pixels.
    pub fn size(&self) -> (u32, u32) {
        (self.config.width, self.config.height)
    }

    /// Draw `head` into the window and present it. The host calls this when
    /// the head changed, or after a resize, an expose or a [`Drawn::Reconfigured`]:
    /// a swapchain image is not kept between calls, so each call redraws.
    pub fn draw(
        &mut self,
        gpu: &Gpu,
        head: &HeadTexture,
        mode: ScaleMode,
        cursor: Option<&CursorDraw>,
    ) -> Result<Drawn, String> {
        if !self.visible || self.config.width == 0 || self.config.height == 0 {
            return Ok(Drawn::Skipped);
        }
        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f) => f,
            // Usable now; the swapchain is rebuilt before the next one.
            wgpu::CurrentSurfaceTexture::Suboptimal(f) => {
                self.draw_into(gpu, &f, head, mode, cursor);
                gpu.queue.present(f);
                self.presents += 1;
                self.surface.configure(&gpu.device, &self.config);
                return Ok(Drawn::Presented);
            }
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                return Ok(Drawn::Skipped);
            }
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.surface.configure(&gpu.device, &self.config);
                return Ok(Drawn::Reconfigured);
            }
            wgpu::CurrentSurfaceTexture::Validation => {
                return Err("the surface reported a validation error".into());
            }
        };
        self.draw_into(gpu, &frame, head, mode, cursor);
        gpu.queue.present(frame);
        self.presents += 1;
        Ok(Drawn::Presented)
    }

    fn draw_into(
        &self,
        gpu: &Gpu,
        frame: &wgpu::SurfaceTexture,
        head: &HeadTexture,
        mode: ScaleMode,
        cursor: Option<&CursorDraw>,
    ) {
        let target = frame.texture.create_view(&Default::default());
        self.blit.draw(
            gpu,
            &target,
            (self.config.width, self.config.height),
            head,
            mode,
            cursor,
        );
    }
}

/// The surface format to draw into: a non-sRGB one, because the pass writes
/// the guest's pixel values as they are, as the Slint path's texture does. An
/// sRGB format would encode them a second time.
fn pick_format(offered: &[wgpu::TextureFormat]) -> Option<wgpu::TextureFormat> {
    offered
        .iter()
        .copied()
        .find(|f| !f.is_srgb())
        .or_else(|| offered.first().copied())
}

#[cfg(test)]
mod tests {
    use super::*;
    use origami_engine::FrameUpdate;
    use raw_window_handle::{WebDisplayHandle, WebWindowHandle};
    use wgpu::TextureFormat as F;

    /// A GPU for the tests, or None (with the reason printed) where the
    /// machine has no adapter at all; a software one is enough.
    fn gpu() -> Option<Gpu> {
        match Gpu::new() {
            Ok(g) => Some(g),
            Err(e) => {
                eprintln!("skipping: no GPU adapter in this environment ({e})");
                None
            }
        }
    }

    #[test]
    fn the_surface_format_is_not_srgb_when_there_is_a_choice() {
        assert_eq!(
            pick_format(&[F::Bgra8UnormSrgb, F::Bgra8Unorm]),
            Some(F::Bgra8Unorm)
        );
        assert_eq!(
            pick_format(&[F::Rgba8Unorm, F::Bgra8Unorm]),
            Some(F::Rgba8Unorm)
        );
        // Only sRGB on offer: take it rather than refuse the window.
        assert_eq!(pick_format(&[F::Bgra8UnormSrgb]), Some(F::Bgra8UnormSrgb));
        assert_eq!(pick_format(&[]), None);
    }

    #[test]
    fn a_handle_no_backend_can_draw_to_is_an_error_not_a_crash() {
        let Some(gpu) = gpu() else { return };
        // A browser canvas handle, which no native backend takes.
        let host = HostSurface {
            display: Some(RawDisplayHandle::Web(WebDisplayHandle::new())),
            window: RawWindowHandle::Web(WebWindowHandle::new(1)),
        };
        // SAFETY: the handle names nothing; the backends refuse it before
        // touching it.
        let attached = unsafe { View::attach(&gpu, host, 64, 32, 1.0) };
        assert!(attached.is_err());
    }

    /// The pass draws into whatever format a surface offers. Bgra8Unorm is
    /// what Vulkan on Linux and Metal usually hand out, and it is the head
    /// texture's own order, so the bytes must come out as they went in.
    #[test]
    fn the_pass_draws_into_a_swapchain_style_format() {
        let Some(gpu) = gpu() else { return };
        let (w, h) = (8, 4);
        let update = FrameUpdate {
            seq: 1,
            width: w,
            height: h,
            runs: vec![(0, h)],
            resized: false,
        };
        let pixel = [0x10u8, 0x20, 0x30, 0xff];
        let pixels: Vec<u8> = pixel
            .iter()
            .copied()
            .cycle()
            .take((w * h * 4) as usize)
            .collect();
        let head = HeadTexture::apply(&gpu, None, &update, &pixels);
        // 64 by 32: a row of 256 bytes, which a texture copy requires.
        let (tw, th) = (64, 32);
        let target = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("swapchain stand-in"),
            size: wgpu::Extent3d {
                width: tw,
                height: th,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: F::Bgra8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let blit = Blit::with_format(&gpu, F::Bgra8Unorm);
        blit.draw(
            &gpu,
            &target.create_view(&Default::default()),
            (tw, th),
            &head,
            ScaleMode::Fit,
            None,
        );
        let out = read_back(&gpu, &target);
        assert_eq!(&out[..4], &pixel);
        assert_eq!(&out[out.len() - 4..], &pixel);
        gpu.device.poll(wgpu::PollType::wait_indefinitely()).ok();
        assert_eq!(gpu.errors.count(), 0);
    }

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
}
