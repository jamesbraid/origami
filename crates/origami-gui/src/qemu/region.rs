//! Memory the reader maps: the control block and the regions QEMU passes as
//! handles. A [`Handle`] is what arrives with a message (a file descriptor on
//! Linux and macOS, a handle value already duplicated into this process on
//! Windows); mapping it gives an [`Memory`].

use std::io;
use std::sync::Arc;

/// Memory another process may write to at any time.
///
/// # Safety
/// `base` must point at `len` bytes, aligned to 8, valid for as long as the
/// value lives.
pub unsafe trait Memory: Send + Sync {
    fn base(&self) -> *mut u8;
    fn len(&self) -> usize;
}

/// A passed handle, not yet mapped.
pub enum Handle {
    #[cfg(unix)]
    Fd(std::os::fd::OwnedFd),
    #[cfg(windows)]
    Value(u64),
    /// Already memory, for the tests.
    #[cfg(test)]
    Memory(Arc<dyn Memory>),
}

impl Handle {
    /// Map all of it, read-write (the reader writes `armed` and clears
    /// `dirty`), and release the handle: the mapping keeps the memory alive.
    pub fn map(self) -> io::Result<Arc<dyn Memory>> {
        match self {
            #[cfg(unix)]
            Handle::Fd(fd) => {
                let file = std::fs::File::from(fd);
                // SAFETY: the mapping is only touched through atomics and
                // raw copies by this crate.
                let map = unsafe { memmap2::MmapOptions::new().map_mut(&file)? };
                Ok(Arc::new(Mapped(map)))
            }
            #[cfg(windows)]
            Handle::Value(v) => windows::map(v),
            #[cfg(test)]
            Handle::Memory(m) => Ok(m),
        }
    }
}

#[cfg(unix)]
struct Mapped(memmap2::MmapMut);

#[cfg(unix)]
// SAFETY: a MAP_SHARED mapping, page aligned, valid as long as the map.
unsafe impl Memory for Mapped {
    fn base(&self) -> *mut u8 {
        self.0.as_ptr() as *mut u8
    }
    fn len(&self) -> usize {
        self.0.len()
    }
}

#[cfg(windows)]
mod windows {
    use super::*;
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::Memory::{
        MapViewOfFile, UnmapViewOfFile, VirtualQuery, FILE_MAP_ALL_ACCESS,
        MEMORY_BASIC_INFORMATION, MEMORY_MAPPED_VIEW_ADDRESS,
    };

    struct View {
        base: *mut u8,
        len: usize,
    }

    // SAFETY: a shared view, valid until dropped, touched only through
    // atomics and raw copies.
    unsafe impl Send for View {}
    unsafe impl Sync for View {}
    unsafe impl Memory for View {
        fn base(&self) -> *mut u8 {
            self.base
        }
        fn len(&self) -> usize {
            self.len
        }
    }

    impl Drop for View {
        fn drop(&mut self) {
            // SAFETY: returned by MapViewOfFile and released once.
            unsafe {
                UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS {
                    Value: self.base.cast(),
                });
            }
        }
    }

    /// `value` is a section handle QEMU duplicated into this process; it is
    /// ours to close.
    pub fn map(value: u64) -> io::Result<Arc<dyn Memory>> {
        let handle = value as usize as HANDLE;
        // SAFETY: `handle` is a file mapping; 0 bytes maps all of it.
        let view = unsafe { MapViewOfFile(handle, FILE_MAP_ALL_ACCESS, 0, 0, 0) };
        let result = if view.Value.is_null() {
            Err(io::Error::last_os_error())
        } else {
            let mut info: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
            // SAFETY: `info` is the right size and `view` is mapped.
            let got = unsafe {
                VirtualQuery(
                    view.Value,
                    &mut info,
                    std::mem::size_of::<MEMORY_BASIC_INFORMATION>(),
                )
            };
            if got == 0 {
                let e = io::Error::last_os_error();
                // SAFETY: `view` is ours.
                unsafe { UnmapViewOfFile(view) };
                Err(e)
            } else {
                Ok(Arc::new(View {
                    base: view.Value.cast(),
                    len: info.RegionSize,
                }) as Arc<dyn Memory>)
            }
        };
        // SAFETY: the handle was passed to us to close; the view keeps the
        // memory alive.
        unsafe { CloseHandle(handle) };
        result
    }
}

/// Memory the tests stand in for a mapping with, and a writer that follows
/// the protocol the way QEMU does.
#[cfg(test)]
pub mod testing {
    use super::super::proto::*;
    use super::*;
    use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

    pub struct Heap {
        words: Vec<u64>,
    }

    impl Heap {
        pub fn new(bytes: usize) -> Arc<Heap> {
            Arc::new(Heap {
                words: vec![0; bytes.div_ceil(8)],
            })
        }

        pub fn u32_at(&self, off: usize) -> &AtomicU32 {
            assert!(off % 4 == 0 && off + 4 <= self.len());
            // SAFETY: in bounds and aligned; the Vec never moves.
            unsafe { &*(self.base().add(off) as *const AtomicU32) }
        }

        pub fn u64_at(&self, off: usize) -> &AtomicU64 {
            assert!(off % 8 == 0 && off + 8 <= self.len());
            // SAFETY: as above.
            unsafe { &*(self.base().add(off) as *const AtomicU64) }
        }

        pub fn put32(&self, off: usize, v: u32) {
            self.u32_at(off).store(v, Ordering::SeqCst);
        }

        pub fn get32(&self, off: usize) -> u32 {
            self.u32_at(off).load(Ordering::SeqCst)
        }

        /// Write pixel bytes.
        pub fn write(&self, off: usize, data: &[u8]) {
            assert!(off + data.len() <= self.len());
            // SAFETY: in bounds.
            unsafe {
                std::ptr::copy_nonoverlapping(data.as_ptr(), self.base().add(off), data.len())
            };
        }
    }

    // SAFETY: the Vec is never reallocated.
    unsafe impl Memory for Heap {
        fn base(&self) -> *mut u8 {
            self.words.as_ptr() as *mut u8
        }
        fn len(&self) -> usize {
            self.words.len() * 8
        }
    }

    /// A control block as QEMU keeps it.
    pub struct Block {
        pub mem: Arc<Heap>,
    }

    impl Block {
        pub fn new(nheads: u32) -> Block {
            let mem = Heap::new(CONTROL_BYTES);
            mem.put32(H_VERSION, VERSION);
            mem.put32(H_NHEADS, nheads);
            mem.put32(H_HEAD_BYTES, HEAD_BYTES as u32);
            mem.put32(H_HEAD_OFFSET, HEAD_OFFSET as u32);
            mem.put32(H_MAGIC, MAGIC); // last
            Block { mem }
        }

        pub fn head(&self, h: usize) -> usize {
            HEAD_OFFSET + h * HEAD_BYTES
        }

        pub fn get(&self, h: usize, field: usize) -> u32 {
            self.mem.get32(self.head(h) + field)
        }

        pub fn put(&self, h: usize, field: usize, v: u32) {
            self.mem.put32(self.head(h) + field, v);
        }

        /// Rewrite the layout of an xrgb head: the whole layout under an odd
        /// `layout_gen`, every line dirty, `content_gen` bumped.
        #[allow(clippy::too_many_arguments)]
        pub fn set_layout(
            &self,
            h: usize,
            w: u32,
            ht: u32,
            region: u32,
            offset: u64,
            stride: u32,
            flags: u32,
        ) {
            let g = self.get(h, R_LAYOUT_GEN);
            self.put(h, R_LAYOUT_GEN, g + 1);
            self.put(h, R_ACTIVE, 1);
            self.put(h, R_FAMILY, FAMILY_XRGB);
            self.put(h, R_FAMILY_VERSION, 1);
            self.put(h, R_WIDTH, w);
            self.put(h, R_HEIGHT, ht);
            self.put(h, R_FLAGS, flags);
            let p = R_PLANE;
            self.put(h, p + P_REGION, region);
            self.put(h, p + P_FORMAT, FMT_X8R8G8B8);
            self.mem
                .u64_at(self.head(h) + p + P_OFFSET)
                .store(offset, Ordering::SeqCst);
            self.put(h, p + P_WIDTH, w);
            self.put(h, p + P_HEIGHT, ht);
            self.put(h, p + P_STRIDE, stride);
            self.put(h, p + P_FLAGS, 0);
            self.put(h, R_LAYOUT_GEN, g + 2);
            self.mark(h, 0, ht);
            self.bump(h);
        }

        /// Set the dirty bits of lines `y0..y1`.
        pub fn mark(&self, h: usize, y0: u32, y1: u32) {
            for y in y0..y1.min(MAX_LINES) {
                let off = self.head(h) + R_DIRTY + (y as usize / 64) * 8;
                self.mem
                    .u64_at(off)
                    .fetch_or(1 << (y % 64), Ordering::SeqCst);
            }
        }

        pub fn bump(&self, h: usize) {
            self.mem
                .u64_at(self.head(h) + R_CONTENT_GEN)
                .fetch_add(1, Ordering::SeqCst);
        }

        pub fn dirty_word(&self, h: usize, w: usize) -> u64 {
            self.mem
                .u64_at(self.head(h) + R_DIRTY + w * 8)
                .load(Ordering::SeqCst)
        }

        pub fn deactivate(&self, h: usize) {
            let g = self.get(h, R_LAYOUT_GEN);
            self.put(h, R_LAYOUT_GEN, g + 1);
            self.put(h, R_ACTIVE, 0);
            self.put(h, R_LAYOUT_GEN, g + 2);
            self.bump(h);
        }
    }

    /// A plane's pixels: line `y` is filled with `f(y)` as a x8r8g8b8 word.
    pub fn fill_lines(
        region: &Heap,
        offset: usize,
        stride: usize,
        width: usize,
        lines: std::ops::Range<u32>,
        f: &dyn Fn(u32) -> u32,
    ) {
        for y in lines {
            let px = f(y).to_le_bytes();
            let row: Vec<u8> = (0..width).flat_map(|_| px).collect();
            region.write(offset + y as usize * stride, &row);
        }
    }
}
