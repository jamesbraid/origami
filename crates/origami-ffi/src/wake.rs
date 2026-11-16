//! A wake-up object that can be waited on without polling: an eventfd (a
//! pipe where there is none) read end on Unix, an auto-reset event on
//! Windows. `signal` allocates nothing, so engine threads call it per frame.

#[cfg(unix)]
mod imp {
    pub struct WakeHandle {
        read: libc::c_int,
        write: libc::c_int,
    }

    impl WakeHandle {
        pub fn new() -> Result<Self, String> {
            #[cfg(target_os = "linux")]
            {
                // SAFETY: plain syscall.
                let fd = unsafe { libc::eventfd(0, libc::EFD_NONBLOCK | libc::EFD_CLOEXEC) };
                if fd < 0 {
                    return Err(std::io::Error::last_os_error().to_string());
                }
                Ok(Self {
                    read: fd,
                    write: fd,
                })
            }
            #[cfg(not(target_os = "linux"))]
            {
                let mut fds = [0 as libc::c_int; 2];
                // SAFETY: `fds` has room for the two descriptors.
                if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
                    return Err(std::io::Error::last_os_error().to_string());
                }
                for fd in fds {
                    // SAFETY: both descriptors were just opened.
                    unsafe {
                        libc::fcntl(fd, libc::F_SETFL, libc::O_NONBLOCK);
                        libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
                    }
                }
                Ok(Self {
                    read: fds[0],
                    write: fds[1],
                })
            }
        }

        /// Make the handle readable.
        pub fn signal(&self) {
            let one: u64 = 1;
            let (ptr, len) = if cfg!(target_os = "linux") {
                (&one as *const u64 as *const libc::c_void, 8)
            } else {
                (&one as *const u64 as *const libc::c_void, 1)
            };
            // SAFETY: `ptr` points at `len` readable bytes. A full pipe or
            // counter fails with EAGAIN, which already means "readable".
            unsafe { libc::write(self.write, ptr, len) };
        }

        /// Make it unreadable again.
        pub fn clear(&self) {
            let mut sink = [0u8; 64];
            loop {
                // SAFETY: `sink` is writable for its length.
                let n = unsafe { libc::read(self.read, sink.as_mut_ptr().cast(), sink.len()) };
                // An eventfd resets on one read; a pipe is read until empty.
                if n <= 0 || cfg!(target_os = "linux") {
                    break;
                }
            }
        }

        /// The descriptor to poll for readability.
        pub fn raw(&self) -> isize {
            self.read as isize
        }
    }

    impl Drop for WakeHandle {
        fn drop(&mut self) {
            // SAFETY: the descriptors belong to this value.
            unsafe {
                libc::close(self.read);
                if self.write != self.read {
                    libc::close(self.write);
                }
            }
        }
    }
}

#[cfg(windows)]
mod imp {
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::Threading::{CreateEventW, ResetEvent, SetEvent};

    pub struct WakeHandle(HANDLE);

    // SAFETY: an event HANDLE may be used from any thread.
    unsafe impl Send for WakeHandle {}
    // SAFETY: as above.
    unsafe impl Sync for WakeHandle {}

    impl WakeHandle {
        pub fn new() -> Result<Self, String> {
            // SAFETY: no security attributes and no name; auto-reset, unsignalled.
            let handle = unsafe { CreateEventW(std::ptr::null(), 0, 0, std::ptr::null()) };
            if handle.is_null() {
                return Err(std::io::Error::last_os_error().to_string());
            }
            Ok(Self(handle))
        }

        pub fn signal(&self) {
            // SAFETY: the handle is valid until drop.
            unsafe { SetEvent(self.0) };
        }

        pub fn clear(&self) {
            // SAFETY: the handle is valid until drop.
            unsafe { ResetEvent(self.0) };
        }

        pub fn raw(&self) -> isize {
            self.0 as isize
        }
    }

    impl Drop for WakeHandle {
        fn drop(&mut self) {
            // SAFETY: the handle belongs to this value.
            unsafe { CloseHandle(self.0) };
        }
    }
}

pub use imp::WakeHandle;
