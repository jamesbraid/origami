//! The GPU half of the GTK front end, on raw Vulkan (ash), with no wgpu.
//!
//! Two jobs:
//!
//! * import the memory QEMU exported (`OPAQUE_FD`) as a buffer, which is the
//!   [`Importer`] the engine wants;
//! * own a small ring of images whose memory is exported as dma-bufs, copy
//!   each frame from the imported buffer into the next free one on the GPU,
//!   and hand GTK the dma-buf (`GdkDmabufTexture`).
//!
//! The picture never touches the CPU: QEMU's memory is read by the GPU, the
//! copy lands in memory the compositor can scan out.

use ash::{ext, khr, vk};
use origami_display::proto::{Region, CAP_VULKAN, REGION_DEDICATED};
use origami_display::{Handle, Importer, Memory};
use origami_engine::{GpuMemory, ImportedUpdate};
use std::any::Any;
use std::ffi::CStr;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const DRM_FORMAT_XRGB8888: u32 = u32::from_le_bytes(*b"XR24");
pub const DRM_FORMAT_MOD_LINEAR: u64 = 0;
const FORMAT: vk::Format = vk::Format::B8G8R8A8_UNORM;

/// What QEMU's allocation is made for; the import must be a compatible buffer.
fn region_usage() -> vk::BufferUsageFlags {
    vk::BufferUsageFlags::STORAGE_BUFFER | vk::BufferUsageFlags::TRANSFER_SRC
}

pub struct Dev {
    _entry: ash::Entry,
    pub instance: ash::Instance,
    pub phys: vk::PhysicalDevice,
    pub device: ash::Device,
    pub queue: vk::Queue,
    pub qfam: u32,
    pub fd: khr::external_memory_fd::Device,
    pub modifier: Option<ext::image_drm_format_modifier::Device>,
    /// The device can export images as dma-bufs (the GTK dmabuf-texture route).
    pub dma_buf_export: bool,
    /// `minImportedHostPointerAlignment` when the device reads mapped shared
    /// memory in place (`VK_EXT_external_memory_host`).
    pub host_align: Option<u64>,
    host: Option<ext::external_memory_host::Device>,
    pub mem: vk::PhysicalDeviceMemoryProperties,
    pub device_uuid: [u8; 16],
    pub driver_uuid: [u8; 16],
    pub name: String,
    pub driver: String,
    pub api_version: u32,
    pub dev_type: vk::PhysicalDeviceType,
    queue_lock: Mutex<()>,
}

pub fn hex(b: &[u8; 16]) -> String {
    b.iter().map(|b| format!("{b:02x}")).collect()
}

fn has(exts: &[vk::ExtensionProperties], name: &CStr) -> bool {
    exts.iter()
        .any(|e| e.extension_name_as_c_str().is_ok_and(|n| n == name))
}

impl Dev {
    /// Open a device that can import QEMU's memory and export dma-bufs.
    /// `pick` selects one by index or name substring; otherwise the first
    /// discrete adapter that qualifies, then any that does.
    pub fn open(pick: Option<&str>) -> Result<Arc<Dev>, String> {
        // SAFETY: loading the system's Vulkan loader.
        let entry = unsafe { ash::Entry::load() }.map_err(|e| format!("no Vulkan loader: {e}"))?;
        let inst_ver = unsafe { entry.try_enumerate_instance_version() }
            .ok()
            .flatten()
            .unwrap_or(vk::API_VERSION_1_0);
        if inst_ver < vk::API_VERSION_1_1 {
            return Err("Vulkan instance older than 1.1".into());
        }
        let app = vk::ApplicationInfo::default()
            .application_name(c"origami-gtk")
            .api_version(vk::API_VERSION_1_1);
        let instance = unsafe {
            entry.create_instance(
                &vk::InstanceCreateInfo::default().application_info(&app),
                None,
            )
        }
        .map_err(|e| format!("vkCreateInstance: {e}"))?;
        let phys_all = unsafe { instance.enumerate_physical_devices() }
            .map_err(|e| format!("enumerate: {e}"))?;
        let mut cands = Vec::new();
        for (i, &p) in phys_all.iter().enumerate() {
            let props = unsafe { instance.get_physical_device_properties(p) };
            let name = props
                .device_name_as_c_str()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let exts =
                unsafe { instance.enumerate_device_extension_properties(p) }.unwrap_or_default();
            let (fd_ext, dma_ext, mod_ext) = (
                has(&exts, khr::external_memory_fd::NAME),
                has(&exts, ext::external_memory_dma_buf::NAME),
                has(&exts, ext::image_drm_format_modifier::NAME),
            );
            eprintln!(
                "origami-gtk: vulkan device {i}: {name} ({:?}) api {}.{} external_memory_fd={fd_ext} \
                 external_memory_dma_buf={dma_ext} image_drm_format_modifier={mod_ext}",
                props.device_type,
                vk::api_version_major(props.api_version),
                vk::api_version_minor(props.api_version),
            );
            // The import needs the fd extension. The dma-buf export is only
            // for the scanout images; without it the GL route can still run.
            let ok = fd_ext;
            let wanted =
                pick.is_none_or(|s| s.parse::<usize>().is_ok_and(|n| n == i) || name.contains(s));
            if ok && wanted {
                cands.push((p, props, name, exts));
            }
        }
        cands.sort_by_key(|c| c.1.device_type != vk::PhysicalDeviceType::DISCRETE_GPU);
        let (phys, props, name, exts) = cands
            .into_iter()
            .next()
            .ok_or("no Vulkan device with external_memory_fd and external_memory_dma_buf")?;
        if props.api_version < vk::API_VERSION_1_1 {
            return Err("Vulkan device older than 1.1".into());
        }
        let qfam = unsafe { instance.get_physical_device_queue_family_properties(phys) }
            .iter()
            .position(|q| {
                q.queue_flags
                    .intersects(vk::QueueFlags::GRAPHICS | vk::QueueFlags::TRANSFER)
            })
            .ok_or("no queue family")? as u32;
        let mut wanted: Vec<*const i8> = vec![khr::external_memory_fd::NAME.as_ptr()];
        let dma_buf_export = has(&exts, ext::external_memory_dma_buf::NAME);
        if dma_buf_export {
            wanted.push(ext::external_memory_dma_buf::NAME.as_ptr());
        }
        let mut with_modifier = false;
        if dma_buf_export
            && has(&exts, ext::image_drm_format_modifier::NAME)
            && has(&exts, khr::image_format_list::NAME)
        {
            wanted.push(ext::image_drm_format_modifier::NAME.as_ptr());
            wanted.push(khr::image_format_list::NAME.as_ptr());
            // Both depend on promoted-in-1.2 or 1.1 extensions that older
            // drivers list separately.
            for dep in [
                khr::sampler_ycbcr_conversion::NAME,
                khr::bind_memory2::NAME,
                khr::get_memory_requirements2::NAME,
                khr::maintenance1::NAME,
            ] {
                if has(&exts, dep) {
                    wanted.push(dep.as_ptr());
                }
            }
            with_modifier = true;
        }
        for opt in [
            khr::external_memory::NAME,
            ext::queue_family_foreign::NAME,
            ext::external_memory_host::NAME,
        ] {
            if has(&exts, opt) {
                wanted.push(opt.as_ptr());
            }
        }
        let prio = [1.0f32];
        let qinfo = [vk::DeviceQueueCreateInfo::default()
            .queue_family_index(qfam)
            .queue_priorities(&prio)];
        let device = unsafe {
            instance.create_device(
                phys,
                &vk::DeviceCreateInfo::default()
                    .queue_create_infos(&qinfo)
                    .enabled_extension_names(&wanted),
                None,
            )
        }
        .map_err(|e| format!("vkCreateDevice: {e}"))?;
        let queue = unsafe { device.get_device_queue(qfam, 0) };
        let mut id = vk::PhysicalDeviceIDProperties::default();
        let mut driver = vk::PhysicalDeviceDriverProperties::default();
        let mut host = vk::PhysicalDeviceExternalMemoryHostPropertiesEXT::default();
        let mut p2 = vk::PhysicalDeviceProperties2::default()
            .push_next(&mut id)
            .push_next(&mut driver);
        let host_ext = has(&exts, ext::external_memory_host::NAME);
        if host_ext {
            p2 = p2.push_next(&mut host);
        }
        unsafe { instance.get_physical_device_properties2(phys, &mut p2) };
        let host_align =
            Some(host.min_imported_host_pointer_alignment).filter(|a| host_ext && *a > 0);
        let driver_name = format!(
            "{} {}",
            driver
                .driver_name_as_c_str()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default(),
            driver
                .driver_info_as_c_str()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default()
        );
        Ok(Arc::new(Dev {
            fd: khr::external_memory_fd::Device::new(&instance, &device),
            dma_buf_export,
            host_align,
            host: host_align.map(|_| ext::external_memory_host::Device::new(&instance, &device)),
            modifier: with_modifier
                .then(|| ext::image_drm_format_modifier::Device::new(&instance, &device)),
            mem: unsafe { instance.get_physical_device_memory_properties(phys) },
            device_uuid: id.device_uuid,
            driver_uuid: id.driver_uuid,
            name,
            driver: driver_name,
            api_version: props.api_version,
            dev_type: props.device_type,
            queue_lock: Mutex::new(()),
            _entry: entry,
            instance,
            phys,
            device,
            queue,
            qfam,
        }))
    }

    pub fn describe(&self) -> String {
        format!(
            "{} ({:?}, {}), Vulkan {}.{}, device {}, dma-buf export {}, host import {}, modifiers {}",
            self.name,
            self.dev_type,
            self.driver.trim(),
            vk::api_version_major(self.api_version),
            vk::api_version_minor(self.api_version),
            hex(&self.device_uuid),
            self.dma_buf_export,
            self.host_align.is_some(),
            if self.modifier.is_some() {
                "yes"
            } else {
                "no (linear)"
            }
        )
    }
}

impl Drop for Dev {
    fn drop(&mut self) {
        // SAFETY: last reference; every object made from it holds an Arc.
        unsafe {
            let _ = self.device.device_wait_idle();
            self.device.destroy_device(None);
            self.instance.destroy_instance(None);
        }
    }
}

// ---- Importing QEMU's regions ---------------------------------------------

/// A region on the device, as a transfer-source buffer.
pub struct ImportedBuffer {
    dev: Arc<Dev>,
    pub buffer: vk::Buffer,
    memory: vk::DeviceMemory,
    pub size: u64,
    /// The mapping a host-pointer import reads, freed after the buffer.
    _mapping: Option<Arc<dyn Memory>>,
}

impl GpuMemory for ImportedBuffer {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl Drop for ImportedBuffer {
    fn drop(&mut self) {
        // SAFETY: made by `import`; a queued copy holds the Arc this lives in
        // (`Pending::memory`) until its fence has signalled.
        unsafe {
            self.dev.device.destroy_buffer(self.buffer, None);
            self.dev.device.free_memory(self.memory, None);
        }
    }
}

pub struct VkImporter {
    pub dev: Arc<Dev>,
}

fn bad(why: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, why.into())
}

impl Importer for VkImporter {
    fn caps(&self) -> u32 {
        CAP_VULKAN
    }
    fn device_uuid(&self) -> [u8; 16] {
        self.dev.device_uuid
    }
    fn export_uuid(&self) -> Option<[u8; 16]> {
        Some(self.dev.device_uuid)
    }
    fn accepts(&self, device_uuid: &[u8; 16], driver_uuid: &[u8; 16]) -> Result<(), String> {
        if *device_uuid != self.dev.device_uuid {
            return Err(format!(
                "QEMU exported on adapter {}, this viewer draws on {}",
                hex(device_uuid),
                hex(&self.dev.device_uuid)
            ));
        }
        if *driver_uuid != self.dev.driver_uuid {
            return Err(format!(
                "QEMU's driver {} is not this viewer's {}",
                hex(driver_uuid),
                hex(&self.dev.driver_uuid)
            ));
        }
        Ok(())
    }

    fn import(&self, region: &Region, handle: Handle) -> io::Result<Arc<dyn GpuMemory>> {
        let d = &self.dev;
        let Handle::Fd(fd) = handle;
        if region.memory_type >= d.mem.memory_type_count {
            return Err(bad(format!(
                "memory type {} is not here",
                region.memory_type
            )));
        }
        let handle_type = vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD;
        // SAFETY: Vulkan calls on this device's objects.
        unsafe {
            let mut ext = vk::ExternalMemoryBufferCreateInfo::default().handle_types(handle_type);
            let buffer = d
                .device
                .create_buffer(
                    &vk::BufferCreateInfo::default()
                        .size(region.size)
                        .usage(region_usage())
                        .sharing_mode(vk::SharingMode::EXCLUSIVE)
                        .push_next(&mut ext),
                    None,
                )
                .map_err(|e| io::Error::other(format!("vkCreateBuffer: {e}")))?;
            let need = d.device.get_buffer_memory_requirements(buffer);
            if need.memory_type_bits & (1 << region.memory_type) == 0
                || region.alloc_size < need.size
            {
                d.device.destroy_buffer(buffer, None);
                return Err(bad("QEMU's memory does not fit this buffer"));
            }
            let mut import = vk::ImportMemoryFdInfoKHR::default()
                .handle_type(handle_type)
                .fd(fd.as_raw_fd());
            let mut dedicated = vk::MemoryDedicatedAllocateInfo::default().buffer(buffer);
            let mut info = vk::MemoryAllocateInfo::default()
                .allocation_size(region.alloc_size)
                .memory_type_index(region.memory_type)
                .push_next(&mut import);
            if region.flags & REGION_DEDICATED != 0 {
                info = info.push_next(&mut dedicated);
            }
            let memory = match d.device.allocate_memory(&info, None) {
                Ok(m) => {
                    let _ = fd.into_raw_fd(); // the driver owns it now
                    m
                }
                Err(e) => {
                    d.device.destroy_buffer(buffer, None);
                    return Err(io::Error::other(format!("vkAllocateMemory (import): {e}")));
                }
            };
            if let Err(e) = d.device.bind_buffer_memory(buffer, memory, 0) {
                d.device.destroy_buffer(buffer, None);
                d.device.free_memory(memory, None);
                return Err(io::Error::other(format!("vkBindBufferMemory: {e}")));
            }
            Ok(Arc::new(ImportedBuffer {
                dev: d.clone(),
                buffer,
                memory,
                size: region.size,
                _mapping: None,
            }))
        }
    }

    fn wrap(&self, _: &Region, _: &Arc<dyn Memory>) -> Option<Arc<dyn GpuMemory>> {
        None
    }
    fn wrap_shm(&self, size: u64, memory: &Arc<dyn Memory>) -> Result<Arc<dyn GpuMemory>, String> {
        Ok(Arc::new(import_host(&self.dev, memory, size)?))
    }
    fn shm_needs_write(&self) -> bool {
        // NVIDIA refuses to import a read-only mapping.
        self.dev.host_align.is_some()
    }
    fn name(&self) -> &'static str {
        "vulkan import"
    }
}

/// `size` bytes of a mapped slot as a transfer-source buffer, read in place
/// (`VK_EXT_external_memory_host`).
fn import_host(
    d: &Arc<Dev>,
    memory: &Arc<dyn Memory>,
    size: u64,
) -> Result<ImportedBuffer, String> {
    let (Some(align), Some(host)) = (d.host_align, &d.host) else {
        return Err("the Vulkan device cannot import host memory".into());
    };
    let ptr = memory.base();
    if size == 0 || size > memory.len() as u64 || !size.is_multiple_of(align) {
        return Err(format!(
            "{size} bytes of a {} byte mapping, imported in units of {align}",
            memory.len()
        ));
    }
    if !(ptr as u64).is_multiple_of(align) {
        return Err(format!("{ptr:p} is not aligned to {align}"));
    }
    let kind = vk::ExternalMemoryHandleTypeFlags::HOST_ALLOCATION_EXT;
    // SAFETY: Vulkan calls on this device; the buffer keeps `memory` mapped.
    unsafe {
        let mut allowed = vk::MemoryHostPointerPropertiesEXT::default();
        (host.fp().get_memory_host_pointer_properties_ext)(
            host.device(),
            kind,
            ptr.cast(),
            &mut allowed,
        )
        .result()
        .map_err(|e| format!("host pointer properties: {e}"))?;
        let mut ext = vk::ExternalMemoryBufferCreateInfo::default().handle_types(kind);
        let buffer = d
            .device
            .create_buffer(
                &vk::BufferCreateInfo::default()
                    .size(size)
                    .usage(region_usage())
                    .sharing_mode(vk::SharingMode::EXCLUSIVE)
                    .push_next(&mut ext),
                None,
            )
            .map_err(|e| format!("vkCreateBuffer: {e}"))?;
        let need = d.device.get_buffer_memory_requirements(buffer);
        let bits = need.memory_type_bits & allowed.memory_type_bits;
        // Coherent, so the producer's writes are seen without an invalidate.
        let pick = |want: vk::MemoryPropertyFlags| {
            (0..d.mem.memory_type_count).find(|&i| {
                bits & (1 << i) != 0 && d.mem.memory_types[i as usize].property_flags.contains(want)
            })
        };
        let visible = vk::MemoryPropertyFlags::HOST_VISIBLE;
        let Some(index) =
            pick(visible | vk::MemoryPropertyFlags::HOST_COHERENT).or_else(|| pick(visible))
        else {
            d.device.destroy_buffer(buffer, None);
            return Err("no host visible memory type takes host pointers".into());
        };
        let mut import = vk::ImportMemoryHostPointerInfoEXT::default()
            .handle_type(kind)
            .host_pointer(ptr.cast());
        let info = vk::MemoryAllocateInfo::default()
            .allocation_size(size)
            .memory_type_index(index)
            .push_next(&mut import);
        let mem = match d.device.allocate_memory(&info, None) {
            Ok(m) => m,
            Err(e) => {
                d.device.destroy_buffer(buffer, None);
                return Err(format!("vkAllocateMemory (host): {e}"));
            }
        };
        if let Err(e) = d.device.bind_buffer_memory(buffer, mem, 0) {
            d.device.destroy_buffer(buffer, None);
            d.device.free_memory(mem, None);
            return Err(format!("vkBindBufferMemory: {e}"));
        }
        Ok(ImportedBuffer {
            dev: d.clone(),
            buffer,
            memory: mem,
            size,
            _mapping: Some(memory.clone()),
        })
    }
}

// ---- Exported scanout images ----------------------------------------------

/// One image whose memory is a dma-buf.
pub struct Slot {
    dev: Arc<Dev>,
    image: vk::Image,
    memory: vk::DeviceMemory,
    cmd: vk::CommandBuffer,
    fence: Arc<FenceOwner>,
    pub fd: Arc<OwnedFd>,
    pub modifier: u64,
    pub stride: u32,
    pub offset: u32,
    /// False while GTK holds a texture made from this slot.
    pub free: Arc<AtomicBool>,
    /// The image holds a picture (the first copy is the whole frame).
    valid: bool,
    /// Lines whose content is older than the newest picture, as sorted,
    /// disjoint runs `y0..y1`. A copy brings exactly these up to date.
    stale: Vec<(u32, u32)>,
}

#[derive(Default, Debug, Clone)]
pub struct Counters {
    /// Frames copied on the GPU from imported memory into scanout images.
    pub gpu_frames: u64,
    pub gpu_lines: u64,
    pub gpu_bytes: u64,
    /// What full-frame copies would have moved, to show the saving.
    pub full_frame_bytes: u64,
    /// Bytes of picture moved by the CPU (always 0 here; the selftest's
    /// readback is counted apart).
    pub cpu_pixel_bytes: u64,
    pub readback_bytes: u64,
    pub skipped_busy: u64,
}

pub struct Scanout {
    /// Distinguishes this set of images from the one it replaced at a resize.
    pub id: u64,
    dev: Arc<Dev>,
    pool: vk::CommandPool,
    pub width: u32,
    pub height: u32,
    pub slots: Vec<Slot>,
    pub counters: Counters,
    pub last_cost: Duration,
    pub modifier_note: String,
    next: usize,
}

/// A copy that is queued on the GPU and not yet known to be finished.
pub struct Pending {
    pub scanout: u64,
    pub slot: usize,
    /// The frame's sequence number, so a late copy is not shown after a newer one.
    pub seq: u64,
    pub started: Instant,
    /// When the worker saw the GPU finish; the copy's cost is this less `started`.
    pub finished: Option<Instant>,
    /// Keeps the imported buffer the copy reads alive until it has finished.
    #[allow(dead_code)]
    memory: Arc<dyn GpuMemory>,
    /// The frame's lease: the producer may not rewrite the slot until it drops.
    #[allow(dead_code)]
    hold: Option<Arc<dyn Any + Send + Sync>>,
    wait_on: Option<Arc<FenceOwner>>,
}

impl Pending {
    /// Block until the GPU has finished. Safe from any thread: only the
    /// queue needs external synchronisation, not waiting on a fence.
    pub fn wait(&mut self) -> Result<(), String> {
        let Some(fence) = &self.wait_on else {
            self.finished = Some(Instant::now());
            return Ok(());
        };
        // SAFETY: the Arc keeps the fence alive, and its slot is not
        // resubmitted while its image is marked in use.
        let r = unsafe {
            fence
                .dev
                .device
                .wait_for_fences(&[fence.fence], true, 2_000_000_000)
        }
        .map_err(|e| format!("fence: {e}"));
        if r.is_err() {
            // The copy may still be reading the slot: drain the device before the lease can end.
            let _q = fence
                .dev
                .queue_lock
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            // SAFETY: the queue lock serialises this with every submit.
            unsafe {
                let _ = fence.dev.device.device_wait_idle();
            }
        }
        self.finished = Some(Instant::now());
        r
    }
}

/// A ready picture: which slot, and what GTK needs to import it.
pub struct Ready {
    pub slot: usize,
    pub fd: Arc<OwnedFd>,
    pub modifier: u64,
    pub stride: u32,
    pub offset: u32,
    pub free: Arc<AtomicBool>,
    pub gpu_time: Duration,
}

impl Scanout {
    /// Make `count` images of `width` x `height`. `gtk_mods` is what the GDK
    /// display can import for XRGB8888; the images use a modifier in both
    /// that and Vulkan's own list, or fail naming why.
    pub fn new(
        dev: &Arc<Dev>,
        width: u32,
        height: u32,
        count: usize,
        gtk_mods: &[u64],
    ) -> Result<Scanout, String> {
        let d = dev;
        let pool = unsafe {
            d.device.create_command_pool(
                &vk::CommandPoolCreateInfo::default()
                    .queue_family_index(d.qfam)
                    .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER),
                None,
            )
        }
        .map_err(|e| format!("command pool: {e}"))?;
        // Gives the pool back if any image below cannot be made.
        let mut pool_guard = PoolGuard {
            dev: d.clone(),
            pool: Some(pool),
        };
        let mut note = String::new();
        let mods = if d.modifier.is_some() {
            let vkmods = vulkan_modifiers(d, width, height);
            note = format!(
                "vulkan offers {} modifiers {:x?}; gtk offers {} {:x?}",
                vkmods.len(),
                vkmods,
                gtk_mods.len(),
                gtk_mods
            );
            let both: Vec<u64> = vkmods
                .iter()
                .copied()
                .filter(|m| gtk_mods.contains(m))
                .collect();
            if both.is_empty() {
                return Err(format!("no modifier in common. {note}"));
            }
            note.push_str(&format!("; common {both:x?}"));
            Some(both)
        } else {
            if !gtk_mods.contains(&DRM_FORMAT_MOD_LINEAR) {
                return Err("no modifier extension and GTK takes no linear buffer".into());
            }
            None
        };
        let mut slots = Vec::new();
        for _ in 0..count {
            slots.push(Slot::new(d, pool, width, height, mods.as_deref())?);
        }
        let chosen = slots[0].modifier;
        pool_guard.pool = None;
        static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        Ok(Scanout {
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            dev: d.clone(),
            pool,
            width,
            height,
            slots,
            counters: Counters::default(),
            last_cost: Duration::ZERO,
            modifier_note: format!("{note}; chosen {:#x}", chosen),
            next: 0,
        })
    }

    /// Mark `runs` stale in every image. Call it for every frame taken, shown
    /// or not, so a frame skipped while GTK held every image loses nothing.
    pub fn mark_dirty(&mut self, runs: &[(u32, u32)], resized: bool) {
        for s in &mut self.slots {
            if resized {
                s.valid = false;
                s.stale.clear();
            } else {
                s.stale = merge_runs(&s.stale, runs, self.height);
            }
        }
    }

    /// Queue a GPU copy of the stale lines into a free image and return; wait
    /// on [`Pending::wait`] off the UI thread, then [`Scanout::complete`].
    /// None when GTK holds every image.
    pub fn present(&mut self, frame: &ImportedUpdate) -> Result<Option<Pending>, String> {
        let u = &frame.update;
        if frame.swap_rb {
            return Err("red-first pixels (XB24) are not drawn here".into());
        }
        if (u.width, u.height) != (self.width, self.height) {
            return Err("size changed".into());
        }
        let Some(buf) = frame.memory.as_any().downcast_ref::<ImportedBuffer>() else {
            return Err("not our memory".into());
        };
        let plane = u64::from(frame.stride) * u64::from(u.height.saturating_sub(1))
            + u64::from(u.width) * 4;
        if frame.offset + plane > buf.size || !frame.stride.is_multiple_of(4) {
            return Err("plane does not fit its region".into());
        }
        let n = self.slots.len();
        let Some(idx) = (0..n)
            .map(|i| (self.next + i) % n)
            .find(|&i| self.slots[i].free.load(Ordering::Acquire))
        else {
            self.counters.skipped_busy += 1;
            return Ok(None);
        };
        self.next = (idx + 1) % n;
        let t0 = Instant::now();
        let slot = &mut self.slots[idx];
        let runs: Vec<(u32, u32)> = if slot.valid {
            slot.stale.clone()
        } else {
            vec![(0, u.height)]
        };
        let lines: u32 = runs.iter().map(|(a, b)| b - a).sum();
        if lines > 0 || !slot.valid {
            slot.copy_from(buf, frame, &runs)?;
        }
        slot.valid = true;
        slot.stale.clear();
        self.counters.gpu_frames += 1;
        self.counters.gpu_lines += u64::from(lines);
        self.counters.gpu_bytes += u64::from(lines) * u64::from(u.width) * 4;
        self.counters.full_frame_bytes += u64::from(u.width) * u64::from(u.height) * 4;
        slot.free.store(false, Ordering::Release);
        self.last_cost = t0.elapsed();
        Ok(Some(Pending {
            scanout: self.id,
            slot: idx,
            seq: u.seq,
            started: t0,
            finished: None,
            memory: frame.memory.clone(),
            hold: frame.hold.clone(),
            wait_on: (lines > 0).then(|| slot.fence.clone()),
        }))
    }

    /// The picture a finished [`Pending`] made, for GTK to import.
    pub fn complete(&self, p: &Pending) -> Ready {
        let slot = &self.slots[p.slot];
        Ready {
            slot: p.slot,
            fd: slot.fd.clone(),
            modifier: slot.modifier,
            stride: slot.stride,
            offset: slot.offset,
            free: slot.free.clone(),
            gpu_time: p
                .finished
                .unwrap_or_else(Instant::now)
                .duration_since(p.started),
        }
    }

    /// A copy failed or timed out: the GPU may still be using the image, so
    /// wait for the device, then make every image take a full copy next time.
    pub fn fail(&mut self, slot: usize) {
        // SAFETY: waiting on our own device.
        unsafe {
            let _ = self.dev.device.device_wait_idle();
        }
        for s in &mut self.slots {
            s.valid = false;
            s.stale.clear();
        }
        self.slots[slot].free.store(true, Ordering::Release);
    }

    /// Give back an image whose picture is no longer wanted.
    pub fn discard(&self, slot: usize) {
        self.slots[slot].free.store(true, Ordering::Release);
    }

    /// Test only: read a slot's picture back to the CPU as BGRA rows.
    pub fn readback(&mut self, slot: usize) -> Result<Vec<u8>, String> {
        let bytes = self.slots[slot].readback(self.width, self.height)?;
        self.counters.readback_bytes += bytes.len() as u64;
        Ok(bytes)
    }
}

impl Drop for Scanout {
    fn drop(&mut self) {
        // SAFETY: slots drop first (field order is not enough, do it here).
        unsafe {
            let _ = self.dev.device.device_wait_idle();
            self.slots.clear();
            self.dev.device.destroy_command_pool(self.pool, None);
        }
    }
}

/// The modifiers Vulkan can export for XRGB8888 images this size that are one
/// plane and can be a copy's target and source.
fn vulkan_modifiers(d: &Dev, w: u32, h: u32) -> Vec<u64> {
    let mut list = vk::DrmFormatModifierPropertiesListEXT::default();
    let mut fp = vk::FormatProperties2::default().push_next(&mut list);
    unsafe {
        d.instance
            .get_physical_device_format_properties2(d.phys, FORMAT, &mut fp)
    };
    let n = list.drm_format_modifier_count as usize;
    let mut props = vec![vk::DrmFormatModifierPropertiesEXT::default(); n];
    let mut list = vk::DrmFormatModifierPropertiesListEXT::default()
        .drm_format_modifier_properties(&mut props);
    let mut fp = vk::FormatProperties2::default().push_next(&mut list);
    unsafe {
        d.instance
            .get_physical_device_format_properties2(d.phys, FORMAT, &mut fp)
    };
    let need = vk::FormatFeatureFlags::TRANSFER_DST | vk::FormatFeatureFlags::TRANSFER_SRC;
    let mut out = Vec::new();
    for p in props {
        if p.drm_format_modifier_plane_count != 1
            || !p.drm_format_modifier_tiling_features.contains(need)
        {
            continue;
        }
        let mut modinfo = vk::PhysicalDeviceImageDrmFormatModifierInfoEXT::default()
            .drm_format_modifier(p.drm_format_modifier)
            .sharing_mode(vk::SharingMode::EXCLUSIVE);
        let mut ext = vk::PhysicalDeviceExternalImageFormatInfo::default()
            .handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);
        let info = vk::PhysicalDeviceImageFormatInfo2::default()
            .format(FORMAT)
            .ty(vk::ImageType::TYPE_2D)
            .tiling(vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT)
            .usage(vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::TRANSFER_SRC)
            .push_next(&mut ext)
            .push_next(&mut modinfo);
        let mut eprops = vk::ExternalImageFormatProperties::default();
        let mut out_props = vk::ImageFormatProperties2::default().push_next(&mut eprops);
        let r = unsafe {
            d.instance
                .get_physical_device_image_format_properties2(d.phys, &info, &mut out_props)
        };
        if r.is_err() {
            continue;
        }
        let ip = out_props.image_format_properties;
        let exportable = eprops
            .external_memory_properties
            .external_memory_features
            .contains(vk::ExternalMemoryFeatureFlags::EXPORTABLE);
        if exportable && ip.max_extent.width >= w && ip.max_extent.height >= h {
            out.push(p.drm_format_modifier);
        }
    }
    out
}

impl Slot {
    fn new(
        d: &Arc<Dev>,
        pool: vk::CommandPool,
        w: u32,
        h: u32,
        mods: Option<&[u64]>,
    ) -> Result<Slot, String> {
        let handle = vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT;
        unsafe {
            let mut ext = vk::ExternalMemoryImageCreateInfo::default().handle_types(handle);
            let mut list;
            let mut info = vk::ImageCreateInfo::default()
                .image_type(vk::ImageType::TYPE_2D)
                .format(FORMAT)
                .extent(vk::Extent3D {
                    width: w,
                    height: h,
                    depth: 1,
                })
                .mip_levels(1)
                .array_layers(1)
                .samples(vk::SampleCountFlags::TYPE_1)
                .usage(vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::TRANSFER_SRC)
                .sharing_mode(vk::SharingMode::EXCLUSIVE)
                .initial_layout(vk::ImageLayout::UNDEFINED)
                .push_next(&mut ext);
            if let Some(m) = mods {
                list =
                    vk::ImageDrmFormatModifierListCreateInfoEXT::default().drm_format_modifiers(m);
                info = info
                    .tiling(vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT)
                    .push_next(&mut list);
            } else {
                info = info.tiling(vk::ImageTiling::LINEAR);
            }
            let image = d
                .device
                .create_image(&info, None)
                .map_err(|e| format!("vkCreateImage: {e}"))?;
            let need = d.device.get_image_memory_requirements(image);
            let pick = |want: vk::MemoryPropertyFlags| {
                (0..d.mem.memory_type_count).find(|&i| {
                    need.memory_type_bits & (1 << i) != 0
                        && d.mem.memory_types[i as usize].property_flags.contains(want)
                })
            };
            let ty = pick(vk::MemoryPropertyFlags::DEVICE_LOCAL)
                .or_else(|| pick(vk::MemoryPropertyFlags::empty()))
                .ok_or("no memory type")?;
            let mut export = vk::ExportMemoryAllocateInfo::default().handle_types(handle);
            let mut dedicated = vk::MemoryDedicatedAllocateInfo::default().image(image);
            let memory = d
                .device
                .allocate_memory(
                    &vk::MemoryAllocateInfo::default()
                        .allocation_size(need.size)
                        .memory_type_index(ty)
                        .push_next(&mut export)
                        .push_next(&mut dedicated),
                    None,
                )
                .map_err(|e| format!("vkAllocateMemory (export): {e}"))?;
            d.device
                .bind_image_memory(image, memory, 0)
                .map_err(|e| format!("bind: {e}"))?;
            let (modifier, aspect) = match &d.modifier {
                Some(m) if mods.is_some() => {
                    let mut p = vk::ImageDrmFormatModifierPropertiesEXT::default();
                    m.get_image_drm_format_modifier_properties(image, &mut p)
                        .map_err(|e| format!("get modifier: {e}"))?;
                    (
                        p.drm_format_modifier,
                        vk::ImageAspectFlags::MEMORY_PLANE_0_EXT,
                    )
                }
                _ => (DRM_FORMAT_MOD_LINEAR, vk::ImageAspectFlags::COLOR),
            };
            let layout = d.device.get_image_subresource_layout(
                image,
                vk::ImageSubresource {
                    aspect_mask: aspect,
                    mip_level: 0,
                    array_layer: 0,
                },
            );
            let raw =
                d.fd.get_memory_fd(
                    &vk::MemoryGetFdInfoKHR::default()
                        .memory(memory)
                        .handle_type(handle),
                )
                .map_err(|e| format!("vkGetMemoryFdKHR(dma-buf): {e}"))?;
            let cmd = d
                .device
                .allocate_command_buffers(
                    &vk::CommandBufferAllocateInfo::default()
                        .command_pool(pool)
                        .level(vk::CommandBufferLevel::PRIMARY)
                        .command_buffer_count(1),
                )
                .map_err(|e| format!("cmd: {e}"))?[0];
            let fence = d
                .device
                .create_fence(&vk::FenceCreateInfo::default(), None)
                .map_err(|e| format!("fence: {e}"))?;
            let fence = Arc::new(FenceOwner {
                dev: d.clone(),
                fence,
            });
            Ok(Slot {
                dev: d.clone(),
                image,
                memory,
                cmd,
                fence,
                fd: Arc::new(OwnedFd::from_raw_fd(raw)),
                modifier,
                stride: layout.row_pitch as u32,
                offset: layout.offset as u32,
                free: Arc::new(AtomicBool::new(true)),
                valid: false,
                stale: Vec::new(),
            })
        }
    }

    /// Queue the recorded commands and return at once; `fence` signals when
    /// the GPU has finished.
    fn submit(&self) -> Result<(), String> {
        let d = &self.dev;
        let cmds = [self.cmd];
        let submit = [vk::SubmitInfo::default().command_buffers(&cmds)];
        let _g = d.queue_lock.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            d.device
                .reset_fences(&[self.fence.fence])
                .map_err(|e| e.to_string())?;
            d.device
                .queue_submit(d.queue, &submit, self.fence.fence)
                .map_err(|e| format!("submit: {e}"))
        }
    }

    fn submit_wait(&self) -> Result<(), String> {
        self.submit()?;
        // SAFETY: the fence belongs to this device.
        unsafe {
            self.dev
                .device
                .wait_for_fences(&[self.fence.fence], true, 2_000_000_000)
        }
        .map_err(|e| format!("fence: {e}"))
    }

    fn range() -> vk::ImageSubresourceRange {
        vk::ImageSubresourceRange {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            base_mip_level: 0,
            level_count: 1,
            base_array_layer: 0,
            layer_count: 1,
        }
    }

    fn copy_from(
        &self,
        buf: &ImportedBuffer,
        f: &ImportedUpdate,
        runs: &[(u32, u32)],
    ) -> Result<(), String> {
        let d = &self.dev;
        let u = &f.update;
        let layers = vk::ImageSubresourceLayers {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            mip_level: 0,
            base_array_layer: 0,
            layer_count: 1,
        };
        let regions: Vec<vk::BufferImageCopy> = if f.bottom_up {
            runs.iter()
                .flat_map(|&(y0, y1)| y0..y1)
                .map(|y| vk::BufferImageCopy {
                    buffer_offset: f.offset + u64::from(u.height - 1 - y) * u64::from(f.stride),
                    buffer_row_length: 0,
                    buffer_image_height: 0,
                    image_subresource: layers,
                    image_offset: vk::Offset3D {
                        x: 0,
                        y: y as i32,
                        z: 0,
                    },
                    image_extent: vk::Extent3D {
                        width: u.width,
                        height: 1,
                        depth: 1,
                    },
                })
                .collect()
        } else {
            runs.iter()
                .map(|&(y0, y1)| vk::BufferImageCopy {
                    buffer_offset: f.offset + u64::from(y0) * u64::from(f.stride),
                    buffer_row_length: f.stride / 4,
                    buffer_image_height: 0,
                    image_subresource: layers,
                    image_offset: vk::Offset3D {
                        x: 0,
                        y: y0 as i32,
                        z: 0,
                    },
                    image_extent: vk::Extent3D {
                        width: u.width,
                        height: y1 - y0,
                        depth: 1,
                    },
                })
                .collect()
        };
        unsafe {
            d.device
                .reset_command_buffer(self.cmd, vk::CommandBufferResetFlags::empty())
                .map_err(|e| e.to_string())?;
            d.device
                .begin_command_buffer(
                    self.cmd,
                    &vk::CommandBufferBeginInfo::default()
                        .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
                )
                .map_err(|e| e.to_string())?;
            // A transition from UNDEFINED discards the picture; once the image
            // has one, take it back from the outside world with contents kept.
            let (old, src_q, dst_q) = if self.valid {
                (vk::ImageLayout::GENERAL, vk::QUEUE_FAMILY_EXTERNAL, d.qfam)
            } else {
                (
                    vk::ImageLayout::UNDEFINED,
                    vk::QUEUE_FAMILY_IGNORED,
                    vk::QUEUE_FAMILY_IGNORED,
                )
            };
            let to_dst = vk::ImageMemoryBarrier::default()
                .src_access_mask(vk::AccessFlags::empty())
                .dst_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                .old_layout(old)
                .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                .src_queue_family_index(src_q)
                .dst_queue_family_index(dst_q)
                .image(self.image)
                .subresource_range(Self::range());
            d.device.cmd_pipeline_barrier(
                self.cmd,
                vk::PipelineStageFlags::TOP_OF_PIPE,
                vk::PipelineStageFlags::TRANSFER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[to_dst],
            );
            d.device.cmd_copy_buffer_to_image(
                self.cmd,
                buf.buffer,
                self.image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &regions,
            );
            // Release to the outside world: the compositor or GL reads next.
            let release = vk::ImageMemoryBarrier::default()
                .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                .dst_access_mask(vk::AccessFlags::empty())
                .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                .new_layout(vk::ImageLayout::GENERAL)
                .src_queue_family_index(d.qfam)
                .dst_queue_family_index(vk::QUEUE_FAMILY_EXTERNAL)
                .image(self.image)
                .subresource_range(Self::range());
            d.device.cmd_pipeline_barrier(
                self.cmd,
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::BOTTOM_OF_PIPE,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[release],
            );
            d.device
                .end_command_buffer(self.cmd)
                .map_err(|e| e.to_string())?;
        }
        self.submit()
    }

    fn readback(&self, w: u32, h: u32) -> Result<Vec<u8>, String> {
        let d = &self.dev;
        let size = u64::from(w) * u64::from(h) * 4;
        unsafe {
            let buf = d
                .device
                .create_buffer(
                    &vk::BufferCreateInfo::default()
                        .size(size)
                        .usage(vk::BufferUsageFlags::TRANSFER_DST),
                    None,
                )
                .map_err(|e| e.to_string())?;
            let need = d.device.get_buffer_memory_requirements(buf);
            let want =
                vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT;
            let ty = (0..d.mem.memory_type_count)
                .find(|&i| {
                    need.memory_type_bits & (1 << i) != 0
                        && d.mem.memory_types[i as usize].property_flags.contains(want)
                })
                .ok_or("no host memory")?;
            let mem = d
                .device
                .allocate_memory(
                    &vk::MemoryAllocateInfo::default()
                        .allocation_size(need.size)
                        .memory_type_index(ty),
                    None,
                )
                .map_err(|e| e.to_string())?;
            d.device
                .bind_buffer_memory(buf, mem, 0)
                .map_err(|e| e.to_string())?;
            d.device
                .reset_command_buffer(self.cmd, vk::CommandBufferResetFlags::empty())
                .map_err(|e| e.to_string())?;
            d.device
                .begin_command_buffer(
                    self.cmd,
                    &vk::CommandBufferBeginInfo::default()
                        .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
                )
                .map_err(|e| e.to_string())?;
            let barrier = |old, new| {
                vk::ImageMemoryBarrier::default()
                    .src_access_mask(vk::AccessFlags::MEMORY_WRITE)
                    .dst_access_mask(vk::AccessFlags::TRANSFER_READ | vk::AccessFlags::MEMORY_READ)
                    .old_layout(old)
                    .new_layout(new)
                    .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .image(self.image)
                    .subresource_range(Self::range())
            };
            d.device.cmd_pipeline_barrier(
                self.cmd,
                vk::PipelineStageFlags::ALL_COMMANDS,
                vk::PipelineStageFlags::TRANSFER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[barrier(
                    vk::ImageLayout::GENERAL,
                    vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                )],
            );
            d.device.cmd_copy_image_to_buffer(
                self.cmd,
                self.image,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                buf,
                &[vk::BufferImageCopy {
                    buffer_offset: 0,
                    buffer_row_length: 0,
                    buffer_image_height: 0,
                    image_subresource: vk::ImageSubresourceLayers {
                        aspect_mask: vk::ImageAspectFlags::COLOR,
                        mip_level: 0,
                        base_array_layer: 0,
                        layer_count: 1,
                    },
                    image_offset: vk::Offset3D::default(),
                    image_extent: vk::Extent3D {
                        width: w,
                        height: h,
                        depth: 1,
                    },
                }],
            );
            d.device.cmd_pipeline_barrier(
                self.cmd,
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::ALL_COMMANDS,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[barrier(
                    vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                    vk::ImageLayout::GENERAL,
                )],
            );
            d.device
                .end_command_buffer(self.cmd)
                .map_err(|e| e.to_string())?;
            self.submit_wait()?;
            let p = d
                .device
                .map_memory(mem, 0, vk::WHOLE_SIZE, vk::MemoryMapFlags::empty())
                .map_err(|e| e.to_string())?;
            let out = std::slice::from_raw_parts(p as *const u8, size as usize).to_vec();
            d.device.unmap_memory(mem);
            d.device.destroy_buffer(buf, None);
            d.device.free_memory(mem, None);
            Ok(out)
        }
    }
}

struct PoolGuard {
    dev: Arc<Dev>,
    pool: Option<vk::CommandPool>,
}

impl Drop for PoolGuard {
    fn drop(&mut self) {
        if let Some(p) = self.pool {
            // SAFETY: no command buffer from it is in flight; creation failed.
            unsafe { self.dev.device.destroy_command_pool(p, None) };
        }
    }
}

/// A fence that outlives its slot while a worker thread still waits on it.
struct FenceOwner {
    dev: Arc<Dev>,
    fence: vk::Fence,
}

impl Drop for FenceOwner {
    fn drop(&mut self) {
        // SAFETY: the last holder; the device is alive through `dev`.
        unsafe { self.dev.device.destroy_fence(self.fence, None) };
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        // SAFETY: the owner waited for the device to idle.
        unsafe {
            self.dev.device.destroy_image(self.image, None);
            self.dev.device.free_memory(self.memory, None);
        }
    }
}

/// The union of two sorted, disjoint run lists, clamped to `height`, merged
/// where runs touch. More than 64 runs collapse to their bounding span: a
/// copy region costs more than the few untouched lines between them.
fn merge_runs(a: &[(u32, u32)], b: &[(u32, u32)], height: u32) -> Vec<(u32, u32)> {
    let mut all: Vec<(u32, u32)> = a
        .iter()
        .chain(b)
        .map(|&(x, y)| (x.min(height), y.min(height)))
        .filter(|(x, y)| y > x)
        .collect();
    all.sort_unstable();
    let mut out: Vec<(u32, u32)> = Vec::with_capacity(all.len());
    for (x, y) in all {
        match out.last_mut() {
            Some(last) if x <= last.1 => last.1 = last.1.max(y),
            _ => out.push((x, y)),
        }
    }
    if out.len() > 64 {
        out = vec![(out[0].0, out[out.len() - 1].1)];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::merge_runs;

    #[test]
    fn runs_union_and_touching_runs_join() {
        assert_eq!(
            merge_runs(&[(0, 4), (10, 12)], &[(4, 6), (11, 20)], 100),
            vec![(0, 6), (10, 20)]
        );
    }

    #[test]
    fn runs_are_clamped_and_empty_ones_dropped() {
        assert_eq!(merge_runs(&[], &[(90, 200), (5, 5)], 100), vec![(90, 100)]);
    }

    #[test]
    fn many_runs_collapse_to_a_span() {
        let many: Vec<(u32, u32)> = (0..100).map(|i| (i * 4, i * 4 + 1)).collect();
        assert_eq!(merge_runs(&many, &[], 1000), vec![(0, 397)]);
    }
}
