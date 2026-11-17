//! The operating-system edge of the session. POSIX passes a region's descriptor
//! with `SCM_RIGHTS`; Windows puts a handle value in the message's last 8 bytes.

use std::collections::VecDeque;
use std::io;
use std::time::Duration;

#[cfg(unix)]
pub use std::os::fd::OwnedFd as Passed;
#[cfg(unix)]
pub use std::os::unix::net::{UnixListener, UnixStream};
#[cfg(windows)]
pub use std::os::windows::io::OwnedHandle as Passed;
#[cfg(windows)]
pub use uds_windows::{UnixListener, UnixStream};

#[cfg(unix)]
pub type RawSock = std::os::fd::RawFd;
#[cfg(windows)]
pub type RawSock = std::os::windows::io::RawSocket;

/// What rides with a send on POSIX: the descriptor. Windows writes the value
/// into the message, so it is ignored there.
#[cfg(unix)]
pub type RawPassed = std::os::fd::RawFd;
#[cfg(windows)]
pub type RawPassed = usize;

/// The OS-level socket of a stream or listener.
pub trait AsSock {
    fn sock(&self) -> RawSock;
}

#[cfg(unix)]
impl<T: std::os::fd::AsRawFd> AsSock for T {
    fn sock(&self) -> RawSock {
        self.as_raw_fd()
    }
}
#[cfg(windows)]
impl<T: std::os::windows::io::AsRawSocket> AsSock for T {
    fn sock(&self) -> RawSock {
        self.as_raw_socket()
    }
}

/// More descriptors than this with no message to match is a protocol error.
pub const MAX_UNMATCHED: usize = 64;

pub fn ms(t: Option<Duration>) -> i32 {
    t.map_or(-1, |d| d.as_millis().min(i32::MAX as u128) as i32)
}

/// Monotonic nanoseconds, the clock of the state region's timestamps.
pub fn now_ns() -> u64 {
    #[cfg(unix)]
    {
        let mut ts = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: valid timespec out-pointer.
        unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
        ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Performance::{
            QueryPerformanceCounter, QueryPerformanceFrequency,
        };
        let (mut c, mut f) = (0i64, 0i64);
        // SAFETY: valid out-pointers; these calls cannot fail on Windows XP and later.
        unsafe {
            QueryPerformanceCounter(&mut c);
            QueryPerformanceFrequency(&mut f);
        }
        (c as u128 * 1_000_000_000 / f.max(1) as u128) as u64
    }
}

#[cfg(unix)]
mod imp {
    use super::*;
    use std::os::fd::{FromRawFd, RawFd};

    /// Linux marks received descriptors close-on-exec as they arrive; macOS has
    /// no such flag, so they are marked as soon as they are taken.
    #[cfg(target_os = "linux")]
    const RECV_FLAGS: libc::c_int = libc::MSG_CMSG_CLOEXEC;
    #[cfg(not(target_os = "linux"))]
    const RECV_FLAGS: libc::c_int = 0;

    /// macOS has no `MSG_NOSIGNAL`; `SO_NOSIGPIPE` on the socket does the work.
    #[cfg(target_os = "linux")]
    const SEND_FLAGS: libc::c_int = libc::MSG_NOSIGNAL;
    #[cfg(not(target_os = "linux"))]
    const SEND_FLAGS: libc::c_int = 0;

    /// Keep a write to a closed socket from raising `SIGPIPE`, where the send
    /// flag cannot.
    #[cfg(not(target_os = "linux"))]
    pub fn no_sigpipe(sock: &impl AsSock) -> io::Result<()> {
        let on: libc::c_int = 1;
        // SAFETY: setsockopt with an int the size it says.
        let r = unsafe {
            libc::setsockopt(
                sock.sock(),
                libc::SOL_SOCKET,
                libc::SO_NOSIGPIPE,
                (&on as *const libc::c_int).cast(),
                std::mem::size_of::<libc::c_int>() as libc::socklen_t,
            )
        };
        if r < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
    #[cfg(target_os = "linux")]
    pub fn no_sigpipe(_: &impl AsSock) -> io::Result<()> {
        Ok(())
    }

    /// One `recvmsg` into `buf`, queueing received descriptors. Returns the
    /// byte count (0 is end of stream) and whether ancillary data was cut off.
    pub fn recv(
        sock: RawSock,
        buf: &mut [u8],
        passed: &mut VecDeque<Passed>,
    ) -> io::Result<(usize, bool)> {
        let mut cmsg = [0u64; 64]; // room for 64 descriptors
        let mut iov = libc::iovec {
            iov_base: buf.as_mut_ptr().cast(),
            iov_len: buf.len(),
        };
        // SAFETY: zeroed msghdr is valid; the pointers set below outlive the call.
        let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;
        msg.msg_control = cmsg.as_mut_ptr().cast();
        msg.msg_controllen = std::mem::size_of_val(&cmsg) as _;
        // SAFETY: `msg` points at live buffers of the stated sizes.
        let n = unsafe { libc::recvmsg(sock, &mut msg, RECV_FLAGS) };
        if n < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: walking the control buffer the kernel just filled.
        unsafe {
            let mut c = libc::CMSG_FIRSTHDR(&msg);
            while !c.is_null() {
                if (*c).cmsg_level == libc::SOL_SOCKET && (*c).cmsg_type == libc::SCM_RIGHTS {
                    let data = libc::CMSG_DATA(c) as *const RawFd;
                    let count = ((*c).cmsg_len as usize - libc::CMSG_LEN(0) as usize)
                        / std::mem::size_of::<RawFd>();
                    for i in 0..count {
                        let fd = data.add(i).read_unaligned();
                        #[cfg(not(target_os = "linux"))]
                        libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
                        passed.push_back(Passed::from_raw_fd(fd));
                    }
                }
                c = libc::CMSG_NXTHDR(&msg, c);
            }
        }
        Ok((n as usize, msg.msg_flags & libc::MSG_CTRUNC != 0))
    }

    /// The handle of the `HANDLE` message `_msg`: the oldest queued descriptor.
    pub fn take_passed(passed: &mut VecDeque<Passed>, _msg: &[u8]) -> Result<Passed, &'static str> {
        passed
            .pop_front()
            .ok_or("HANDLE message without a descriptor")
    }

    /// Write as much of `data` as the socket takes. The descriptor, if any,
    /// rides the first byte, so pass it only with a message's first write.
    pub fn send(sock: RawSock, data: &[u8], fd: Option<RawPassed>) -> io::Result<usize> {
        let mut iov = libc::iovec {
            iov_base: data.as_ptr() as *mut _,
            iov_len: data.len(),
        };
        // SAFETY: zeroed msghdr is valid; pointers below outlive the call.
        let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;
        let mut cmsg = [0u64; 4];
        if let Some(fd) = fd {
            // SAFETY: `cmsg` holds CMSG_SPACE(4) bytes (32); one header is written.
            unsafe {
                msg.msg_control = cmsg.as_mut_ptr().cast();
                msg.msg_controllen = libc::CMSG_SPACE(4) as _;
                let c = libc::CMSG_FIRSTHDR(&msg);
                (*c).cmsg_level = libc::SOL_SOCKET;
                (*c).cmsg_type = libc::SCM_RIGHTS;
                (*c).cmsg_len = libc::CMSG_LEN(4) as _;
                (libc::CMSG_DATA(c) as *mut RawFd).write_unaligned(fd);
            }
        }
        // SAFETY: valid msghdr over live buffers.
        let n = unsafe { libc::sendmsg(sock, &msg, SEND_FLAGS) };
        if n < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(n as usize)
        }
    }

    /// Wait up to `t` for `sock` to be readable. True when it is.
    pub fn wait_readable(sock: RawSock, t: Option<Duration>) -> io::Result<bool> {
        let mut p = libc::pollfd {
            fd: sock,
            events: libc::POLLIN,
            revents: 0,
        };
        loop {
            // SAFETY: one valid pollfd.
            let r = unsafe { libc::poll(&mut p, 1, ms(t)) };
            if r >= 0 {
                return Ok(r > 0);
            }
            let e = io::Error::last_os_error();
            if e.kind() != io::ErrorKind::Interrupted {
                return Err(e);
            }
        }
    }

    /// Nothing to close: a POSIX viewer is never sent orphans.
    pub fn close_orphan(_value: u64) {}
}

#[cfg(windows)]
mod imp {
    use super::*;
    use std::os::windows::io::{FromRawHandle, RawHandle};
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::Networking::WinSock as ws;

    pub fn no_sigpipe(_: &impl AsSock) -> io::Result<()> {
        Ok(())
    }

    pub fn recv(
        sock: RawSock,
        buf: &mut [u8],
        _passed: &mut VecDeque<Passed>,
    ) -> io::Result<(usize, bool)> {
        // SAFETY: `buf` is live for the stated length.
        let n = unsafe { ws::recv(sock as ws::SOCKET, buf.as_mut_ptr(), buf.len() as i32, 0) };
        if n < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok((n as usize, false))
        }
    }

    /// The handle value in the last 8 bytes of `msg`; the message owns it
    /// from the moment it is read.
    pub fn take_passed(_passed: &mut VecDeque<Passed>, msg: &[u8]) -> Result<Passed, &'static str> {
        let tail: [u8; 8] = msg[msg.len() - 8..].try_into().unwrap();
        match u64::from_le_bytes(tail) {
            0 => Err("HANDLE message with no handle value"),
            // SAFETY: the producer duplicated this handle into our process for us to own.
            v => Ok(unsafe { Passed::from_raw_handle(v as usize as RawHandle) }),
        }
    }

    pub fn send(sock: RawSock, data: &[u8], _passed: Option<RawPassed>) -> io::Result<usize> {
        // SAFETY: `data` is live for the stated length.
        let n = unsafe { ws::send(sock as ws::SOCKET, data.as_ptr(), data.len() as i32, 0) };
        if n < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(n as usize)
        }
    }

    pub fn wait_readable(sock: RawSock, t: Option<Duration>) -> io::Result<bool> {
        let mut p = ws::WSAPOLLFD {
            fd: sock as ws::SOCKET,
            events: ws::POLLRDNORM,
            revents: 0,
        };
        // SAFETY: one valid pollfd.
        let r = unsafe { ws::WSAPoll(&mut p, 1, ms(t)) };
        if r < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(r > 0)
        }
    }

    /// Close a handle the producer duplicated into this process and the
    /// connection broke before it was taken.
    pub fn close_orphan(value: u64) {
        // SAFETY: the producer names only handles it created for us and we never took.
        unsafe { CloseHandle(value as usize as _) };
    }
}

pub use imp::*;
