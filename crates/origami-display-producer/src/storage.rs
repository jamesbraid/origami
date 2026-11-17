//! Shared memory a producer hands out: a region whose size a viewer cannot
//! change, read-only to the viewer unless it asked for write access.

use crate::plat::{self, Grant};
use origami_display::v4::regions::{Expect, Mapping};
use origami_display::v4::sys::Passed;
use std::io;
use std::sync::Arc;

/// One region and the producer's own writable mapping of it.
pub struct Shm {
    pub fd: Arc<Passed>,
    pub map: Mapping,
    pub size: u64,
    pub viewer_writable: bool,
}

impl Shm {
    /// What a viewer is sent for this region.
    pub fn grant(&self) -> Grant {
        Grant {
            handle: self.fd.clone(),
            writable: self.viewer_writable,
        }
    }
}

/// Create a zeroed region of `size` bytes. With `viewer_writable` false the
/// viewer's handle can never be mapped writable.
pub fn create(size: u64, viewer_writable: bool) -> io::Result<Shm> {
    assert!(size > 0 && size.is_multiple_of(65_536));
    let fd = plat::create_section(size)?;
    // Mapped before sealing, so the producer keeps its writable view.
    let map = Mapping::map(fd.try_clone()?, size, true, Expect::Nothing)?;
    plat::seal(&fd, viewer_writable)?;
    Ok(Shm {
        fd: Arc::new(fd),
        map,
        size,
        viewer_writable,
    })
}
