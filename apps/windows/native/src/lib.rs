//! D3D12 fallback presenter: shows the core's copy-path frames where its Vulkan renderer finds no GPU.
//! The CPU copies changed rows into an upload buffer (counted); the GPU copies that into the swap chain.
use std::ffi::c_void;
use std::mem::ManuallyDrop;
use std::sync::Mutex;
use std::time::Instant;

use windows::core::{Interface, Result, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct3D::*;
use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::System::Threading::*;

#[repr(C)]
#[derive(Default, Clone, Copy)]
pub struct Counters {
    pub presents: u64,
    /// Calls that found the sequence number unchanged and did no GPU work.
    pub skipped: u64,
    /// Picture bytes the CPU copied into the upload buffer. Non-zero here: this is the copy path.
    pub cpu_bytes_copied_picture: u64,
    /// Bytes read back for the screendump check. Not part of the picture path.
    pub readback_bytes_check_only: u64,
    pub last_mismatch: u64,
    pub last_seq: u64,
    /// Time inside `op_present`, summed, in microseconds (includes the fence wait).
    pub present_us_total: u64,
    pub present_us_max: u64,
    pub width: u32,
    pub height: u32,
    pub is_warp: u32,
    pub reserved: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Run {
    y0: u32,
    y1: u32,
}

struct Gpu {
    queue: ID3D12CommandQueue,
    alloc: ID3D12CommandAllocator,
    list: ID3D12GraphicsCommandList,
    fence: ID3D12Fence,
    event: HANDLE,
    value: u64,
}

pub struct Handle {
    gpu: Gpu,
    upload: ID3D12Resource,
    rb: ID3D12Resource,
    sc: IDXGISwapChain3,
    mem: *mut u8,
    w: u32,
    h: u32,
    pitch: u32,
    size: usize,
    c: Counters,
    last_seq: Option<u64>,
}

unsafe impl Send for Handle {}

impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            self.upload.Unmap(0, None);
            let _ = CloseHandle(self.gpu.event);
        }
    }
}

static LAST_ERROR: Mutex<String> = Mutex::new(String::new());

fn fail(step: &str, e: impl std::fmt::Display) {
    *LAST_ERROR.lock().unwrap() = format!("{step}: {e}");
}

fn barrier(res: &ID3D12Resource, from: D3D12_RESOURCE_STATES, to: D3D12_RESOURCE_STATES) -> D3D12_RESOURCE_BARRIER {
    D3D12_RESOURCE_BARRIER {
        Type: D3D12_RESOURCE_BARRIER_TYPE_TRANSITION,
        Flags: D3D12_RESOURCE_BARRIER_FLAG_NONE,
        Anonymous: D3D12_RESOURCE_BARRIER_0 {
            Transition: ManuallyDrop::new(D3D12_RESOURCE_TRANSITION_BARRIER {
                // A bitwise copy: no AddRef, and ManuallyDrop means no Release either.
                pResource: ManuallyDrop::new(Some(unsafe { std::mem::transmute_copy(res) })),
                Subresource: D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES,
                StateBefore: from,
                StateAfter: to,
            }),
        },
    }
}

fn footprint_loc(res: &ID3D12Resource, w: u32, h: u32, pitch: u32) -> D3D12_TEXTURE_COPY_LOCATION {
    D3D12_TEXTURE_COPY_LOCATION {
        pResource: ManuallyDrop::new(Some(unsafe { std::mem::transmute_copy(res) })),
        Type: D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT,
        Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
            PlacedFootprint: D3D12_PLACED_SUBRESOURCE_FOOTPRINT {
                Offset: 0,
                Footprint: D3D12_SUBRESOURCE_FOOTPRINT {
                    Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    Width: w,
                    Height: h,
                    Depth: 1,
                    RowPitch: pitch,
                },
            },
        },
    }
}

fn tex_loc(res: &ID3D12Resource) -> D3D12_TEXTURE_COPY_LOCATION {
    D3D12_TEXTURE_COPY_LOCATION {
        pResource: ManuallyDrop::new(Some(unsafe { std::mem::transmute_copy(res) })),
        Type: D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX,
        Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 { SubresourceIndex: 0 },
    }
}

fn buffer_desc(size: u64, flags: D3D12_RESOURCE_FLAGS) -> D3D12_RESOURCE_DESC {
    D3D12_RESOURCE_DESC {
        Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
        Alignment: 0,
        Width: size,
        Height: 1,
        DepthOrArraySize: 1,
        MipLevels: 1,
        Format: DXGI_FORMAT_UNKNOWN,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
        Flags: flags,
    }
}

impl Gpu {
    fn wait(&mut self) -> Result<()> {
        self.value += 1;
        unsafe {
            self.queue.Signal(&self.fence, self.value)?;
            if self.fence.GetCompletedValue() < self.value {
                self.fence.SetEventOnCompletion(self.value, self.event)?;
                if WaitForSingleObject(self.event, 10_000) != WAIT_OBJECT_0 {
                    return Err(windows::core::Error::new(E_FAIL, "GPU fence timed out"));
                }
            }
        }
        Ok(())
    }
}

fn buffer_resource(dev: &ID3D12Device, heap: D3D12_HEAP_TYPE, state: D3D12_RESOURCE_STATES, size: usize) -> Result<ID3D12Resource> {
    let props = D3D12_HEAP_PROPERTIES { Type: heap, ..Default::default() };
    let mut r: Option<ID3D12Resource> = None;
    unsafe {
        dev.CreateCommittedResource(&props, D3D12_HEAP_FLAG_NONE, &buffer_desc(size as u64, D3D12_RESOURCE_FLAG_NONE), state, None, &mut r)?;
    }
    Ok(r.unwrap())
}

fn create(hwnd: HWND, w: u32, h: u32, warp: bool) -> Result<Handle> {
    unsafe {
        let factory: IDXGIFactory4 = CreateDXGIFactory2(DXGI_CREATE_FACTORY_FLAGS(0))?;
        let pick = |warp: bool| -> Result<(IDXGIAdapter, ID3D12Device)> {
            let adapter: IDXGIAdapter = if warp { factory.EnumWarpAdapter::<IDXGIAdapter>()? } else { factory.EnumAdapters(0)? };
            let mut dev: Option<ID3D12Device> = None;
            D3D12CreateDevice(&adapter, D3D_FEATURE_LEVEL_11_0, &mut dev)?;
            Ok((adapter, dev.unwrap()))
        };
        let (adapter, dev) = pick(warp).or_else(|_| pick(true))?;
        let pitch = (w * 4 + 255) & !255; // footprint copies need a 256-byte row pitch
        let size = pitch as usize * h as usize;
        let upload = buffer_resource(&dev, D3D12_HEAP_TYPE_UPLOAD, D3D12_RESOURCE_STATE_GENERIC_READ, size)?;
        let mut p: *mut c_void = std::ptr::null_mut();
        upload.Map(0, None, Some(&mut p))?; // stays mapped for the life of the handle
        let rb = buffer_resource(&dev, D3D12_HEAP_TYPE_READBACK, D3D12_RESOURCE_STATE_COPY_DEST, size)?;
        let queue: ID3D12CommandQueue = dev.CreateCommandQueue(&D3D12_COMMAND_QUEUE_DESC {
            Type: D3D12_COMMAND_LIST_TYPE_DIRECT,
            ..Default::default()
        })?;
        let alloc: ID3D12CommandAllocator = dev.CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_DIRECT)?;
        let list: ID3D12GraphicsCommandList = dev.CreateCommandList(0, D3D12_COMMAND_LIST_TYPE_DIRECT, &alloc, None)?;
        list.Close()?;
        let fence: ID3D12Fence = dev.CreateFence(0, D3D12_FENCE_FLAG_NONE)?;
        let event = CreateEventW(None, false, false, PCWSTR::null())?;
        let f2: IDXGIFactory2 = factory.cast()?;
        // Stretch: the window is the letterboxed size, the picture is the head's size.
        let d = DXGI_SWAP_CHAIN_DESC1 {
            Width: w,
            Height: h,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            Stereo: false.into(),
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
            BufferCount: 2,
            Scaling: DXGI_SCALING_STRETCH,
            SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
            AlphaMode: DXGI_ALPHA_MODE_UNSPECIFIED,
            Flags: 0,
        };
        let sc: IDXGISwapChain3 = f2.CreateSwapChainForHwnd(&queue, hwnd, &d, None, None)?.cast()?;
        let desc = adapter.GetDesc()?;
        let mut c = Counters { width: w, height: h, ..Default::default() };
        c.is_warp = u32::from(String::from_utf16_lossy(&desc.Description).contains("Basic Render"));
        Ok(Handle {
            gpu: Gpu { queue, alloc, list, fence, event, value: 0 },
            upload,
            rb,
            sc,
            mem: p as *mut u8,
            w,
            h,
            pitch,
            size,
            c,
            last_seq: None,
        })
    }
}

impl Handle {
    /// Copy the changed rows (tight, one run after another) into the upload buffer.
    unsafe fn apply(&mut self, pixels: *const u8, nbytes: usize, runs: &[Run]) -> bool {
        let row = self.w as usize * 4;
        let mut off = 0usize;
        for r in runs {
            if r.y1 > self.h || r.y0 > r.y1 {
                return false;
            }
            for y in r.y0..r.y1 {
                if off + row > nbytes {
                    return false;
                }
                std::ptr::copy_nonoverlapping(pixels.add(off), self.mem.add(y as usize * self.pitch as usize), row);
                off += row;
            }
        }
        self.c.cpu_bytes_copied_picture += off as u64;
        true
    }

    fn present(&mut self, seq: u64, verify: bool) -> Result<i32> {
        if self.last_seq == Some(seq) && !verify {
            self.c.skipped += 1;
            return Ok(0);
        }
        let t = Instant::now();
        unsafe {
            let bb: ID3D12Resource = self.sc.GetBuffer(self.sc.GetCurrentBackBufferIndex())?;
            let g = &mut self.gpu;
            g.alloc.Reset()?;
            g.list.Reset(&g.alloc, None)?;
            g.list.ResourceBarrier(&[barrier(&bb, D3D12_RESOURCE_STATE_PRESENT, D3D12_RESOURCE_STATE_COPY_DEST)]);
            g.list.CopyTextureRegion(&tex_loc(&bb), 0, 0, 0, &footprint_loc(&self.upload, self.w, self.h, self.pitch), None);
            if verify {
                g.list.ResourceBarrier(&[barrier(&bb, D3D12_RESOURCE_STATE_COPY_DEST, D3D12_RESOURCE_STATE_COPY_SOURCE)]);
                g.list.CopyTextureRegion(&footprint_loc(&self.rb, self.w, self.h, self.pitch), 0, 0, 0, &tex_loc(&bb), None);
                g.list.ResourceBarrier(&[barrier(&bb, D3D12_RESOURCE_STATE_COPY_SOURCE, D3D12_RESOURCE_STATE_PRESENT)]);
            } else {
                g.list.ResourceBarrier(&[barrier(&bb, D3D12_RESOURCE_STATE_COPY_DEST, D3D12_RESOURCE_STATE_PRESENT)]);
            }
            g.list.Close()?;
            let l: ID3D12CommandList = g.list.cast()?;
            g.queue.ExecuteCommandLists(&[Some(l)]);
            g.wait()?;
            if verify {
                self.c.readback_bytes_check_only += self.size as u64;
                let mut p: *mut c_void = std::ptr::null_mut();
                self.rb.Map(0, None, Some(&mut p))?;
                let row = self.w as usize * 4;
                let mut bad = 0u64;
                for y in 0..self.h as usize {
                    let a = std::slice::from_raw_parts((p as *const u8).add(y * self.pitch as usize), row);
                    let b = std::slice::from_raw_parts(self.mem.add(y * self.pitch as usize), row);
                    bad += a.chunks(4).zip(b.chunks(4)).filter(|(x, y)| x[..3] != y[..3]).count() as u64;
                }
                self.rb.Unmap(0, None);
                self.c.last_mismatch = bad;
            }
            self.sc.Present(0, DXGI_PRESENT(0)).ok()?;
        }
        self.last_seq = Some(seq);
        self.c.last_seq = seq;
        self.c.presents += 1;
        let us = t.elapsed().as_micros() as u64;
        self.c.present_us_total += us;
        self.c.present_us_max = self.c.present_us_max.max(us);
        Ok(1)
    }
}

fn guard<T>(step: &str, default: T, f: impl FnOnce() -> Result<T>) -> T {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => {
            fail(step, e);
            default
        }
        Err(_) => {
            fail(step, "panic");
            default
        }
    }
}

/// A presenter for `hwnd` showing a `w` by `h` picture. Null on failure; `op_last_error` says why.
/// The window must outlive the handle.
#[no_mangle]
pub unsafe extern "C" fn op_create(hwnd: *mut c_void, w: u32, h: u32, warp: i32) -> *mut Handle {
    guard("op_create", std::ptr::null_mut(), || Ok(Box::into_raw(Box::new(create(HWND(hwnd), w, h, warp != 0)?))))
}

/// Applies `nruns` changed-line runs (`y0`, `y1` pairs) whose rows are in `pixels`, tight, as
/// `origami_frame_take` writes them. Returns 0, or -1 when the buffer is shorter than the runs say.
#[no_mangle]
pub unsafe extern "C" fn op_apply_rows(h: *mut Handle, pixels: *const u8, nbytes: usize, runs: *const u32, nruns: usize) -> i32 {
    if h.is_null() || pixels.is_null() || (runs.is_null() && nruns > 0) {
        return -1;
    }
    let runs = if nruns == 0 { &[][..] } else { std::slice::from_raw_parts(runs as *const Run, nruns) };
    if (*h).apply(pixels, nbytes, runs) { 0 } else { -1 }
}

/// Copies the upload buffer to the back buffer on the GPU and presents, unless `seq` is the one
/// already shown. With `verify`, also reads the frame back for the check.
/// Returns 1 presented, 0 skipped, -1 error.
#[no_mangle]
pub unsafe extern "C" fn op_present(h: *mut Handle, seq: u64, verify: i32) -> i32 {
    if h.is_null() {
        return -1;
    }
    guard("op_present", -1, || (*h).present(seq, verify != 0))
}

/// Copies the last verified frame, tight (BGRX, `w * 4` bytes a row), into `out`. Returns bytes written.
#[no_mangle]
pub unsafe extern "C" fn op_screendump(h: *mut Handle, out: *mut u8, len: usize) -> isize {
    if h.is_null() || out.is_null() {
        return -1;
    }
    guard("op_screendump", -1, || {
        let h = &*h;
        let row = h.w as usize * 4;
        if len < row * h.h as usize {
            return Ok(-1);
        }
        let mut p: *mut c_void = std::ptr::null_mut();
        h.rb.Map(0, None, Some(&mut p))?;
        for y in 0..h.h as usize {
            std::ptr::copy_nonoverlapping((p as *const u8).add(y * h.pitch as usize), out.add(y * row), row);
        }
        h.rb.Unmap(0, None);
        Ok((row * h.h as usize) as isize)
    })
}

#[no_mangle]
pub unsafe extern "C" fn op_counters(h: *mut Handle, out: *mut Counters) {
    if !h.is_null() && !out.is_null() {
        *out = (*h).c;
    }
}

#[no_mangle]
pub unsafe extern "C" fn op_destroy(h: *mut Handle) {
    if !h.is_null() {
        drop(Box::from_raw(h));
    }
}

/// Copies the last error text (UTF-8, not NUL terminated) into `out`; returns its length.
#[no_mangle]
pub unsafe extern "C" fn op_last_error(out: *mut u8, len: usize) -> usize {
    let s = LAST_ERROR.lock().unwrap();
    let n = s.len().min(len);
    std::ptr::copy_nonoverlapping(s.as_ptr(), out, n);
    n
}
