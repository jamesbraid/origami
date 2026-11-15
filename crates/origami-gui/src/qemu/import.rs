//! How the GUI's GPU takes a region without copying it. The engine does not
//! know a graphics API: it hands each region QEMU passes to an [`Importer`]
//! and keeps what comes back beside the region's mapping.

use super::proto::Region;
use super::region::{Handle, Memory};
use crate::core::GpuMemory;
use std::io;
use std::sync::Arc;

pub trait Importer: Send + Sync {
    /// The `HELLO` capability bits this importer earns (`CAP_VULKAN`,
    /// `CAP_METAL`).
    fn caps(&self) -> u32;

    /// The adapter's Vulkan `deviceUUID` for `HELLO`, zero for Metal.
    fn device_uuid(&self) -> [u8; 16];

    /// The adapter QEMU should export on, if this importer imports exported
    /// memory at all: what the runtime passes as `display-shm-gpu`.
    fn export_uuid(&self) -> Option<[u8; 16]>;

    /// Whether memory QEMU exported on `device_uuid` and `driver_uuid` can be
    /// imported here. The error says why not.
    fn accepts(&self, device_uuid: &[u8; 16], driver_uuid: &[u8; 16]) -> Result<(), String>;

    /// Import a region QEMU allocated as exportable GPU memory (backends 1
    /// and 2). The handle is consumed either way.
    fn import(&self, region: &Region, handle: Handle) -> io::Result<Arc<dyn GpuMemory>>;

    /// Wrap a region that is plain shared memory for the GPU without copying
    /// it, where the API can (Metal). None leaves the region to the copying
    /// path.
    fn wrap(&self, region: &Region, memory: &Arc<dyn Memory>) -> Option<Arc<dyn GpuMemory>>;

    /// A few words naming the way regions reach the GPU, for the debug output.
    fn name(&self) -> &'static str;
}
