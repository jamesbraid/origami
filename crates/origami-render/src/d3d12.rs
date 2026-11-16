//! The Direct3D 12 half of importing on Windows. QEMU's region is a
//! pagefile-backed section; D3D12 opens it as a heap (no copy, no QEMU
//! change) and a buffer is placed on it for the pass to read in place.
//!
//! Not run on hardware yet: written from the Windows spike's measurements
//! (WARP) and checked only by cross-compiling.

use crate::ImportedBuffer;
use origami_display::proto::Region;
use origami_display::Handle;
use std::ffi::c_void;
use std::io;
use wgpu::hal::api::Dx12;
use windows::core::Interface;
use windows::Win32::Foundation::{CloseHandle, DuplicateHandle, DUPLICATE_SAME_ACCESS, HANDLE};
use windows::Win32::Graphics::Direct3D12::{
    ID3D12Device, ID3D12Device3, ID3D12Heap, ID3D12Resource, D3D12_FEATURE_DATA_EXISTING_HEAPS,
    D3D12_FEATURE_EXISTING_HEAPS, D3D12_RESOURCE_DESC, D3D12_RESOURCE_DIMENSION_BUFFER,
    D3D12_RESOURCE_FLAG_ALLOW_CROSS_ADAPTER, D3D12_RESOURCE_STATE_COMMON,
    D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_UNKNOWN, DXGI_SAMPLE_DESC};
use windows::Win32::System::Threading::GetCurrentProcess;

fn bad(why: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, why.into())
}

fn com(what: &str, e: windows::core::Error) -> io::Error {
    io::Error::other(format!("{what}: {e}"))
}

fn device_of(device: &wgpu::Device) -> Option<ID3D12Device> {
    // SAFETY: only reads the hal device's COM pointer.
    unsafe { device.as_hal::<Dx12>() }.map(|hal| hal.raw_device().clone())
}

/// Whether the device can open existing memory as a heap.
pub fn probe(device: &wgpu::Device) -> Result<(), String> {
    let raw = device_of(device).ok_or("not a Direct3D 12 device")?;
    raw.cast::<ID3D12Device3>()
        .map_err(|_| "the device predates ID3D12Device3 (Windows 10 1703)".to_string())?;
    let mut support = D3D12_FEATURE_DATA_EXISTING_HEAPS::default();
    // SAFETY: `support` is the structure this feature query fills.
    unsafe {
        raw.CheckFeatureSupport(
            D3D12_FEATURE_EXISTING_HEAPS,
            (&mut support as *mut D3D12_FEATURE_DATA_EXISTING_HEAPS).cast::<c_void>(),
            std::mem::size_of::<D3D12_FEATURE_DATA_EXISTING_HEAPS>() as u32,
        )
    }
    .map_err(|e| format!("cannot query existing-heap support: {e}"))?;
    if support.Supported.as_bool() {
        Ok(())
    } else {
        Err("the adapter cannot open existing memory as a heap".into())
    }
}

/// What keeps the section open while a buffer is placed on it.
struct Section {
    heap: ID3D12Heap,
    handle: HANDLE,
}

// SAFETY: D3D12 objects are free threaded, and a section handle is a plain
// kernel handle.
unsafe impl Send for Section {}
unsafe impl Sync for Section {}

impl Drop for Section {
    fn drop(&mut self) {
        // SAFETY: our own duplicate, closed once.
        let _ = unsafe { CloseHandle(self.handle) };
    }
}

/// Open the section `handle` names as a heap and place a buffer of the
/// region's size on it. The handle stays the caller's.
pub fn open(device: &wgpu::Device, region: &Region, handle: &Handle) -> io::Result<ImportedBuffer> {
    let Handle::Value(value) = handle;
    let raw = device_of(device).ok_or_else(|| bad("not a Direct3D 12 device"))?;
    let dev3: ID3D12Device3 = raw.cast().map_err(|e| com("ID3D12Device3", e))?;
    let mut dup = HANDLE::default();
    // SAFETY: duplicating a handle of this process into this process; the
    // copy outlives the caller's handle, which `map` closes.
    unsafe {
        let me = GetCurrentProcess();
        DuplicateHandle(
            me,
            HANDLE(*value as usize as *mut c_void),
            me,
            &mut dup,
            0,
            false,
            DUPLICATE_SAME_ACCESS,
        )
    }
    .map_err(|e| com("DuplicateHandle", e))?;
    // From here the section owns the duplicate and closes it on every path.
    // SAFETY: `dup` is a file mapping handle; the heap keeps its own reference.
    let heap = match unsafe { dev3.OpenExistingHeapFromFileMapping::<ID3D12Heap>(dup) } {
        Ok(heap) => heap,
        Err(e) => {
            // SAFETY: our duplicate, not yet owned by a Section.
            let _ = unsafe { CloseHandle(dup) };
            return Err(com("OpenExistingHeapFromFileMapping", e));
        }
    };
    let section = Section { heap, handle: dup };
    // SAFETY: plain query of the heap just opened.
    let heap_bytes = unsafe { section.heap.GetDesc() }.SizeInBytes;
    if heap_bytes < region.size {
        return Err(bad(format!(
            "region {} is {} bytes, its section {heap_bytes}",
            region.id, region.size
        )));
    }
    // A buffer on a heap from another process's section needs the
    // cross-adapter flag, or placement fails with E_INVALIDARG.
    let desc = D3D12_RESOURCE_DESC {
        Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
        Alignment: 0,
        Width: region.size,
        Height: 1,
        DepthOrArraySize: 1,
        MipLevels: 1,
        Format: DXGI_FORMAT_UNKNOWN,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
        Flags: D3D12_RESOURCE_FLAG_ALLOW_CROSS_ADAPTER,
    };
    let mut resource: Option<ID3D12Resource> = None;
    // SAFETY: the description fits the heap (checked above).
    unsafe {
        raw.CreatePlacedResource(
            &section.heap,
            0,
            &desc,
            D3D12_RESOURCE_STATE_COMMON,
            None,
            &mut resource,
        )
    }
    .map_err(|e| com("CreatePlacedResource", e))?;
    let resource = resource.ok_or_else(|| bad("CreatePlacedResource returned no resource"))?;
    // SAFETY: the resource is a buffer of `region.size` bytes on this device.
    let hal = unsafe { wgpu::hal::dx12::Device::buffer_from_raw(resource, region.size) };
    // SAFETY: as above; wgpu owns the resource from here.
    let buffer = unsafe {
        device.create_buffer_from_hal::<Dx12>(
            hal,
            &wgpu::BufferDescriptor {
                label: Some("qemu region"),
                size: region.size,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            },
        )
    };
    Ok(ImportedBuffer {
        buffer,
        keep: Some(Box::new(section)),
    })
}
