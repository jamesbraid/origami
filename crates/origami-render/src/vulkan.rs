//! The Vulkan half of importing: open the device with the external-memory
//! extension, and turn a handle QEMU exported into a buffer on it.
//!
//! The same code serves Linux (`OPAQUE_FD`) and Windows (`OPAQUE_WIN32`); only
//! the handle type and the import structure differ. wgpu has no API for
//! this, so the device is opened through its hal (a callback adds the
//! extension), the buffer is made with raw calls, and wgpu is handed the
//! result.

use ash::vk;
use origami_display::proto::Region;
use origami_display::Handle;
#[cfg(unix)]
use origami_display::Memory;
use std::io;
#[cfg(unix)]
use std::sync::Arc;
use wgpu::hal::api::Vulkan;

#[cfg(unix)]
const HANDLE_TYPE: vk::ExternalMemoryHandleTypeFlags = vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD;
#[cfg(windows)]
const HANDLE_TYPE: vk::ExternalMemoryHandleTypeFlags =
    vk::ExternalMemoryHandleTypeFlags::OPAQUE_WIN32;

#[cfg(unix)]
const HANDLE_EXTENSION: &std::ffi::CStr = ash::khr::external_memory_fd::NAME;
#[cfg(windows)]
const HANDLE_EXTENSION: &std::ffi::CStr = ash::khr::external_memory_win32::NAME;

/// What QEMU's allocation is made for: the same usage it creates the buffer
/// with, so the import is of a compatible buffer.
fn usage() -> vk::BufferUsageFlags {
    vk::BufferUsageFlags::STORAGE_BUFFER | vk::BufferUsageFlags::TRANSFER_SRC
}

/// What the opened device offers.
#[derive(Clone, Debug)]
pub struct Caps {
    pub device_uuid: [u8; 16],
    pub driver_uuid: [u8; 16],
    pub name: String,
    /// `minImportedHostPointerAlignment` where the device can import host
    /// memory (`VK_EXT_external_memory_host`), else None.
    pub host_align: Option<u64>,
}

impl Caps {
    pub fn describe(&self) -> String {
        format!("{}, device {}", self.name, hex(&self.device_uuid))
    }

    /// Memory exported on a different adapter or driver cannot be imported.
    pub fn accepts(&self, device_uuid: &[u8; 16], driver_uuid: &[u8; 16]) -> Result<(), String> {
        if *device_uuid != self.device_uuid {
            return Err(format!(
                "QEMU exported on adapter {}, this viewer draws on {}",
                hex(device_uuid),
                hex(&self.device_uuid)
            ));
        }
        if *driver_uuid != self.driver_uuid {
            return Err(format!(
                "QEMU's driver {} is not this viewer's {}",
                hex(driver_uuid),
                hex(&self.driver_uuid)
            ));
        }
        Ok(())
    }
}

pub fn hex(b: &[u8; 16]) -> String {
    b.iter().map(|b| format!("{b:02x}")).collect()
}

/// What the adapter must have for an import, by feature: Vulkan 1.1, the
/// extension for this host's handle type, and the handle type importable for
/// a storage buffer.
fn probe(adapter: &wgpu::hal::vulkan::Adapter) -> Result<Caps, String> {
    let instance = adapter.shared_instance();
    if instance.instance_api_version() < vk::API_VERSION_1_1 {
        return Err("the Vulkan instance is older than 1.1".into());
    }
    let raw = instance.raw_instance();
    let phys = adapter.raw_physical_device();
    let caps = adapter.physical_device_capabilities();
    if !caps.supports_extension(HANDLE_EXTENSION) {
        return Err(format!(
            "the device lacks {}",
            HANDLE_EXTENSION.to_string_lossy()
        ));
    }
    // SAFETY: `phys` belongs to `raw`; both outlive the calls.
    unsafe {
        let mut id = vk::PhysicalDeviceIDProperties::default();
        let mut props = vk::PhysicalDeviceProperties2::default().push_next(&mut id);
        raw.get_physical_device_properties2(phys, &mut props);
        let p = props.properties;
        if p.api_version < vk::API_VERSION_1_1 {
            return Err("the Vulkan device is older than 1.1".into());
        }
        let name = p
            .device_name_as_c_str()
            .map_or_else(|_| String::new(), |n| n.to_string_lossy().into_owned());
        let info = vk::PhysicalDeviceExternalBufferInfo::default()
            .usage(usage())
            .handle_type(HANDLE_TYPE);
        let mut out = vk::ExternalBufferProperties::default();
        raw.get_physical_device_external_buffer_properties(phys, &info, &mut out);
        let features = out.external_memory_properties.external_memory_features;
        if !features.contains(vk::ExternalMemoryFeatureFlags::IMPORTABLE) {
            return Err(
                "a storage buffer's memory cannot be imported from this handle type".into(),
            );
        }
        let host_align = caps
            .supports_extension(ash::ext::external_memory_host::NAME)
            .then(|| {
                let mut host = vk::PhysicalDeviceExternalMemoryHostPropertiesEXT::default();
                let mut props = vk::PhysicalDeviceProperties2::default().push_next(&mut host);
                raw.get_physical_device_properties2(phys, &mut props);
                host.min_imported_host_pointer_alignment
            })
            .filter(|a| *a > 0);
        Ok(Caps {
            device_uuid: id.device_uuid,
            driver_uuid: id.driver_uuid,
            name,
            host_align,
        })
    }
}

/// Open the device through the hal, adding the extensions an import needs.
pub fn open_device(
    adapter: &wgpu::Adapter,
    descriptor: &wgpu::DeviceDescriptor<'_>,
) -> Result<(wgpu::Device, wgpu::Queue, Caps), String> {
    // SAFETY: the adapter is a Vulkan one (the caller checked) and the hal
    // adapter is only read here.
    let hal_adapter = unsafe { adapter.as_hal::<Vulkan>() }.ok_or("not a Vulkan adapter")?;
    let caps = probe(&hal_adapter)?;
    let base = hal_adapter
        .physical_device_capabilities()
        .supports_extension(ash::khr::external_memory::NAME);
    let host = caps.host_align.is_some();
    let callback = Box::new(
        move |args: wgpu::hal::vulkan::CreateDeviceCallbackArgs<'_, '_, '_>| {
            // `VK_KHR_external_memory` is core in 1.1 and a no-op to name there;
            // naming the handle extension needs it listed on older loaders.
            let wanted = base
                .then_some(ash::khr::external_memory::NAME)
                .into_iter()
                .chain([HANDLE_EXTENSION])
                .chain(host.then_some(ash::ext::external_memory_host::NAME));
            for ext in wanted {
                if !args.extensions.contains(&ext) {
                    args.extensions.push(ext);
                }
            }
        },
    );
    // SAFETY: the features and limits are the ones the adapter was asked for.
    let open = unsafe {
        hal_adapter.open_with_callback(
            descriptor.required_features,
            &descriptor.required_limits,
            &descriptor.memory_hints,
            Some(callback),
        )
    }
    .map_err(|e| format!("cannot open the Vulkan device with the import extension: {e:?}"))?;
    drop(hal_adapter);
    // SAFETY: `open` was opened on this adapter with these features.
    let (device, queue) =
        pollster::block_on(async { unsafe { adapter.create_device_from_hal(open, descriptor) } })
            .map_err(|e| format!("cannot open the GPU: {e}"))?;
    // The extension must be on the device that came out, not only asked for.
    // SAFETY: only reads the hal device's list.
    let enabled = unsafe { device.as_hal::<Vulkan>() }
        .map(|d| d.enabled_device_extensions().contains(&HANDLE_EXTENSION))
        .unwrap_or(false);
    if !enabled {
        return Err("the device came up without the import extension".into());
    }
    Ok((device, queue, caps))
}

fn bad(why: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, why.into())
}

/// Import the memory QEMU exported for `region` as a storage buffer.
///
/// The handle is consumed: on success the driver owns a descriptor (Linux) or
/// the handle is closed after (Windows); on failure it is closed here.
pub fn import(
    device: &wgpu::Device,
    caps: &Caps,
    region: &Region,
    handle: Handle,
) -> io::Result<wgpu::Buffer> {
    // SAFETY: the Vulkan calls below follow the extension's rules; the hal
    // device outlives them and the buffer and memory are released by the
    // callback below, after wgpu has dropped its buffer.
    // Importing an NT handle does not take it: it is ours to close on every
    // path out of here, including the early errors.
    #[cfg(windows)]
    let _close = {
        struct Close(u64);
        impl Drop for Close {
            fn drop(&mut self) {
                // SAFETY: a handle duplicated into this process for us to close.
                unsafe { windows_sys::Win32::Foundation::CloseHandle(self.0 as usize as _) };
            }
        }
        #[allow(irrefutable_let_patterns)]
        if let Handle::Value(v) = &handle {
            Some(Close(*v))
        } else {
            None
        }
    };
    unsafe {
        let hal = device
            .as_hal::<Vulkan>()
            .ok_or_else(|| bad("not a Vulkan device"))?;
        let raw = hal.raw_device().clone();
        let phys = hal.raw_physical_device();
        let mem_types = hal
            .shared_instance()
            .raw_instance()
            .get_physical_device_memory_properties(phys)
            .memory_type_count;
        if region.memory_type >= mem_types {
            return Err(bad(format!(
                "QEMU's memory type {} is not one of this device's {mem_types}",
                region.memory_type
            )));
        }
        let mut external = vk::ExternalMemoryBufferCreateInfo::default().handle_types(HANDLE_TYPE);
        let info = vk::BufferCreateInfo::default()
            .size(region.size)
            .usage(usage())
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .push_next(&mut external);
        let buffer = raw
            .create_buffer(&info, None)
            .map_err(|e| io::Error::other(format!("vkCreateBuffer: {e}")))?;
        let fail = |what: &str, e: vk::Result| {
            raw.destroy_buffer(buffer, None);
            io::Error::other(format!("{what}: {e} (adapter {})", hex(&caps.device_uuid)))
        };
        let need = raw.get_buffer_memory_requirements(buffer);
        if need.memory_type_bits & (1 << region.memory_type) == 0 {
            raw.destroy_buffer(buffer, None);
            return Err(bad(format!(
                "QEMU's memory type {} is not allowed for this buffer ({:#x})",
                region.memory_type, need.memory_type_bits
            )));
        }
        if region.alloc_size < need.size {
            raw.destroy_buffer(buffer, None);
            return Err(bad(format!(
                "QEMU allocated {} bytes, this buffer needs {}",
                region.alloc_size, need.size
            )));
        }
        let mut dedicated = vk::MemoryDedicatedAllocateInfo::default().buffer(buffer);
        let memory =
            allocate(&raw, region, handle, &mut dedicated).map_err(|(what, e)| fail(what, e))?;
        if let Err(e) = raw.bind_buffer_memory(buffer, memory, 0) {
            raw.free_memory(memory, None);
            return Err(fail("vkBindBufferMemory", e));
        }
        let release = {
            let raw = raw.clone();
            move || {
                raw.destroy_buffer(buffer, None);
                raw.free_memory(memory, None);
            }
        };
        let hal_buffer =
            wgpu::hal::vulkan::Buffer::from_raw_externally_owned(buffer, Box::new(release));
        drop(hal);
        Ok(device.create_buffer_from_hal::<Vulkan>(
            hal_buffer,
            &wgpu::BufferDescriptor {
                label: Some("qemu region"),
                size: region.size,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            },
        ))
    }
}

/// Import `size` bytes of mapped shared memory as a storage buffer, with no
/// copy (`VK_EXT_external_memory_host`). The buffer keeps `memory` alive.
#[cfg(unix)]
pub fn import_host(
    device: &wgpu::Device,
    caps: &Caps,
    memory: &Arc<dyn Memory>,
    size: u64,
) -> io::Result<wgpu::Buffer> {
    let align = caps
        .host_align
        .ok_or_else(|| bad("the device cannot import host memory"))?;
    let ptr = memory.base();
    if size == 0 || size > memory.len() as u64 || !size.is_multiple_of(align) {
        return Err(bad(format!(
            "{size} bytes of a {} byte mapping, imported in units of {align}",
            memory.len()
        )));
    }
    if !(ptr as u64).is_multiple_of(align) {
        return Err(bad(format!("{ptr:p} is not aligned to {align}")));
    }
    // SAFETY: the Vulkan calls follow the extension's rules; `memory` is held
    // by the release callback until wgpu drops the buffer.
    unsafe {
        let hal = device
            .as_hal::<Vulkan>()
            .ok_or_else(|| bad("not a Vulkan device"))?;
        let raw = hal.raw_device().clone();
        let instance = hal.shared_instance().raw_instance();
        let phys = hal.raw_physical_device();
        let host = ash::ext::external_memory_host::Device::new(instance, &raw);
        let kind = vk::ExternalMemoryHandleTypeFlags::HOST_ALLOCATION_EXT;
        let mut allowed = vk::MemoryHostPointerPropertiesEXT::default();
        (host.fp().get_memory_host_pointer_properties_ext)(
            raw.handle(),
            kind,
            ptr.cast(),
            &mut allowed,
        )
        .result()
        .map_err(|e| io::Error::other(format!("host pointer properties: {e}")))?;
        let mut external = vk::ExternalMemoryBufferCreateInfo::default().handle_types(kind);
        let info = vk::BufferCreateInfo::default()
            .size(size)
            .usage(usage())
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .push_next(&mut external);
        let buffer = raw
            .create_buffer(&info, None)
            .map_err(|e| io::Error::other(format!("vkCreateBuffer: {e}")))?;
        let need = raw.get_buffer_memory_requirements(buffer);
        let types = instance.get_physical_device_memory_properties(phys);
        let bits = need.memory_type_bits & allowed.memory_type_bits;
        let visible = vk::MemoryPropertyFlags::HOST_VISIBLE;
        let coherent = visible | vk::MemoryPropertyFlags::HOST_COHERENT;
        let pick = |want: vk::MemoryPropertyFlags| {
            (0..types.memory_type_count).find(|&i| {
                bits & (1 << i) != 0 && types.memory_types[i as usize].property_flags.contains(want)
            })
        };
        let Some(index) = pick(coherent).or_else(|| pick(visible)) else {
            raw.destroy_buffer(buffer, None);
            return Err(bad("no host visible memory type takes host pointers"));
        };
        if need.size > size {
            raw.destroy_buffer(buffer, None);
            return Err(bad(format!(
                "the buffer needs {} of {size} bytes",
                need.size
            )));
        }
        let mut import = vk::ImportMemoryHostPointerInfoEXT::default()
            .handle_type(kind)
            .host_pointer(ptr.cast());
        let alloc = vk::MemoryAllocateInfo::default()
            .allocation_size(size)
            .memory_type_index(index)
            .push_next(&mut import);
        let mem = match raw.allocate_memory(&alloc, None) {
            Ok(m) => m,
            Err(e) => {
                raw.destroy_buffer(buffer, None);
                return Err(io::Error::other(format!("vkAllocateMemory (host): {e}")));
            }
        };
        if let Err(e) = raw.bind_buffer_memory(buffer, mem, 0) {
            raw.free_memory(mem, None);
            raw.destroy_buffer(buffer, None);
            return Err(io::Error::other(format!("vkBindBufferMemory: {e}")));
        }
        let keep = memory.clone();
        let release = {
            let raw = raw.clone();
            move || {
                raw.destroy_buffer(buffer, None);
                raw.free_memory(mem, None);
                drop(keep);
            }
        };
        let hal_buffer =
            wgpu::hal::vulkan::Buffer::from_raw_externally_owned(buffer, Box::new(release));
        drop(hal);
        Ok(device.create_buffer_from_hal::<Vulkan>(
            hal_buffer,
            &wgpu::BufferDescriptor {
                label: Some("shared slot"),
                size,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            },
        ))
    }
}

/// `vkAllocateMemory` with the import structure for this host's handle.
#[cfg(unix)]
unsafe fn allocate(
    raw: &ash::Device,
    region: &Region,
    handle: Handle,
    dedicated: &mut vk::MemoryDedicatedAllocateInfo<'_>,
) -> Result<vk::DeviceMemory, (&'static str, vk::Result)> {
    use std::os::fd::{AsRawFd, IntoRawFd};
    #[allow(unreachable_patterns)]
    let fd = match handle {
        Handle::Fd(fd) => fd,
        _ => {
            return Err((
                "a handle that is not a descriptor",
                vk::Result::ERROR_INVALID_EXTERNAL_HANDLE,
            ))
        }
    };
    let mut import = vk::ImportMemoryFdInfoKHR::default()
        .handle_type(HANDLE_TYPE)
        .fd(fd.as_raw_fd());
    let mut info = vk::MemoryAllocateInfo::default()
        .allocation_size(region.alloc_size)
        .memory_type_index(region.memory_type)
        .push_next(&mut import);
    if region.flags & origami_display::proto::REGION_DEDICATED != 0 {
        info = info.push_next(dedicated);
    }
    match raw.allocate_memory(&info, None) {
        Ok(memory) => {
            // The driver owns the descriptor now.
            let _ = fd.into_raw_fd();
            Ok(memory)
        }
        // A failed import leaves the descriptor ours, and `fd` closes it.
        Err(e) => Err(("vkAllocateMemory (import)", e)),
    }
}

#[cfg(windows)]
unsafe fn allocate(
    raw: &ash::Device,
    region: &Region,
    handle: Handle,
    dedicated: &mut vk::MemoryDedicatedAllocateInfo<'_>,
) -> Result<vk::DeviceMemory, (&'static str, vk::Result)> {
    #[allow(irrefutable_let_patterns)]
    let Handle::Value(value) = handle
    else {
        return Err((
            "a handle that is not a value",
            vk::Result::ERROR_INVALID_EXTERNAL_HANDLE,
        ));
    };
    let nt = value as usize as vk::HANDLE;
    let mut import = vk::ImportMemoryWin32HandleInfoKHR::default()
        .handle_type(HANDLE_TYPE)
        .handle(nt);
    let mut info = vk::MemoryAllocateInfo::default()
        .allocation_size(region.alloc_size)
        .memory_type_index(region.memory_type)
        .push_next(&mut import);
    if region.flags & origami_display::proto::REGION_DEDICATED != 0 {
        info = info.push_next(dedicated);
    }
    let result = raw.allocate_memory(&info, None);
    result.map_err(|e| ("vkAllocateMemory (import)", e))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::{Compose, Gpu, HeadTexture, ImportedBuffer};
    use origami_display::proto::REGION_DEDICATED;
    use origami_engine::{FrameUpdate, GpuMemory, ImportedUpdate};
    use std::os::fd::{FromRawFd, OwnedFd};
    use std::sync::Arc;

    /// Memory exported the way QEMU exports a region: host-visible and
    /// coherent, a dedicated allocation, its descriptor and the numbers an
    /// import needs.
    struct Exported {
        fd: Option<OwnedFd>,
        region: Region,
        mapped: *mut u8,
        raw: ash::Device,
        buffer: vk::Buffer,
        memory: vk::DeviceMemory,
    }

    impl Drop for Exported {
        fn drop(&mut self) {
            // SAFETY: made by `export`, destroyed once, after the device
            // finished with it (the tests wait for the queue).
            unsafe {
                self.raw.destroy_buffer(self.buffer, None);
                self.raw.free_memory(self.memory, None);
            }
        }
    }

    fn export(device: &wgpu::Device, size: u64) -> Option<Exported> {
        // SAFETY: Vulkan calls on the device's own handles.
        unsafe {
            let hal = device.as_hal::<Vulkan>()?;
            let raw = hal.raw_device().clone();
            let inst = hal.shared_instance().raw_instance();
            let phys = hal.raw_physical_device();
            let mut ext = vk::ExternalMemoryBufferCreateInfo::default().handle_types(HANDLE_TYPE);
            let buffer = raw
                .create_buffer(
                    &vk::BufferCreateInfo::default()
                        .size(size)
                        .usage(usage())
                        .push_next(&mut ext),
                    None,
                )
                .ok()?;
            let need = raw.get_buffer_memory_requirements(buffer);
            let types = inst.get_physical_device_memory_properties(phys);
            let want =
                vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT;
            let index = (0..types.memory_type_count)
                .filter(|&i| need.memory_type_bits & (1 << i) != 0)
                .find(|&i| types.memory_types[i as usize].property_flags.contains(want))?;
            let mut export = vk::ExportMemoryAllocateInfo::default().handle_types(HANDLE_TYPE);
            let mut dedicated = vk::MemoryDedicatedAllocateInfo::default().buffer(buffer);
            let memory = raw
                .allocate_memory(
                    &vk::MemoryAllocateInfo::default()
                        .allocation_size(need.size)
                        .memory_type_index(index)
                        .push_next(&mut export)
                        .push_next(&mut dedicated),
                    None,
                )
                .ok()?;
            raw.bind_buffer_memory(buffer, memory, 0).ok()?;
            let mapped = raw
                .map_memory(memory, 0, vk::WHOLE_SIZE, vk::MemoryMapFlags::empty())
                .ok()?
                .cast::<u8>();
            std::ptr::write_bytes(mapped, 0, need.size as usize);
            let fd = ash::khr::external_memory_fd::Device::new(inst, &raw)
                .get_memory_fd(
                    &vk::MemoryGetFdInfoKHR::default()
                        .memory(memory)
                        .handle_type(HANDLE_TYPE),
                )
                .ok()?;
            Some(Exported {
                fd: Some(OwnedFd::from_raw_fd(fd)),
                region: Region {
                    id: 1,
                    kind: 1,
                    backend: 1, // VK_FD
                    memory_type: index,
                    size,
                    alloc_size: need.size,
                    flags: REGION_DEDICATED,
                    handle: 0,
                },
                mapped,
                raw,
                buffer,
                memory,
            })
        }
    }

    fn read_back(gpu: &Gpu, t: &wgpu::Texture) -> Vec<u8> {
        let (w, h) = (t.width(), t.height());
        let padded = (w * 4).next_multiple_of(256);
        let buf = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: u64::from(padded * h),
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
                    bytes_per_row: Some(padded),
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
        let mut rgb = Vec::new();
        for row in data.chunks(padded as usize).take(h as usize) {
            for px in row[..(w * 4) as usize].chunks(4) {
                rgb.extend_from_slice(&[px[2], px[1], px[0]]);
            }
        }
        rgb
    }

    #[test]
    fn exported_memory_is_imported_and_drawn_with_no_copy() {
        let gpu = match Gpu::new() {
            Ok(g) if matches!(g.import.kind, Some(crate::import::Kind::Vulkan(_))) => g,
            other => {
                eprintln!(
                    "skipping: {}",
                    other.map_or_else(|e| e, |g| g.import.describe())
                );
                return;
            }
        };
        let Some(crate::import::Kind::Vulkan(caps)) = gpu.import.kind.as_ref() else {
            unreachable!()
        };
        let (w, h, stride, offset) = (40u32, 24u32, 192usize, 256usize);
        let Some(mut exported) = export(&gpu.device, 1 << 16) else {
            eprintln!("skipping: this device cannot export host-visible memory");
            return;
        };
        // What WELCOME names must be accepted, and anything else refused.
        assert!(caps.accepts(&caps.device_uuid, &caps.driver_uuid).is_ok());
        assert!(caps.accepts(&[0; 16], &caps.driver_uuid).is_err());
        assert!(caps.accepts(&caps.device_uuid, &[0; 16]).is_err());

        let fd = exported.fd.take().unwrap();
        let buffer = import(&gpu.device, caps, &exported.region, Handle::Fd(fd)).expect("import");
        let region: Arc<dyn GpuMemory> = Arc::new(ImportedBuffer::new(buffer));
        let compose = Compose::new(&gpu);
        let word = |x: u32, y: u32| 0x0001_0203 + x * 0x0001_0000 + y * 0x0000_0100;
        let paint = |f: &dyn Fn(u32, u32) -> u32| {
            for y in 0..h {
                for x in 0..w {
                    let at = offset + y as usize * stride + x as usize * 4;
                    // SAFETY: inside the mapped allocation; the GPU may be reading.
                    unsafe {
                        exported
                            .mapped
                            .add(at)
                            .cast::<u32>()
                            .write_volatile(f(x, y))
                    };
                }
            }
        };
        let expect = |f: &dyn Fn(u32, u32) -> u32| -> Vec<u8> {
            (0..h)
                .flat_map(|y| {
                    (0..w).flat_map(move |x| {
                        [(f(x, y) >> 16) as u8, (f(x, y) >> 8) as u8, f(x, y) as u8]
                    })
                })
                .collect()
        };
        let frame = |runs: Vec<(u32, u32)>, seq: u64, bottom_up: bool| ImportedUpdate {
            update: FrameUpdate {
                seq,
                width: w,
                height: h,
                runs,
                resized: false,
            },
            memory: region.clone(),
            offset: offset as u64,
            stride: stride as u32,
            bottom_up,
            swap_rb: false,
            hold: None,
        };

        // The CPU writes into the exporter's mapping; the GPU reads the same
        // memory through the import, nothing copied between.
        paint(&word);
        let head =
            HeadTexture::apply_imported(&gpu, &compose, None, &frame(vec![(0, h)], 1, false));
        assert!(
            read_back(&gpu, &head.texture) == expect(&word),
            "the first picture"
        );
        // Two lines are redrawn; a third that changed in memory is not marked
        // and stays as it was.
        let marked = |x: u32, y: u32| {
            if (4..6).contains(&y) {
                0x00ff_8040
            } else {
                word(x, y)
            }
        };
        paint(&|x, y| {
            if (4..6).contains(&y) || y == 20 {
                0x00ff_8040
            } else {
                word(x, y)
            }
        });
        let head =
            HeadTexture::apply_imported(&gpu, &compose, Some(head), &frame(vec![(4, 6)], 2, false));
        assert!(
            read_back(&gpu, &head.texture) == expect(&marked),
            "two marked lines"
        );
        // A bottom-up plane is flipped.
        let flipped = |x: u32, y: u32| {
            if (4..6).contains(&(h - 1 - y)) || h - 1 - y == 20 {
                0x00ff_8040
            } else {
                word(x, h - 1 - y)
            }
        };
        let head =
            HeadTexture::apply_imported(&gpu, &compose, Some(head), &frame(vec![(0, h)], 3, true));
        assert!(
            read_back(&gpu, &head.texture) == expect(&flipped),
            "bottom up"
        );
        assert_eq!(
            (head.bytes_uploaded, head.rows_uploaded),
            (0, 0),
            "no CPU upload"
        );
        assert_eq!(head.rows_imported, u64::from(h) * 2 + 2);

        // The imported buffer lets go of the memory when it is dropped.
        drop(head);
        drop(region);
        gpu.device.poll(wgpu::PollType::wait_indefinitely()).ok();
        assert_eq!(gpu.errors.count(), 0);
    }

    /// Page-aligned shared memory standing in for a mapped slot.
    struct Anon(*mut u8, usize);

    // SAFETY: a private anonymous mapping, valid until dropped.
    unsafe impl Send for Anon {}
    unsafe impl Sync for Anon {}
    // SAFETY: as above; page aligned.
    unsafe impl origami_display::Memory for Anon {
        fn base(&self) -> *mut u8 {
            self.0
        }
        fn len(&self) -> usize {
            self.1
        }
    }

    impl Drop for Anon {
        fn drop(&mut self) {
            // SAFETY: mapped in `host_memory`, unmapped once.
            unsafe { libc::munmap(self.0.cast(), self.1) };
        }
    }

    fn host_memory(len: usize) -> Arc<dyn origami_display::Memory> {
        // SAFETY: an anonymous shared mapping nobody else holds.
        let p = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        assert_ne!(p, libc::MAP_FAILED);
        Arc::new(Anon(p.cast(), len))
    }

    #[test]
    fn mapped_memory_is_imported_as_host_memory_and_held_until_the_gpu_is_done() {
        use std::sync::atomic::{AtomicBool, Ordering};
        struct Lent(Arc<AtomicBool>);
        impl Drop for Lent {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }
        let gpu = match Gpu::new() {
            Ok(g) if matches!(g.import.kind, Some(crate::import::Kind::Vulkan(_))) => g,
            _ => return,
        };
        let Some(crate::import::Kind::Vulkan(caps)) = gpu.import.kind.as_ref() else {
            unreachable!()
        };
        let Some(align) = caps.host_align else {
            eprintln!("skipping: this device cannot import host memory");
            return;
        };
        let len = (1usize << 16).next_multiple_of(align as usize);
        let mem = host_memory(len);
        let (w, h) = (32u32, 16u32);
        let word = |x: u32, y: u32| 0x0010_2030 + x * 0x0001_0000 + y * 0x0000_0100;
        for y in 0..h {
            for x in 0..w {
                // SAFETY: inside the mapping; the GPU has not seen it yet.
                unsafe {
                    mem.base()
                        .add((y * w + x) as usize * 4)
                        .cast::<u32>()
                        .write_volatile(word(x, y))
                };
            }
        }
        // Sizes the driver cannot take are errors, not crashes.
        assert!(import_host(&gpu.device, caps, &mem, 0).is_err());
        assert!(import_host(&gpu.device, caps, &mem, len as u64 + align).is_err());
        assert!(import_host(&gpu.device, caps, &mem, align + 1).is_err());
        let buffer = import_host(&gpu.device, caps, &mem, len as u64).expect("import");
        let region: Arc<dyn GpuMemory> = Arc::new(ImportedBuffer::new(buffer));
        let released = Arc::new(AtomicBool::new(false));
        let frame = ImportedUpdate {
            update: FrameUpdate {
                seq: 1,
                width: w,
                height: h,
                runs: vec![(0, h)],
                resized: false,
            },
            memory: region,
            offset: 0,
            stride: w * 4,
            bottom_up: false,
            swap_rb: false,
            hold: Some(Arc::new(Lent(released.clone()))),
        };
        let compose = Compose::new(&gpu);
        let head = HeadTexture::apply_imported(&gpu, &compose, None, &frame);
        let want: Vec<u8> = (0..h)
            .flat_map(|y| {
                (0..w).flat_map(move |x| {
                    let v = word(x, y);
                    [(v >> 16) as u8, (v >> 8) as u8, v as u8]
                })
            })
            .collect();
        assert!(read_back(&gpu, &head.texture) == want, "the picture");
        assert_eq!(head.bytes_uploaded, 0, "no CPU upload");
        // The same bytes as XB24 (red first) come out with red and blue swapped.
        let swapped = ImportedUpdate {
            swap_rb: true,
            hold: None,
            ..frame.clone()
        };
        let head = HeadTexture::apply_imported(&gpu, &compose, Some(head), &swapped);
        let want_swapped: Vec<u8> = want.chunks(3).flat_map(|p| [p[2], p[1], p[0]]).collect();
        assert!(
            read_back(&gpu, &head.texture) == want_swapped,
            "red and blue swapped"
        );
        // The lent slot is let go once the GPU finished, and not before the
        // caller dropped its own reference.
        drop(frame);
        let t = std::time::Instant::now();
        while !released.load(Ordering::SeqCst) {
            assert!(t.elapsed().as_secs() < 5, "the hold was never released");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        drop(head);
        gpu.device.poll(wgpu::PollType::wait_indefinitely()).ok();
        assert_eq!(gpu.errors.count(), 0);
    }

    #[test]
    fn a_bad_import_is_an_error_not_a_crash() {
        let Ok(gpu) = Gpu::new() else { return };
        let Some(crate::import::Kind::Vulkan(caps)) = gpu.import.kind.as_ref() else {
            return;
        };
        let Some(mut exported) = export(&gpu.device, 1 << 16) else {
            return;
        };
        let mut r = exported.region;
        // A memory type this device does not have.
        r.memory_type = 99;
        let e = import(
            &gpu.device,
            caps,
            &r,
            Handle::Fd(exported.fd.take().unwrap()),
        )
        .unwrap_err();
        assert!(e.to_string().contains("memory type"), "{e}");
        // An allocation smaller than the buffer needs.
        let mut r = exported.region;
        r.alloc_size = 16;
        let fd = export(&gpu.device, 1 << 16).and_then(|mut e| e.fd.take());
        let Some(fd) = fd else { return };
        assert!(import(&gpu.device, caps, &r, Handle::Fd(fd)).is_err());
    }
}
