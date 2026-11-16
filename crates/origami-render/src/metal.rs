//! The Metal half of importing: QEMU's region is page-aligned POSIX shared
//! memory, mapped here, and Metal wraps that mapping as a buffer with no copy
//! (`newBufferWithBytesNoCopy`). wgpu is handed the buffer through its hal.
//!
//! Metal needs the pointer page aligned and the length a multiple of the page
//! size (16 KiB on Apple silicon); QEMU sizes regions in 64 KiB multiples and
//! the mapping starts on a page, so both hold. Metal does not retain the
//! memory, so the buffer's deallocator holds the mapping: it stays valid for
//! as long as the GPU may read the buffer, which outlives `REGION_GONE`
//! until the last frame using it is done.

use block2::RcBlock;
use objc2_metal::{MTLDevice, MTLResourceOptions};
use origami_display::Memory;
use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::{Arc, Mutex};
use wgpu::hal::api::Metal;

/// The system's page size, which a no-copy buffer must be a multiple of.
fn page_size() -> usize {
    // SAFETY: sysconf has no preconditions.
    let n = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if n > 0 {
        n as usize
    } else {
        16384
    }
}

/// Wrap the first `size` bytes of `memory` as a storage buffer on `device`.
pub fn wrap(
    device: &wgpu::Device,
    memory: &Arc<dyn Memory>,
    size: u64,
) -> Result<wgpu::Buffer, String> {
    let page = page_size();
    let ptr = memory.base();
    let len = usize::try_from(size).map_err(|_| "region too large".to_string())?;
    if len == 0 || len > memory.len() {
        return Err(format!(
            "region says {len} bytes, the mapping has {}",
            memory.len()
        ));
    }
    if !(ptr as usize).is_multiple_of(page) || !len.is_multiple_of(page) {
        return Err(format!(
            "not page aligned: {ptr:p}, {len} bytes, pages of {page}"
        ));
    }
    let pointer = NonNull::new(ptr.cast::<c_void>()).ok_or("null mapping")?;
    // The buffer's deallocator owns a handle on the mapping and lets it go
    // when Metal releases the buffer.
    let keep = Mutex::new(Some(memory.clone()));
    let release = RcBlock::new(move |_: NonNull<c_void>, _: usize| {
        drop(keep.lock().unwrap_or_else(|e| e.into_inner()).take());
    });
    // SAFETY: the hal device is only read; the mapping outlives the buffer
    // through the deallocator.
    unsafe {
        let hal = device.as_hal::<Metal>().ok_or("not a Metal device")?;
        let raw = hal
            .raw_device()
            .newBufferWithBytesNoCopy_length_options_deallocator(
                pointer,
                len,
                MTLResourceOptions::StorageModeShared,
                Some(&release),
            )
            .ok_or("Metal refused to wrap the shared memory")?;
        let buffer = wgpu::hal::metal::Device::buffer_from_raw(raw, len as u64);
        drop(hal);
        Ok(device.create_buffer_from_hal::<Metal>(
            buffer,
            &wgpu::BufferDescriptor {
                label: Some("qemu region"),
                size: len as u64,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            },
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Compose, Gpu, HeadTexture, ImportedBuffer};
    use origami_display::Handle;
    use origami_engine::{FrameUpdate, ImportedUpdate};
    use std::os::fd::{FromRawFd, OwnedFd};

    /// A POSIX shared-memory object as QEMU makes one on macOS: created,
    /// sized, and unlinked at once so only the descriptor names it.
    fn shared_memory(bytes: usize) -> Handle {
        let name = std::ffi::CString::new(format!("/ogui{}", std::process::id())).unwrap();
        // SAFETY: plain libc calls on a fresh object we own.
        unsafe {
            libc::shm_unlink(name.as_ptr());
            let fd = libc::shm_open(
                name.as_ptr(),
                libc::O_RDWR | libc::O_CREAT | libc::O_EXCL,
                0o600,
            );
            assert!(fd >= 0, "shm_open: {}", std::io::Error::last_os_error());
            libc::shm_unlink(name.as_ptr());
            assert_eq!(
                libc::ftruncate(fd, bytes as libc::off_t),
                0,
                "{}",
                std::io::Error::last_os_error()
            );
            Handle::Fd(OwnedFd::from_raw_fd(fd))
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

    /// Write x8r8g8b8 word `f(x, y)` for every pixel of a plane in `mem`.
    fn paint(
        mem: &Arc<dyn Memory>,
        offset: usize,
        stride: usize,
        w: u32,
        h: u32,
        f: &dyn Fn(u32, u32) -> u32,
    ) {
        for y in 0..h {
            for x in 0..w {
                let at = offset + y as usize * stride + x as usize * 4;
                // SAFETY: inside the mapping; the GPU may be reading, which is
                // the point.
                unsafe { mem.base().add(at).cast::<u32>().write_volatile(f(x, y)) };
            }
        }
    }

    #[test]
    fn a_shared_memory_segment_is_read_by_the_gpu_in_place() {
        let gpu = match Gpu::new() {
            Ok(g) if g.import.kind.is_some() => g,
            other => {
                eprintln!(
                    "skipping: {}",
                    other.map_or_else(|e| e, |g| g.import.describe())
                );
                return;
            }
        };
        eprintln!("GPU {}; {}", gpu.describe(), gpu.import.describe());
        let (w, h, stride, offset) = (40u32, 24u32, 192usize, 256usize);
        let len = 4 * page_size().max(65536);
        let mem = shared_memory(len).map().expect("map");
        assert_eq!(mem.len(), len);
        let buffer = wrap(&gpu.device, &mem, len as u64).expect("wrap");
        let region: Arc<dyn origami_engine::GpuMemory> = Arc::new(ImportedBuffer::new(buffer));
        let compose = Compose::new(&gpu);
        let frame = |runs: Vec<(u32, u32)>, seq: u64| ImportedUpdate {
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
            bottom_up: false,
            swap_rb: false,
            hold: None,
        };
        let word = |x: u32, y: u32| 0x0001_0203 + x * 0x0001_0000 + y * 0x0000_0100;
        let rgb = |w: u32| [(w >> 16) as u8, (w >> 8) as u8, w as u8];
        let expect = |f: &dyn Fn(u32, u32) -> u32| -> Vec<u8> {
            (0..h)
                .flat_map(|y| (0..w).flat_map(move |x| rgb(f(x, y))))
                .collect()
        };

        // The CPU writes after the buffer was wrapped; the GPU draws those
        // bytes with nothing copied in between.
        paint(&mem, offset, stride, w, h, &word);
        let head = HeadTexture::apply_imported(&gpu, &compose, None, &frame(vec![(0, h)], 1));
        assert!(
            read_back(&gpu, &head.texture) == expect(&word),
            "the first picture"
        );

        // A change in two lines only: the rest of the texture stays as it was
        // even where the memory also changed (it was not marked).
        let changed = |x: u32, y: u32| {
            if (4..6).contains(&y) {
                0x00ff_8040
            } else {
                word(x, y)
            }
        };
        paint(&mem, offset, stride, w, h, &|x, y| {
            if (4..6).contains(&y) || y == 20 {
                0x00ff_8040
            } else {
                word(x, y)
            }
        });
        let head = HeadTexture::apply_imported(&gpu, &compose, Some(head), &frame(vec![(4, 6)], 2));
        assert!(
            read_back(&gpu, &head.texture) == expect(&changed),
            "two lines redrawn"
        );
        assert_eq!(
            (head.rows_imported, head.bytes_uploaded, head.rows_uploaded),
            (h as u64 + 2, 0, 0)
        );

        // The mapping outlives our handle on it: the buffer holds it.
        let weak = Arc::downgrade(&mem);
        drop(mem);
        assert!(
            weak.upgrade().is_some(),
            "the GPU buffer keeps the mapping alive"
        );
        let still =
            HeadTexture::apply_imported(&gpu, &compose, Some(head), &frame(vec![(20, 21)], 3));
        let all = |x: u32, y: u32| {
            if (4..6).contains(&y) || y == 20 {
                0x00ff_8040
            } else {
                word(x, y)
            }
        };
        assert!(
            read_back(&gpu, &still.texture) == expect(&all),
            "after our handle is gone"
        );
        // Dropping the last user of the buffer releases the mapping.
        drop(still);
        drop(region);
        gpu.device.poll(wgpu::PollType::wait_indefinitely()).ok();
        assert!(
            weak.upgrade().is_none(),
            "the mapping is released with the buffer"
        );
        assert_eq!(gpu.errors.count(), 0);
    }

    #[test]
    fn an_unaligned_or_short_mapping_is_refused_not_wrapped() {
        let Ok(gpu) = Gpu::new() else { return };
        let mem = shared_memory(page_size().max(65536)).map().expect("map");
        let len = mem.len() as u64;
        assert!(
            wrap(&gpu.device, &mem, len + 1).is_err(),
            "more than is mapped"
        );
        assert!(
            wrap(&gpu.device, &mem, len - 1).is_err(),
            "not a whole number of pages"
        );
        assert!(wrap(&gpu.device, &mem, 0).is_err());
    }
}
