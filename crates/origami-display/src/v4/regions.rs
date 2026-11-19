//! Checked mappings of the regions a producer passes: the state region,
//! the viewer's feedback region and the slots.

use super::sys::Passed;
use std::io;
#[cfg(target_os = "linux")]
use std::os::fd::AsRawFd;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

pub const SEAL_SEAL: i32 = 1;
pub const SEAL_SHRINK: i32 = 2;
pub const SEAL_GROW: i32 = 4;
pub const SEAL_FUTURE_WRITE: i32 = 16;

/// The seals on a memfd, or an error if `fd` is not sealable. Linux only:
/// macOS regions are unsealed shared-memory objects.
#[cfg(target_os = "linux")]
pub fn seals(fd: &impl AsRawFd) -> io::Result<i32> {
    // SAFETY: F_GET_SEALS takes no argument and reads kernel state only.
    let r = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GET_SEALS) };
    if r < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(r)
    }
}

#[cfg(unix)]
type Raw = memmap2::MmapRaw;
#[cfg(windows)]
type Raw = win::View;

/// Shared memory another process may write (the viewer's feedback region,
/// or a producer's slot): reads and writes go through raw copies and atomics.
pub struct Mapping {
    raw: Raw,
    writable: bool,
}

// SAFETY: a shared mapping; every access is an atomic or a raw copy, and
// the protocol (not Rust's aliasing rules) orders accesses between processes.
unsafe impl Send for Mapping {}
unsafe impl Sync for Mapping {}

impl Mapping {
    /// Map all of `fd`, which must be exactly `size` bytes, and when a
    /// viewer is the caller (`expect`), carry the seals the contract requires.
    #[cfg(unix)]
    #[cfg_attr(not(target_os = "linux"), allow(unused_variables))]
    pub fn map(fd: Passed, size: u64, writable: bool, expect: Expect) -> io::Result<Mapping> {
        let file = std::fs::File::from(fd);
        let actual = file.metadata()?.len();
        if actual != size {
            return Err(io::Error::other(format!(
                "region is {actual} bytes, message says {size}"
            )));
        }
        #[cfg(target_os = "linux")]
        if expect != Expect::Nothing {
            let s =
                seals(&file).map_err(|e| io::Error::other(format!("not a sealed memfd: {e}")))?;
            let need = SEAL_SEAL | SEAL_SHRINK | SEAL_GROW;
            if s & need != need {
                return Err(io::Error::other("memfd lacks the size seals"));
            }
            if expect == Expect::ReadOnly && s & SEAL_FUTURE_WRITE == 0 {
                return Err(io::Error::other("read-only memfd can still be written"));
            }
        }
        let opts = memmap2::MmapOptions::new();
        let raw = if writable {
            opts.map_raw(&file)?
        } else {
            opts.map_raw_read_only(&file)?
        };
        Ok(Mapping { raw, writable })
    }

    /// Map the section `h`, which must be `size` bytes; a read-only one must refuse a writable view.
    #[cfg(windows)]
    pub fn map(h: Passed, size: u64, writable: bool, expect: Expect) -> io::Result<Mapping> {
        let m = Mapping::open(h, writable)?;
        let raw = m.raw;
        if raw.len() as u64 != size {
            return Err(io::Error::other(format!(
                "region is {} bytes, message says {size}",
                raw.len()
            )));
        }
        if expect == Expect::ReadOnly && raw.section_allows_write() {
            return Err(io::Error::other("read-only section can still be written"));
        }
        Ok(Mapping { raw, writable })
    }

    /// Map all of the section `h`, whatever its size.
    #[cfg(windows)]
    pub fn open(h: Passed, writable: bool) -> io::Result<Mapping> {
        Ok(Mapping {
            raw: win::View::new(h, writable)?,
            writable,
        })
    }

    /// The section this mapping came from, for a GPU API that opens it itself.
    #[cfg(windows)]
    pub fn section(&self) -> &std::os::windows::io::OwnedHandle {
        self.raw.section()
    }

    pub fn len(&self) -> usize {
        self.raw.len()
    }
    pub fn is_empty(&self) -> bool {
        self.raw.len() == 0
    }
    pub fn writable(&self) -> bool {
        self.writable
    }
    pub fn as_ptr(&self) -> *mut u8 {
        self.raw.as_mut_ptr()
    }

    fn at<T>(&self, off: usize) -> *mut T {
        assert!(
            off.is_multiple_of(std::mem::align_of::<T>())
                && off + std::mem::size_of::<T>() <= self.len()
        );
        // SAFETY: bounds and alignment asserted above.
        unsafe { self.as_ptr().add(off).cast() }
    }
    fn a32(&self, off: usize) -> &AtomicU32 {
        // SAFETY: in bounds, aligned, and only ever accessed atomically.
        unsafe { &*self.at::<AtomicU32>(off) }
    }
    fn a64(&self, off: usize) -> &AtomicU64 {
        // SAFETY: as above.
        unsafe { &*self.at::<AtomicU64>(off) }
    }
    pub fn load32(&self, off: usize, o: Ordering) -> u32 {
        self.a32(off).load(o)
    }
    pub fn load64(&self, off: usize, o: Ordering) -> u64 {
        self.a64(off).load(o)
    }
    pub fn store32(&self, off: usize, v: u32, o: Ordering) {
        self.a32(off).store(v, o)
    }
    pub fn store64(&self, off: usize, v: u64, o: Ordering) {
        self.a64(off).store(v, o)
    }

    /// Copy `dst.len()` bytes from `off`.
    pub fn copy_out(&self, off: usize, dst: &mut [u8]) {
        assert!(off + dst.len() <= self.len());
        // SAFETY: bounds asserted; raw copy, no reference to the shared bytes.
        unsafe {
            std::ptr::copy_nonoverlapping(self.as_ptr().add(off), dst.as_mut_ptr(), dst.len())
        }
    }
    /// Copy `src` to `off`. The mapping must be writable.
    pub fn copy_in(&self, off: usize, src: &[u8]) {
        assert!(self.writable && off + src.len() <= self.len());
        // SAFETY: bounds asserted; raw copy into a mapping nobody else may read yet,
        // or may read only under the protocol's ownership rules.
        unsafe { std::ptr::copy_nonoverlapping(src.as_ptr(), self.as_ptr().add(off), src.len()) }
    }

    /// A view of `len` bytes at `off`.
    ///
    /// # Safety
    /// Nothing may write these bytes while the slice lives: the caller holds a
    /// lease on the slot (the producer never writes a lent slot).
    pub unsafe fn bytes(&self, off: usize, len: usize) -> &[u8] {
        assert!(off + len <= self.len());
        std::slice::from_raw_parts(self.as_ptr().add(off), len)
    }
}

#[cfg(windows)]
mod win {
    use super::*;
    use std::os::windows::io::{AsRawHandle, OwnedHandle};
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::System::Memory::{
        MapViewOfFile, UnmapViewOfFile, VirtualQuery, FILE_MAP_READ, FILE_MAP_WRITE,
        MEMORY_BASIC_INFORMATION, MEMORY_MAPPED_VIEW_ADDRESS,
    };

    /// A view of a whole section, which it keeps open.
    pub struct View {
        base: *mut u8,
        len: usize,
        section: OwnedHandle,
    }

    fn map_view(section: &OwnedHandle, access: u32) -> io::Result<MEMORY_MAPPED_VIEW_ADDRESS> {
        // SAFETY: a file mapping handle; 0 bytes maps all of it.
        let v = unsafe { MapViewOfFile(section.as_raw_handle() as HANDLE, access, 0, 0, 0) };
        if v.Value.is_null() {
            Err(io::Error::last_os_error())
        } else {
            Ok(v)
        }
    }

    impl View {
        pub fn new(section: OwnedHandle, writable: bool) -> io::Result<View> {
            let access = if writable {
                FILE_MAP_READ | FILE_MAP_WRITE
            } else {
                FILE_MAP_READ
            };
            let v = map_view(&section, access)?;
            let mut info: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
            // SAFETY: `info` is the right size and `v` is mapped.
            let got = unsafe {
                VirtualQuery(
                    v.Value,
                    &mut info,
                    std::mem::size_of::<MEMORY_BASIC_INFORMATION>(),
                )
            };
            if got == 0 {
                let e = io::Error::last_os_error();
                // SAFETY: `v` is ours.
                unsafe { UnmapViewOfFile(v) };
                return Err(e);
            }
            Ok(View {
                base: v.Value.cast(),
                len: info.RegionSize,
                section,
            })
        }

        pub fn len(&self) -> usize {
            self.len
        }

        pub fn as_mut_ptr(&self) -> *mut u8 {
            self.base
        }

        pub fn section(&self) -> &OwnedHandle {
            &self.section
        }

        /// Whether this handle could map the section writable.
        pub fn section_allows_write(&self) -> bool {
            match map_view(&self.section, FILE_MAP_WRITE) {
                Ok(v) => {
                    // SAFETY: the probe view just made.
                    unsafe { UnmapViewOfFile(v) };
                    true
                }
                Err(_) => false,
            }
        }
    }

    impl Drop for View {
        fn drop(&mut self) {
            // SAFETY: returned by MapViewOfFile and released once.
            unsafe {
                UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS {
                    Value: self.base.cast(),
                })
            };
        }
    }
}

/// What a viewer requires of a region it receives: seals on a Linux memfd,
/// no write access on a Windows section.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Expect {
    /// Size seals, plus `FUTURE_WRITE` (the descriptor cannot be mapped writable).
    ReadOnly,
    /// Size seals only.
    Writable,
    /// No check: not a memfd (a test double).
    Nothing,
}

/// One head's cursor record in the state region.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CursorRecord {
    pub seq: u32,
    pub flags: u32,
    pub x: i32,
    pub y: i32,
    pub image_serial: u32,
}

/// One head's frame record in the state region.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FrameRecord {
    pub serial: u64,
    pub sample_ns: u64,
    pub publish_ns: u64,
    pub guest_ns: u64,
    pub skipped: u32,
}

/// The read-only state region: seqlock readers.
pub struct StateView {
    map: Mapping,
    head_offset: usize,
    head_stride: usize,
    capacity: usize,
    closed: std::sync::Arc<std::sync::atomic::AtomicBool>,
    last: std::sync::Mutex<Vec<(CursorRecord, FrameRecord)>>,
}

impl StateView {
    pub fn new(
        map: Mapping,
        closed: std::sync::Arc<std::sync::atomic::AtomicBool>,
        session_id: u64,
    ) -> Result<StateView, &'static str> {
        use super::wire::{state, MAJOR, STATE_MAGIC};
        if map.len() < state::HEADER {
            return Err("state region too small");
        }
        let (mut hdr, mut sid) = ([0u8; 8], [0u8; 8]);
        map.copy_out(0, &mut hdr);
        map.copy_out(8, &mut sid);
        if u32::from_le_bytes(hdr[..4].try_into().unwrap()) != STATE_MAGIC {
            return Err("state region magic");
        }
        if u16::from_le_bytes(hdr[4..6].try_into().unwrap()) != MAJOR {
            return Err("state region major");
        }
        if u64::from_le_bytes(sid) != session_id {
            return Err("state region session");
        }
        let head_offset = map.load32(16, Ordering::Relaxed) as usize;
        let head_stride = map.load32(20, Ordering::Relaxed) as usize;
        let capacity = map.load32(24, Ordering::Relaxed) as usize;
        if head_stride < state::HEAD_STRIDE
            || head_offset < state::HEADER
            || !head_offset.is_multiple_of(8)
            || !head_stride.is_multiple_of(8)
            || capacity > super::wire::FEEDBACK_HEADS
            || head_offset.saturating_add(head_stride.saturating_mul(capacity)) > map.len()
        {
            return Err("state region head table");
        }
        Ok(StateView {
            map,
            head_offset,
            head_stride,
            capacity,
            closed,
            last: std::sync::Mutex::new(vec![Default::default(); capacity]),
        })
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    fn base(&self, head: u32) -> Option<usize> {
        ((head as usize) < self.capacity)
            .then(|| self.head_offset + head as usize * self.head_stride)
    }

    /// Spin until `seq` is even and unchanged around `read`, or the
    /// connection closes, when the last good value stands.
    fn seqlock<T>(&self, seq_off: usize, read: impl Fn() -> T) -> Option<T> {
        let mut tries = 0u32;
        loop {
            if self.closed.load(Ordering::Acquire) {
                return None;
            }
            let s1 = self.map.load32(seq_off, Ordering::Acquire);
            if s1 & 1 != 0 {
                tries += 1;
                if tries > 1000 {
                    std::thread::yield_now();
                } else {
                    std::hint::spin_loop();
                }
                continue;
            }
            let v = read();
            std::sync::atomic::fence(Ordering::Acquire);
            if self.map.load32(seq_off, Ordering::Relaxed) == s1 {
                return Some(v);
            }
        }
    }

    pub fn cursor(&self, head: u32) -> Option<CursorRecord> {
        use super::wire::state as s;
        let b = self.base(head)?;
        let m = &self.map;
        let r = self.seqlock(b + s::CURSOR_SEQ, || CursorRecord {
            seq: m.load32(b + s::CURSOR_SEQ, Ordering::Relaxed),
            flags: m.load32(b + s::CURSOR_FLAGS, Ordering::Relaxed),
            x: m.load32(b + s::CURSOR_X, Ordering::Relaxed) as i32,
            y: m.load32(b + s::CURSOR_Y, Ordering::Relaxed) as i32,
            image_serial: m.load32(b + s::CURSOR_IMAGE_SERIAL, Ordering::Relaxed),
        });
        let mut last = self.last.lock().unwrap();
        match r {
            Some(v) => {
                last[head as usize].0 = v;
                Some(v)
            }
            None => Some(last[head as usize].0),
        }
    }

    /// The `qemu.pointer-steering` block at `offset`: where the guest drew
    /// its cursor, as (seq, kind, x, y). Kind 0 means it has drawn none.
    pub fn steer(&self, offset: usize) -> Option<(u32, u32, i32, i32)> {
        if !offset.is_multiple_of(4) || offset.checked_add(16)? > self.map.len() {
            return None;
        }
        let m = &self.map;
        self.seqlock(offset, || {
            (
                m.load32(offset, Ordering::Relaxed),
                m.load32(offset + 4, Ordering::Relaxed),
                m.load32(offset + 8, Ordering::Relaxed) as i32,
                m.load32(offset + 12, Ordering::Relaxed) as i32,
            )
        })
    }

    pub fn frame(&self, head: u32) -> Option<FrameRecord> {
        use super::wire::state as s;
        let b = self.base(head)?;
        let m = &self.map;
        let r = self.seqlock(b + s::FRAME_SEQ, || FrameRecord {
            serial: m.load64(b + s::FRAME_SERIAL, Ordering::Relaxed),
            sample_ns: m.load64(b + s::SAMPLE_NS, Ordering::Relaxed),
            publish_ns: m.load64(b + s::PUBLISH_NS, Ordering::Relaxed),
            guest_ns: m.load64(b + s::GUEST_NS, Ordering::Relaxed),
            skipped: m.load32(b + s::SKIPPED, Ordering::Relaxed),
        });
        let mut last = self.last.lock().unwrap();
        match r {
            Some(v) => {
                last[head as usize].1 = v;
                Some(v)
            }
            None => Some(last[head as usize].1),
        }
    }
}
