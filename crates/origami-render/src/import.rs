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

#[cfg(target_os = "macos")]
use super::metal as native;
#[cfg(any(target_os = "linux", windows))]
use super::vulkan as native;

/// A region on the device, as a storage buffer.
pub struct ImportedBuffer {
    pub buffer: wgpu::Buffer,
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
                Ok(Arc::new(ImportedBuffer { buffer }))
            }
            _ => Err(io::Error::other("this device imports no exported memory")),
        }
    }

    #[cfg_attr(not(target_os = "macos"), allow(unused_variables))]
    fn wrap(&self, region: &Region, memory: &Arc<dyn Memory>) -> Option<Arc<dyn GpuMemory>> {
        #[cfg(target_os = "macos")]
        if let Some(Kind::Metal) = self.support.kind {
            return match native::wrap(&self.device, memory, region.size) {
                Ok(buffer) => Some(Arc::new(ImportedBuffer { buffer })),
                Err(why) => {
                    eprintln!("origami-gui: region {}: {why}; copying", region.id);
                    None
                }
            };
        }
        None
    }

    fn name(&self) -> &'static str {
        match self.support.kind {
            #[cfg(any(target_os = "linux", windows))]
            Some(Kind::Vulkan(_)) => "vulkan import",
            #[cfg(target_os = "macos")]
            Some(Kind::Metal) => "metal no-copy",
            None => "none",
        }
    }
}
