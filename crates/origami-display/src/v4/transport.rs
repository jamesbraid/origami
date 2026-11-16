//! Whole messages out of arbitrary read boundaries, with `SCM_RIGHTS`
//! descriptors matched to `HANDLE` messages. POSIX only.

use super::wire::{Header, WireError};
use std::collections::VecDeque;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};

/// More descriptors than this with no message to match is a protocol error.
const MAX_UNMATCHED_FDS: usize = 64;
const READ_CHUNK: usize = 64 * 1024;

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
pub fn no_sigpipe(sock: &impl AsRawFd) -> io::Result<()> {
    let on: libc::c_int = 1;
    // SAFETY: setsockopt with an int the size it says.
    let r = unsafe {
        libc::setsockopt(
            sock.as_raw_fd(),
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

#[derive(Debug)]
pub enum Fault {
    /// The peer broke the framing or handle rules: answer `ERROR PROTOCOL`.
    Protocol(&'static str),
    Io(io::Error),
}

impl From<WireError> for Fault {
    fn from(e: WireError) -> Fault {
        Fault::Protocol(e.0)
    }
}

impl From<io::Error> for Fault {
    fn from(e: io::Error) -> Fault {
        Fault::Io(e)
    }
}

/// Bytes and descriptors received so far, not yet cut into messages.
#[derive(Default)]
pub struct Reader {
    buf: Vec<u8>,
    start: usize,
    fds: VecDeque<OwnedFd>,
    /// `HANDLE` messages cut so far.
    pub handles_parsed: u64,
    eof: bool,
}

impl Reader {
    pub fn new() -> Reader {
        Reader::default()
    }

    pub fn at_eof(&self) -> bool {
        self.eof
    }

    /// True when a whole message may be waiting.
    pub fn has_buffered(&self) -> bool {
        self.buf.len() > self.start
    }

    /// One `recvmsg`. Returns the byte count; 0 is end of stream. A
    /// non-blocking socket with nothing to read gives `WouldBlock`.
    pub fn fill(&mut self, sock: RawFd) -> Result<usize, Fault> {
        if self.start > 0 && self.start >= self.buf.len() {
            self.buf.clear();
            self.start = 0;
        } else if self.start > READ_CHUNK {
            self.buf.drain(..self.start);
            self.start = 0;
        }
        let old = self.buf.len();
        self.buf.resize(old + READ_CHUNK, 0);
        let mut cmsg = [0u64; 64]; // room for 64 descriptors
        let mut iov = libc::iovec {
            iov_base: self.buf[old..].as_mut_ptr().cast(),
            iov_len: READ_CHUNK,
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
            self.buf.truncate(old);
            return Err(Fault::Io(io::Error::last_os_error()));
        }
        self.buf.truncate(old + n as usize);
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
                        self.fds.push_back(OwnedFd::from_raw_fd(fd));
                    }
                }
                c = libc::CMSG_NXTHDR(&msg, c);
            }
        }
        if msg.msg_flags & libc::MSG_CTRUNC != 0 {
            return Err(Fault::Protocol("ancillary data truncated"));
        }
        if self.fds.len() > MAX_UNMATCHED_FDS {
            return Err(Fault::Protocol("too many unmatched descriptors"));
        }
        if n == 0 {
            self.eof = true;
        }
        Ok(n as usize)
    }

    /// Hand the next whole message, if one is buffered, to `f` with its
    /// descriptor. The slice is the message including its header.
    pub fn next_with<T>(
        &mut self,
        f: impl FnOnce(Header, &[u8], Option<OwnedFd>) -> T,
    ) -> Result<Option<T>, Fault> {
        let avail = &self.buf[self.start..];
        if avail.len() < 8 {
            return Ok(None);
        }
        let h = Header::parse(avail)?;
        let len = h.len as usize;
        if avail.len() < len {
            return Ok(None);
        }
        let fd = if h.handle() {
            if len < 16 {
                return Err(Fault::Protocol("HANDLE message under 16 bytes"));
            }
            match self.fds.pop_front() {
                Some(fd) => Some(fd),
                None => return Err(Fault::Protocol("HANDLE message without a descriptor")),
            }
        } else {
            None
        };
        if h.handle() {
            self.handles_parsed += 1;
        }
        let out = f(h, &self.buf[self.start..self.start + len], fd);
        self.start += len;
        Ok(Some(out))
    }
}

/// Write as much of `data` as the socket takes. The descriptor, if any,
/// rides the first byte, so pass it only with a message's first write.
pub fn send(sock: RawFd, data: &[u8], fd: Option<RawFd>) -> io::Result<usize> {
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

/// Write all of `data` on a blocking socket.
pub fn send_all(sock: &impl AsRawFd, mut data: &[u8], mut fd: Option<RawFd>) -> io::Result<()> {
    while !data.is_empty() {
        match send(sock.as_raw_fd(), data, fd) {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(n) => {
                data = &data[n..];
                fd = None;
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixStream;

    #[test]
    fn messages_survive_any_read_boundary() {
        let (a, b) = UnixStream::pair().unwrap();
        let m1 = super::super::wire::handles_seen(7);
        let m2 = super::super::wire::input_reset();
        let mut all = m1.clone();
        all.extend_from_slice(&m2);
        let mut r = Reader::new();
        let mut got = Vec::new();
        for byte in all {
            send_all(&a, &[byte], None).unwrap();
            r.fill(b.as_raw_fd()).unwrap();
            while let Some(h) = r.next_with(|h, _, _| h.ty).unwrap() {
                got.push(h);
            }
        }
        assert_eq!(got, vec![0x0004, 0x0016]);
    }

    #[test]
    fn a_handle_message_takes_one_descriptor_in_order() {
        let (a, b) = UnixStream::pair().unwrap();
        let f1 = std::fs::File::open("/dev/null").unwrap();
        let f2 = std::fs::File::open("/dev/zero").unwrap();
        let region = |id| {
            super::super::wire::Region {
                id,
                kind: 3,
                storage: 1,
                access: 1,
                size: 65536,
                payload: vec![],
            }
            .encode()
        };
        send_all(&a, &region(1), Some(f1.as_raw_fd())).unwrap();
        send_all(&a, &region(2), Some(f2.as_raw_fd())).unwrap();
        let mut r = Reader::new();
        let mut n = 0;
        while n < 2 {
            r.fill(b.as_raw_fd()).unwrap();
            while r
                .next_with(|_, _, fd| assert!(fd.is_some()))
                .unwrap()
                .is_some()
            {
                n += 1;
            }
        }
        assert_eq!((n, r.handles_parsed), (2, 2));
    }

    #[test]
    fn a_handle_message_without_a_descriptor_is_a_fault() {
        let (a, b) = UnixStream::pair().unwrap();
        let m = super::super::wire::Region {
            id: 1,
            kind: 3,
            storage: 1,
            access: 1,
            size: 65536,
            payload: vec![],
        }
        .encode();
        send_all(&a, &m, None).unwrap();
        let mut r = Reader::new();
        r.fill(b.as_raw_fd()).unwrap();
        assert!(matches!(r.next_with(|_, _, _| ()), Err(Fault::Protocol(_))));
    }
}
