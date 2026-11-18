use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::time::Duration;

/// The longest line either side accepts. A wait-serial pattern or a
/// screenshot path is far smaller; this only bounds a hostile peer.
pub const MAX_LINE: usize = 1 << 20;

#[derive(Debug)]
pub enum FrameError {
    Io(io::Error),
    TooLong,
}

impl From<io::Error> for FrameError {
    fn from(e: io::Error) -> Self {
        FrameError::Io(e)
    }
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FrameError::Io(e) => write!(f, "{e}"),
            FrameError::TooLong => write!(f, "line longer than {MAX_LINE} bytes"),
        }
    }
}

impl std::error::Error for FrameError {}

/// Reads newline-terminated lines and keeps what it read past the last one,
/// so a connection can switch to raw bytes without losing any.
pub struct LineReader<R> {
    inner: R,
    buf: Vec<u8>,
    start: usize,
    max: usize,
}

impl<R: Read> LineReader<R> {
    pub fn new(inner: R) -> Self {
        Self::with_max(inner, MAX_LINE)
    }

    pub fn with_max(inner: R, max: usize) -> Self {
        LineReader {
            inner,
            buf: Vec::new(),
            start: 0,
            max,
        }
    }

    /// The stream being read.
    pub fn get_ref(&self) -> &R {
        &self.inner
    }

    /// The next line without its terminator (and any `\r`), or `None` at a
    /// clean end of stream. A final unterminated line is returned too.
    pub fn read_line(&mut self) -> Result<Option<Vec<u8>>, FrameError> {
        let mut scan = self.start;
        loop {
            if let Some(i) = self.buf[scan..].iter().position(|&b| b == b'\n') {
                let end = scan + i;
                if end - self.start > self.max {
                    return Err(FrameError::TooLong);
                }
                let mut line = self.buf[self.start..end].to_vec();
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                self.start = end + 1;
                if self.start == self.buf.len() {
                    self.buf.clear();
                    self.start = 0;
                }
                return Ok(Some(line));
            }
            if self.buf.len() - self.start > self.max {
                return Err(FrameError::TooLong);
            }
            // Compact so a long-lived connection's buffer stays small.
            self.buf.drain(..self.start);
            scan = self.buf.len();
            self.start = 0;
            let mut chunk = [0u8; 8192];
            let n = loop {
                match self.inner.read(&mut chunk) {
                    Ok(n) => break n,
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(e) => return Err(e.into()),
                }
            };
            if n == 0 {
                if self.start < self.buf.len() {
                    let rest = self.buf[self.start..].to_vec();
                    self.buf.clear();
                    self.start = 0;
                    return Ok(Some(rest));
                }
                return Ok(None);
            }
            self.buf.extend_from_slice(&chunk[..n]);
        }
    }

    /// The reader and the bytes read but not yet returned.
    pub fn into_parts(self) -> (R, Vec<u8>) {
        let pending = self.buf[self.start..].to_vec();
        (self.inner, pending)
    }
}

pub fn write_line(w: &mut impl Write, line: &str) -> io::Result<()> {
    let mut out = Vec::with_capacity(line.len() + 1);
    out.extend_from_slice(line.as_bytes());
    out.push(b'\n');
    w.write_all(&out)?;
    w.flush()
}

/// A connection after the `console` reply: raw bytes both ways. Reads
/// return the bytes buffered behind the reply line first.
pub struct RawStream {
    stream: TcpStream,
    pending: Vec<u8>,
    pos: usize,
}

impl RawStream {
    pub(crate) fn new(stream: TcpStream, pending: Vec<u8>) -> Self {
        RawStream {
            stream,
            pending,
            pos: 0,
        }
    }

    /// A second handle for the other direction. Only the original carries
    /// the buffered bytes, so take the clone for writing.
    pub fn try_clone_writer(&self) -> io::Result<TcpStream> {
        self.stream.try_clone()
    }

    pub fn set_read_timeout(&self, t: Option<Duration>) -> io::Result<()> {
        self.stream.set_read_timeout(t)
    }

    pub fn set_write_timeout(&self, t: Option<Duration>) -> io::Result<()> {
        self.stream.set_write_timeout(t)
    }

    pub fn shutdown(&self) -> io::Result<()> {
        self.stream.shutdown(Shutdown::Both)
    }
}

impl Read for RawStream {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.pos < self.pending.len() {
            let n = out.len().min(self.pending.len() - self.pos);
            out[..n].copy_from_slice(&self.pending[self.pos..self.pos + n]);
            self.pos += n;
            return Ok(n);
        }
        self.stream.read(out)
    }
}

impl Write for RawStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.stream.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}
