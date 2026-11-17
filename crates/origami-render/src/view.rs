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
    /// (Wayland, X11). wgpu refuses a surface without one, so Cocoa and
    /// Win32 give their empty display handles.
    pub display: Option<RawDisplayHandle>,
    /// The window: an `NSView` or `CAMetalLayer` pointer, an `HWND`, an X11
    /// window or a `wl_surface`.
    pub window: RawWindowHandle,
    /// `window` is an AppKit pointer that is a `CAMetalLayer`, not an `NSView`.
    pub metal_layer: bool,
}

/// A DirectComposition target (Windows), where the host composites the swap
/// chain itself: the only way to a surface that blends over what is behind it.
#[derive(Clone, Copy, Debug)]
pub enum Composition {
    /// An `ISwapChainPanelNative*`: a WinUI `SwapChainPanel`.
    SwapChainPanel(*mut std::ffi::c_void),
    /// An `IDCompositionVisual*`.
    Visual(*mut std::ffi::c_void),
}

/// Where a view draws: a window handle or a composition target.
#[derive(Clone, Copy, Debug)]
pub enum Host {
    Window(HostSurface),
    Composition(Composition),
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
    /// The window system says the window is covered or not shown (Metal
    /// reports this for any window that is not on screen, headless ones
    /// included). The head was drawn into an offscreen image of the same
    /// size and nothing was presented.
    Occluded,
    /// The swapchain no longer matched the window and was rebuilt. Nothing
    /// was presented; draw again.
    Reconfigured,
}

/// A head drawn into one host surface.
pub struct View {
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    blit: Blit,
    /// The target of a draw while the window is occluded.
    offscreen: Option<wgpu::Texture>,
    scale: f64,
    visible: bool,
    transparent: bool,
    /// Presents since attach, for the host's statistics.
    pub presents: u64,
    /// Wall nanoseconds in `draw` since attach: acquire, encode and submit;
    /// and the platform's present, which a window system can make slow.
    pub draw_ns: u64,
    pub present_ns: u64,
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
        // SAFETY: the caller's promise.
        unsafe { View::attach_with(gpu, host, width, height, scale, false) }
    }

    /// [`View::attach_with`] on either kind of host. A composition target needs
    /// Windows.
    ///
    /// # Safety
    /// As [`View::attach`]; a composition pointer is a live COM interface.
    pub unsafe fn attach_to(
        gpu: &Gpu,
        host: Host,
        width: u32,
        height: u32,
        scale: f64,
        transparent: bool,
    ) -> Result<View, String> {
        match host {
            // SAFETY: the caller's promise.
            Host::Window(w) => unsafe {
                View::attach_with(gpu, w, width, height, scale, transparent)
            },
            // SAFETY: as above.
            Host::Composition(c) => unsafe {
                View::attach_composition(gpu, c, width, height, scale, transparent)
            },
        }
    }

    #[cfg(windows)]
    unsafe fn attach_composition(
        gpu: &Gpu,
        c: Composition,
        width: u32,
        height: u32,
        scale: f64,
        transparent: bool,
    ) -> Result<View, String> {
        let target = match c {
            Composition::SwapChainPanel(p) => wgpu::SurfaceTargetUnsafe::SwapChainPanel(p),
            Composition::Visual(p) => wgpu::SurfaceTargetUnsafe::CompositionVisual(p),
        };
        // SAFETY: the caller's promise.
        let surface = unsafe { gpu.instance.create_surface_unsafe(target) }
            .map_err(|e| format!("cannot create a surface: {e}"))?;
        View::from_surface(gpu, surface, width, height, scale, transparent)
    }

    #[cfg(not(windows))]
    unsafe fn attach_composition(
        _: &Gpu,
        _: Composition,
        _: u32,
        _: u32,
        _: f64,
        _: bool,
    ) -> Result<View, String> {
        Err("a composition target needs Windows".into())
    }

    /// [`View::attach`], asking for a premultiplied-alpha surface;
    /// [`View::transparent`] says whether the window system gave one.
    ///
    /// # Safety
    /// As [`View::attach`].
    pub unsafe fn attach_with(
        gpu: &Gpu,
        host: HostSurface,
        width: u32,
        height: u32,
        scale: f64,
        transparent: bool,
    ) -> Result<View, String> {
        // SAFETY: the caller's promise above.
        let target = match host.window {
            #[cfg(target_vendor = "apple")]
            RawWindowHandle::AppKit(h) if host.metal_layer => {
                wgpu::SurfaceTargetUnsafe::CoreAnimationLayer(h.ns_view.as_ptr())
            }
            window => wgpu::SurfaceTargetUnsafe::RawHandle {
                raw_display_handle: host.display,
                raw_window_handle: window,
            },
        };
        let surface = unsafe { gpu.instance.create_surface_unsafe(target) }
            .map_err(|e| format!("cannot create a surface: {e}"))?;
        View::from_surface(gpu, surface, width, height, scale, transparent)
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
        View::from_surface(gpu, surface, width, height, scale, false)
    }

    fn from_surface(
        gpu: &Gpu,
        surface: wgpu::Surface<'static>,
        width: u32,
        height: u32,
        scale: f64,
        transparent: bool,
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
        let alpha = transparent.then(|| pick_alpha(&caps.alpha_modes)).flatten();
        if let Some(mode) = alpha {
            config.alpha_mode = mode;
        }
        let mut view = View {
            transparent: alpha.is_some(),
            surface,
            config,
            blit: Blit::with_format(gpu, format),
            offscreen: None,
            scale,
            visible: true,
            presents: 0,
            draw_ns: 0,
            present_ns: 0,
        };
        view.resize(gpu, width, height, scale);
        Ok(view)
    }

    /// The window changed size or moved to a display of another scale. A
    /// zero size (a minimised window) is remembered and draws nothing until a
    /// real one arrives.
    pub fn resize(&mut self, gpu: &Gpu, width: u32, height: u32, scale: f64) {
        self.scale = scale;
        self.blit.set_device_scale(scale as f32);
        if width == 0 || height == 0 {
            self.config.width = 0;
            self.config.height = 0;
            return;
        }
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&gpu.device, &self.config);
    }

    /// The surface composites over what is behind it (premultiplied alpha).
    pub fn transparent(&self) -> bool {
        self.transparent
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

    /// Where a picture of `head` pixels is drawn in this view under `mode`.
    pub fn layout(&self, head: (u32, u32), mode: ScaleMode) -> crate::Layout {
        crate::layout(self.size(), head, mode, self.scale as f32)
    }

    /// [`render_image`] at the view's size. Waits for the GPU: for self-tests,
    /// never the frame path.
    pub fn readback(
        &self,
        gpu: &Gpu,
        head: &HeadTexture,
        mode: ScaleMode,
        cursor: Option<&CursorDraw>,
    ) -> Result<Vec<u8>, String> {
        render_image(gpu, head, self.size(), mode, cursor)
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
        self.draw_with(gpu, head, mode, cursor, |_| {})
    }

    /// [`View::draw`] with `before` recording into the same command buffer, so
    /// a frame costs one submit. It runs even when no image could be had.
    pub fn draw_with(
        &mut self,
        gpu: &Gpu,
        head: &HeadTexture,
        mode: ScaleMode,
        cursor: Option<&CursorDraw>,
        before: impl FnOnce(&mut wgpu::CommandEncoder),
    ) -> Result<Drawn, String> {
        self.draw_pass(gpu, true, before, |blit, enc, target, size| {
            blit.encode(gpu, enc, target, size, head, mode, cursor)
        })
    }

    /// The surface's pixel format.
    pub fn format(&self) -> wgpu::TextureFormat {
        self.config.format
    }

    /// [`View::draw_with`] with the drawing left to `pass`, which gets the
    /// view's [`Blit`], the encoder, the image to draw into and its size.
    /// With `draw_occluded` false a covered window is not drawn at all.
    pub fn draw_pass(
        &mut self,
        gpu: &Gpu,
        draw_occluded: bool,
        before: impl FnOnce(&mut wgpu::CommandEncoder),
        pass: impl FnOnce(&Blit, &mut wgpu::CommandEncoder, &wgpu::TextureView, (u32, u32)),
    ) -> Result<Drawn, String> {
        let mut enc = gpu.device.create_command_encoder(&Default::default());
        if !self.visible || self.config.width == 0 || self.config.height == 0 {
            before(&mut enc);
            gpu.queue.submit([enc.finish()]);
            return Ok(Drawn::Skipped);
        }
        let start = std::time::Instant::now();
        let (frame, suboptimal) = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f) => (f, false),
            // Usable now; the swapchain is rebuilt after it is shown.
            wgpu::CurrentSurfaceTexture::Suboptimal(f) => (f, true),
            wgpu::CurrentSurfaceTexture::Timeout => {
                before(&mut enc);
                gpu.queue.submit([enc.finish()]);
                return Ok(Drawn::Skipped);
            }
            wgpu::CurrentSurfaceTexture::Occluded => {
                before(&mut enc);
                if !draw_occluded {
                    gpu.queue.submit([enc.finish()]);
                    return Ok(Drawn::Occluded);
                }
                let size = (self.config.width, self.config.height);
                let stale = self
                    .offscreen
                    .as_ref()
                    .is_none_or(|t| (t.width(), t.height()) != size);
                if stale {
                    self.offscreen = Some(gpu.device.create_texture(&wgpu::TextureDescriptor {
                        label: Some("occluded view"),
                        size: wgpu::Extent3d {
                            width: size.0,
                            height: size.1,
                            depth_or_array_layers: 1,
                        },
                        mip_level_count: 1,
                        sample_count: 1,
                        dimension: wgpu::TextureDimension::D2,
                        format: self.config.format,
                        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                        view_formats: &[],
                    }));
                }
                let target = self
                    .offscreen
                    .as_ref()
                    .unwrap()
                    .create_view(&Default::default());
                pass(&self.blit, &mut enc, &target, size);
                gpu.queue.submit([enc.finish()]);
                return Ok(Drawn::Occluded);
            }
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                before(&mut enc);
                gpu.queue.submit([enc.finish()]);
                self.surface.configure(&gpu.device, &self.config);
                return Ok(Drawn::Reconfigured);
            }
            wgpu::CurrentSurfaceTexture::Validation => {
                return Err("the surface reported a validation error".into());
            }
        };
        before(&mut enc);
        let target = frame.texture.create_view(&Default::default());
        pass(
            &self.blit,
            &mut enc,
            &target,
            (self.config.width, self.config.height),
        );
        gpu.queue.submit([enc.finish()]);
        let drawn = start.elapsed();
        gpu.queue.present(frame);
        self.draw_ns += drawn.as_nanos() as u64;
        self.present_ns += (start.elapsed() - drawn).as_nanos() as u64;
        self.presents += 1;
        if suboptimal {
            self.surface.configure(&gpu.device, &self.config);
        }
        Ok(Drawn::Presented)
    }
}

/// Draw `head` as a view of `size` would and copy the result to the CPU as
/// tight Bgra8 rows, whatever format the window's swapchain uses.
pub fn render_image(
    gpu: &Gpu,
    head: &HeadTexture,
    size: (u32, u32),
    mode: ScaleMode,
    cursor: Option<&CursorDraw>,
) -> Result<Vec<u8>, String> {
    let (w, h) = size;
    if w == 0 || h == 0 {
        return Err("the view has no size".into());
    }
    let format = wgpu::TextureFormat::Bgra8Unorm;
    let extent = wgpu::Extent3d {
        width: w,
        height: h,
        depth_or_array_layers: 1,
    };
    let target = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("readback"),
        size: extent,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let row = w * 4;
    let padded = row.next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
    let buf = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback"),
        size: u64::from(padded) * u64::from(h),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut enc = gpu.device.create_command_encoder(&Default::default());
    Blit::with_format(gpu, format).encode(
        gpu,
        &mut enc,
        &target.create_view(&Default::default()),
        size,
        head,
        mode,
        cursor,
    );
    enc.copy_texture_to_buffer(
        target.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buf,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded),
                rows_per_image: None,
            },
        },
        extent,
    );
    gpu.queue.submit([enc.finish()]);
    let (tx, rx) = std::sync::mpsc::channel();
    buf.slice(..).map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    gpu.device
        .poll(wgpu::PollType::wait_indefinitely())
        .map_err(|e| format!("the GPU did not finish the readback: {e}"))?;
    rx.recv()
        .map_err(|e| e.to_string())?
        .map_err(|e| format!("cannot map the readback buffer: {e}"))?;
    let mapped = buf
        .slice(..)
        .get_mapped_range()
        .map_err(|e| format!("cannot read the readback buffer: {e}"))?;
    let mut out = Vec::with_capacity((row * h) as usize);
    for line in mapped.chunks(padded as usize) {
        out.extend_from_slice(&line[..row as usize]);
    }
    Ok(out)
}

/// The alpha mode that blends over what is behind. Metal offers only Opaque and
/// PostMultiplied, which clears the layer's opaque flag; CAMetalLayer treats
/// the pixels as premultiplied either way, and the cube draws them so.
fn pick_alpha(offered: &[wgpu::CompositeAlphaMode]) -> Option<wgpu::CompositeAlphaMode> {
    use wgpu::CompositeAlphaMode as M;
    [M::PreMultiplied, M::PostMultiplied, M::Inherit]
        .into_iter()
        .find(|m| offered.contains(m))
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
    fn a_blending_alpha_mode_is_found_where_metal_offers_only_post_multiplied() {
        use wgpu::CompositeAlphaMode as M;
        assert_eq!(
            pick_alpha(&[M::Opaque, M::PostMultiplied]),
            Some(M::PostMultiplied)
        );
        assert_eq!(
            pick_alpha(&[M::Opaque, M::PostMultiplied, M::PreMultiplied]),
            Some(M::PreMultiplied)
        );
        assert_eq!(pick_alpha(&[M::Opaque, M::Inherit]), Some(M::Inherit));
        assert_eq!(pick_alpha(&[M::Opaque]), None);
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

    /// A visual of our own gives a surface that blends over what is behind it.
    #[cfg(windows)]
    #[test]
    fn a_composition_visual_gives_a_transparent_surface() {
        use windows::core::{IUnknown, Interface};
        use windows::Win32::Graphics::DirectComposition::{
            DCompositionCreateDevice2, IDCompositionDevice,
        };
        let Some(gpu) = gpu() else { return };
        // SAFETY: plain COM calls; the visual outlives the view.
        let (device, view) = unsafe {
            let device: IDCompositionDevice = DCompositionCreateDevice2(None::<&IUnknown>).unwrap();
            let visual = device.CreateVisual().unwrap();
            let host = Host::Composition(Composition::Visual(visual.as_raw()));
            (
                device,
                View::attach_to(&gpu, host, 64, 64, 1.0, true).unwrap(),
            )
        };
        assert!(view.transparent());
        drop((view, device));
    }

    // wgpu-hal asserts on a non-Windows display handle under DX12 instead of erroring.
    #[cfg_attr(windows, ignore)]
    #[test]
    fn a_handle_no_backend_can_draw_to_is_an_error_not_a_crash() {
        let Some(gpu) = gpu() else { return };
        // A browser canvas handle, which no native backend takes.
        let host = HostSurface {
            display: Some(RawDisplayHandle::Web(WebDisplayHandle::new())),
            window: RawWindowHandle::Web(WebWindowHandle::new(1)),
            metal_layer: false,
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

    /// The shader must put the picture where `layout` says: whole device
    /// pixels at 2x when they give one logical pixel per guest pixel.
    #[test]
    fn fit_device_draws_where_the_layout_says() {
        let Some(gpu) = gpu() else { return };
        use wgpu::TextureFormat as F;
        let (w, h) = (5, 2);
        let update = FrameUpdate {
            seq: 1,
            width: w,
            height: h,
            runs: vec![(0, h)],
            resized: false,
        };
        let (left, right) = ([0x10u8, 0x20, 0x30, 0xff], [0x90u8, 0x80, 0x70, 0xff]);
        let pixels: Vec<u8> = (0..w * h)
            .flat_map(|i| if i % w < 2 { left } else { right })
            .collect();
        let head = HeadTexture::apply(&gpu, None, &update, &pixels);
        let target = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("swapchain stand-in"),
            size: wgpu::Extent3d {
                width: 64,
                height: 32,
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
        blit.set_device_scale(2.0);
        blit.draw(
            &gpu,
            &target.create_view(&Default::default()),
            (64, 32),
            &head,
            ScaleMode::FitDevice,
            None,
        );
        let out = read_back(&gpu, &target);
        let at = |x: usize, y: usize| out[(y * 64 + x) * 4..][..4].to_vec();
        let l = crate::layout((64, 32), (w, h), ScaleMode::FitDevice, 2.0);
        assert_eq!((l.x, l.y, l.width, l.height), (2.0, 4.0, 60.0, 24.0));
        let bg = vec![16, 16, 16, 255];
        assert_eq!(
            (at(1, 10), at(62, 10), at(10, 3), at(10, 28)),
            (bg.clone(), bg.clone(), bg.clone(), bg)
        );
        assert_eq!((at(2, 4), at(25, 27)), (left.to_vec(), left.to_vec()));
        assert_eq!((at(26, 4), at(61, 27)), (right.to_vec(), right.to_vec()));
        gpu.device.poll(wgpu::PollType::wait_indefinitely()).ok();
        assert_eq!(gpu.errors.count(), 0);
    }

    #[test]
    fn a_readback_returns_the_drawn_picture_with_tight_rows() {
        let Some(gpu) = gpu() else { return };
        // 5 pixels is a 20-byte row, so the copy's 256-byte padding is cut.
        let (w, h) = (5, 3);
        let update = FrameUpdate {
            seq: 1,
            width: w,
            height: h,
            runs: vec![(0, h)],
            resized: false,
        };
        let pixels: Vec<u8> = (0..w * h * 4).map(|i| (i % 251) as u8 | 1).collect();
        let head = HeadTexture::apply(&gpu, None, &update, &pixels);
        let want = pixels;
        let got = render_image(&gpu, &head, (w, h), ScaleMode::Integer, None).unwrap();
        assert_eq!(got.len(), want.len());
        // The unused byte is whatever the pass writes; compare the colours.
        for (g, p) in got.chunks(4).zip(want.chunks(4)) {
            assert_eq!(&g[..3], &p[..3]);
        }
        assert!(render_image(&gpu, &head, (0, h), ScaleMode::Integer, None).is_err());
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
