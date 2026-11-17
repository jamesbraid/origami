//! Whole messages out of arbitrary read boundaries, with the handle of each
//! `HANDLE` message matched to it ([`super::sys`] says how it travels).

use super::sys::{self, AsSock, Passed, RawPassed, RawSock};
use super::wire::{Header, WireError};
use std::collections::VecDeque;
use std::io;

const READ_CHUNK: usize = 64 * 1024;

pub use super::sys::no_sigpipe;

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

/// Bytes and handles received so far, not yet cut into messages.
#[derive(Default)]
pub struct Reader {
    buf: Vec<u8>,
    start: usize,
    fds: VecDeque<Passed>,
    /// `HANDLE` messages cut so far. On Windows this is also the count of
    /// handles taken: a handle is taken when its message is cut.
    pub handles_parsed: u64,
    /// A producer's reader: a handle value in a viewer's message is never
    /// ours to own, so on Windows it is not turned into one.
    producer: bool,
    eof: bool,
}

impl Reader {
    pub fn new() -> Reader {
        Reader::default()
    }

    pub fn for_producer() -> Reader {
        Reader {
            producer: true,
            ..Reader::default()
        }
    }

    pub fn at_eof(&self) -> bool {
        self.eof
    }

    /// True when a whole message may be waiting.
    pub fn has_buffered(&self) -> bool {
        self.buf.len() > self.start
    }

    /// One receive. Returns the byte count; 0 is end of stream. A
    /// non-blocking socket with nothing to read gives `WouldBlock`.
    pub fn fill(&mut self, sock: RawSock) -> Result<usize, Fault> {
        if self.start > 0 && self.start >= self.buf.len() {
            self.buf.clear();
            self.start = 0;
        } else if self.start > READ_CHUNK {
            self.buf.drain(..self.start);
            self.start = 0;
        }
        let old = self.buf.len();
        self.buf.resize(old + READ_CHUNK, 0);
        let (n, truncated) = match sys::recv(sock, &mut self.buf[old..], &mut self.fds) {
            Ok(r) => r,
            Err(e) => {
                self.buf.truncate(old);
                return Err(Fault::Io(e));
            }
        };
        self.buf.truncate(old + n);
        if truncated {
            return Err(Fault::Protocol("ancillary data truncated"));
        }
        if self.fds.len() > sys::MAX_UNMATCHED {
            return Err(Fault::Protocol("too many unmatched descriptors"));
        }
        if n == 0 {
            self.eof = true;
        }
        Ok(n)
    }

    /// Hand the next whole message, if one is buffered, to `f` with its
    /// handle. The slice is the message including its header.
    pub fn next_with<T>(
        &mut self,
        f: impl FnOnce(Header, &[u8], Option<Passed>) -> T,
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
            if cfg!(windows) && self.producer {
                None
            } else {
                Some(sys::take_passed(&mut self.fds, &avail[..len]).map_err(Fault::Protocol)?)
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

pub use super::sys::send;

/// Write all of `data` on a blocking socket.
pub fn send_all(sock: &impl AsSock, mut data: &[u8], mut fd: Option<RawPassed>) -> io::Result<()> {
    while !data.is_empty() {
        match send(sock.sock(), data, fd) {
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
    use crate::v4::sys::UnixStream;
    use crate::v4::wire;

    fn region(id: u32) -> Vec<u8> {
        wire::Region {
            id,
            kind: 3,
            storage: 1,
            access: 1,
            size: 65536,
            payload: vec![],
        }
        .encode()
    }

    #[test]
    fn messages_survive_any_read_boundary() {
        let (a, b) = UnixStream::pair().unwrap();
        let m1 = wire::handles_seen(7);
        let m2 = wire::input_reset();
        let mut all = m1.clone();
        all.extend_from_slice(&m2);
        let mut r = Reader::new();
        let mut got = Vec::new();
        for byte in all {
            send_all(&a, &[byte], None).unwrap();
            r.fill(b.sock()).unwrap();
            while let Some(h) = r.next_with(|h, _, _| h.ty).unwrap() {
                got.push(h);
            }
        }
        assert_eq!(got, vec![0x0004, 0x0016]);
    }

    #[cfg(unix)]
    #[test]
    fn a_handle_message_takes_one_descriptor_in_order() {
        use std::os::fd::AsRawFd;
        let (a, b) = UnixStream::pair().unwrap();
        let f1 = std::fs::File::open("/dev/null").unwrap();
        let f2 = std::fs::File::open("/dev/zero").unwrap();
        send_all(&a, &region(1), Some(f1.as_raw_fd())).unwrap();
        send_all(&a, &region(2), Some(f2.as_raw_fd())).unwrap();
        let mut r = Reader::new();
        let mut n = 0;
        while n < 2 {
            r.fill(b.sock()).unwrap();
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

    #[cfg(windows)]
    #[test]
    fn a_handle_message_takes_the_value_in_its_last_eight_bytes() {
        use std::os::windows::io::AsRawHandle;
        let (a, b) = UnixStream::pair().unwrap();
        let own = std::fs::File::open("NUL").unwrap();
        let mut m = region(1);
        let at = m.len() - 8;
        m[at..].copy_from_slice(&(own.as_raw_handle() as usize as u64).to_le_bytes());
        send_all(&a, &m, None).unwrap();
        let mut r = Reader::new();
        r.fill(b.sock()).unwrap();
        // The value is a live handle here, so dropping the Passed would close
        // it; the test hands back a duplicate instead.
        let got = r
            .next_with(|_, _, h| h.map(|h| h.try_clone().is_ok()))
            .unwrap();
        assert_eq!(got, Some(Some(true)));
        assert_eq!(r.handles_parsed, 1);
        std::mem::forget(own);
    }

    #[test]
    fn a_handle_message_without_a_handle_is_a_fault() {
        let (a, b) = UnixStream::pair().unwrap();
        send_all(&a, &region(1), None).unwrap();
        let mut r = Reader::new();
        r.fill(b.sock()).unwrap();
        assert!(matches!(r.next_with(|_, _, _| ()), Err(Fault::Protocol(_))));
    }
}
