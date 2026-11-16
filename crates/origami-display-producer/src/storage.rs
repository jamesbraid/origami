//! Shared memory a producer hands out: sealed memfds whose size a viewer
//! cannot change, read-only to the viewer unless it asked for write access.

use origami_display::v4::regions::{
    seals, Expect, Mapping, SEAL_FUTURE_WRITE, SEAL_GROW, SEAL_SEAL, SEAL_SHRINK,
};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::Arc;

/// One memfd and the producer's own writable mapping of it.
pub struct Shm {
    pub fd: Arc<OwnedFd>,
    pub map: Mapping,
    pub size: u64,
}

/// Create a zeroed, sealed region of `size` bytes. With `viewer_writable`
/// false the viewer's descriptor can never be mapped writable.
pub fn create(size: u64, viewer_writable: bool) -> io::Result<Shm> {
    assert!(size > 0 && size.is_multiple_of(65_536));
    // SAFETY: memfd_create with a NUL-terminated name.
    let raw = unsafe {
        libc::memfd_create(
            c"origami-display".as_ptr(),
            libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
        )
    };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: raw is a fresh descriptor.
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    // SAFETY: ftruncate on our own memfd.
    if unsafe { libc::ftruncate(fd.as_raw_fd(), size as libc::off_t) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // Mapped before FUTURE_WRITE, so the producer keeps its writable view.
    let map = Mapping::map(fd.try_clone()?, size, true, Expect::Nothing)?;
    let mut want = SEAL_SHRINK | SEAL_GROW;
    if !viewer_writable {
        want |= SEAL_FUTURE_WRITE;
    }
    for s in [want, SEAL_SEAL] {
        // SAFETY: F_ADD_SEALS on our memfd.
        if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_ADD_SEALS, s) } != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    let got = seals(&fd)?;
    if got & (want | SEAL_SEAL) != (want | SEAL_SEAL) {
        return Err(io::Error::other("memfd refused a seal"));
    }
    Ok(Shm {
        fd: Arc::new(fd),
        map,
        size,
    })
}
