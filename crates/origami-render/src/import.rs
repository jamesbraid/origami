//! Taking memory QEMU shares into the GPU without copying it.
//!
//! On Vulkan (Linux and Windows, one code path) QEMU allocates host-visible
//! device memory, exports it as a handle, and the device imports that handle
//! as a buffer. On Metal the shared-memory mapping is wrapped as a buffer
//! with no copy. Either way the result is a `wgpu::Buffer` the pass in
//! [`super::compose`] reads in place.
//!
//! What is detected is a capability, not a vendor: the device extension, the
//! handle type's importability for a storage buffer, and an adapter and
//! driver QEMU's export matches. Anything missing leaves the device as it
//! would be without this (and the GUI asks QEMU for plain shared memory).

use origami_display::proto::Region;
#[cfg(target_os = "macos")]
use origami_display::proto::CAP_METAL;
#[cfg(any(target_os = "linux", windows))]
use origami_display::proto::CAP_VULKAN;
use origami_display::{Handle, Importer, Memory};
use origami_engine::GpuMemory;
use std::any::Any;
use std::io;
use std::sync::Arc;

#[cfg(windows)]
use super::d3d12;
#[cfg(target_os = "macos")]
use super::metal as native;
#[cfg(any(target_os = "linux", windows))]
use super::vulkan as native;

/// A region on the device, as a storage buffer.
pub struct ImportedBuffer {
    pub buffer: wgpu::Buffer,
    /// What the buffer needs alive besides itself (a Direct3D heap).
    pub keep: Option<Box<dyn Any + Send + Sync>>,
}

impl ImportedBuffer {
    pub fn new(buffer: wgpu::Buffer) -> ImportedBuffer {
        ImportedBuffer { buffer, keep: None }
    }
}

impl GpuMemory for ImportedBuffer {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// What the device was opened able to import.
pub enum Kind {
    #[cfg(any(target_os = "linux", windows))]
    Vulkan(native::Caps),
    #[cfg(target_os = "macos")]
    Metal,
    /// Direct3D 12 opens QEMU's shared-memory section as a heap.
    #[cfg(windows)]
    D3d12,
}

pub struct ImportSupport {
    pub kind: Option<Kind>,
    /// Why the device imports nothing, for the debug output.
    pub why_not: String,
}

impl ImportSupport {
    fn none(why: impl Into<String>) -> ImportSupport {
        ImportSupport {
            kind: None,
            why_not: why.into(),
        }
    }

    /// Open the device the adapter offers, with what importing needs added
    /// when `wanted` and the adapter can.
    pub fn open_device(
        adapter: &wgpu::Adapter,
        descriptor: &wgpu::DeviceDescriptor<'_>,
        wanted: bool,
    ) -> Result<(wgpu::Device, wgpu::Queue, ImportSupport), String> {
        let plain = |why: String| {
            pollster::block_on(adapter.request_device(descriptor))
                .map(|(d, q)| (d, q, ImportSupport::none(why)))
                .map_err(|e| format!("cannot open the GPU: {e}"))
        };
        if !wanted {
            return plain(format!("off ({})", super::device::ZERO_COPY_ENV));
        }
        #[cfg(any(target_os = "linux", windows))]
        if adapter.get_info().backend == wgpu::Backend::Vulkan {
            return match native::open_device(adapter, descriptor) {
                Ok((d, q, caps)) => Ok((
                    d,
                    q,
                    ImportSupport {
                        kind: Some(Kind::Vulkan(caps)),
                        why_not: String::new(),
                    },
                )),
                Err(why) => plain(why),
            };
        }
        #[cfg(windows)]
        if adapter.get_info().backend == wgpu::Backend::Dx12 {
            let (d, q, _) = plain(String::new())?;
            let support = match d3d12::probe(&d) {
                Ok(()) => ImportSupport {
                    kind: Some(Kind::D3d12),
                    why_not: String::new(),
                },
                Err(why) => ImportSupport::none(why),
            };
            return Ok((d, q, support));
        }
        #[cfg(target_os = "macos")]
        if adapter.get_info().backend == wgpu::Backend::Metal {
            let (d, q, _) = plain(String::new())?;
            return Ok((
                d,
                q,
                ImportSupport {
                    kind: Some(Kind::Metal),
                    why_not: String::new(),
                },
            ));
        }
        plain(format!(
            "the {:?} backend cannot import memory",
            adapter.get_info().backend
        ))
    }

    /// The adapter's `deviceUUID` as `--display-gpu` and `display-shm-gpu`
    /// take it, when QEMU can export on it for this device to import.
    pub fn export_uuid(&self) -> Option<String> {
        match &self.kind {
            #[cfg(any(target_os = "linux", windows))]
            Some(Kind::Vulkan(c)) => Some(native::hex(&c.device_uuid)),
            _ => None,
        }
    }

    /// One line for the debug output.
    pub fn describe(&self) -> String {
        match &self.kind {
            #[cfg(any(target_os = "linux", windows))]
            Some(Kind::Vulkan(c)) => format!("vulkan import ({})", c.describe()),
            #[cfg(target_os = "macos")]
            Some(Kind::Metal) => "metal no-copy".to_string(),
            #[cfg(windows)]
            Some(Kind::D3d12) => "d3d12 section heap".to_string(),
            None => format!("none: {}", self.why_not),
        }
    }
}

/// Imports QEMU's regions into the device the GUI draws with.
pub struct GpuImporter {
    device: wgpu::Device,
    support: Arc<ImportSupport>,
}

impl GpuImporter {
    /// The importer for `gpu`, or None when its device imports nothing.
    pub fn new(gpu: &super::Gpu) -> Option<Arc<GpuImporter>> {
        gpu.import.kind.as_ref()?;
        Some(Arc::new(GpuImporter {
            device: gpu.device.clone(),
            support: gpu.import.clone(),
        }))
    }
}

impl Importer for GpuImporter {
    fn caps(&self) -> u32 {
        match self.support.kind {
            #[cfg(any(target_os = "linux", windows))]
            Some(Kind::Vulkan(_)) => CAP_VULKAN,
            #[cfg(target_os = "macos")]
            Some(Kind::Metal) => CAP_METAL,
            // QEMU's plain sections are what Direct3D opens: no export asked.
            #[cfg(windows)]
            Some(Kind::D3d12) => 0,
            None => 0,
        }
    }

    fn device_uuid(&self) -> [u8; 16] {
        match &self.support.kind {
            #[cfg(any(target_os = "linux", windows))]
            Some(Kind::Vulkan(c)) => c.device_uuid,
            _ => [0; 16],
        }
    }

    fn export_uuid(&self) -> Option<[u8; 16]> {
        match &self.support.kind {
            #[cfg(any(target_os = "linux", windows))]
            Some(Kind::Vulkan(c)) => Some(c.device_uuid),
            _ => None,
        }
    }

    fn accepts(&self, device_uuid: &[u8; 16], driver_uuid: &[u8; 16]) -> Result<(), String> {
        match &self.support.kind {
            #[cfg(any(target_os = "linux", windows))]
            Some(Kind::Vulkan(c)) => c.accepts(device_uuid, driver_uuid),
            _ => {
                let _ = (device_uuid, driver_uuid);
                Err("this device imports no exported memory".into())
            }
        }
    }

    #[cfg_attr(not(any(target_os = "linux", windows)), allow(unused_variables))]
    fn import(&self, region: &Region, handle: Handle) -> io::Result<Arc<dyn GpuMemory>> {
        match &self.support.kind {
            #[cfg(any(target_os = "linux", windows))]
            Some(Kind::Vulkan(c)) => {
                let buffer = native::import(&self.device, c, region, handle)?;
                Ok(Arc::new(ImportedBuffer::new(buffer)))
            }
            _ => Err(io::Error::other("this device imports no exported memory")),
        }
    }

    #[cfg_attr(not(target_os = "macos"), allow(unused_variables))]
    fn wrap(&self, region: &Region, memory: &Arc<dyn Memory>) -> Option<Arc<dyn GpuMemory>> {
        #[cfg(target_os = "macos")]
        if let Some(Kind::Metal) = self.support.kind {
            return match native::wrap(&self.device, memory, region.size) {
                Ok(buffer) => Some(Arc::new(ImportedBuffer::new(buffer))),
                Err(why) => {
                    eprintln!("origami-gui: region {}: {why}; copying", region.id);
                    None
                }
            };
        }
        None
    }

    #[cfg_attr(not(windows), allow(unused_variables))]
    fn open_section(&self, region: &Region, handle: &Handle) -> Option<Arc<dyn GpuMemory>> {
        #[cfg(windows)]
        if let Some(Kind::D3d12) = self.support.kind {
            return match d3d12::open(&self.device, region, handle) {
                Ok(buffer) => Some(Arc::new(buffer)),
                Err(why) => {
                    eprintln!("origami: region {}: {why}; copying", region.id);
                    None
                }
            };
        }
        None
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn wrap_shm(&self, size: u64, memory: &Arc<dyn Memory>) -> Option<Arc<dyn GpuMemory>> {
        let buffer = match &self.support.kind {
            #[cfg(target_os = "macos")]
            Some(Kind::Metal) => native::wrap(&self.device, memory, size),
            #[cfg(target_os = "linux")]
            Some(Kind::Vulkan(c)) => {
                native::import_host(&self.device, c, memory, size).map_err(|e| e.to_string())
            }
            _ => return None,
        };
        match buffer {
            Ok(buffer) => Some(Arc::new(ImportedBuffer::new(buffer))),
            Err(why) => {
                eprintln!("origami: shared slot: {why}; copying");
                None
            }
        }
    }

    fn shm_needs_write(&self) -> bool {
        // OpenExistingHeapFromFileMapping is measured on writable sections only.
        #[cfg(windows)]
        if let Some(Kind::D3d12) = self.support.kind {
            return true;
        }
        // The driver pins a host pointer for writing as well as reading.
        #[cfg(target_os = "linux")]
        if let Some(Kind::Vulkan(c)) = &self.support.kind {
            return c.host_align.is_some();
        }
        false
    }

    fn name(&self) -> &'static str {
        match self.support.kind {
            #[cfg(any(target_os = "linux", windows))]
            Some(Kind::Vulkan(_)) => "vulkan import",
            #[cfg(target_os = "macos")]
            Some(Kind::Metal) => "metal no-copy",
            #[cfg(windows)]
            Some(Kind::D3d12) => "d3d12 section heap",
            None => "none",
        }
    }
}
