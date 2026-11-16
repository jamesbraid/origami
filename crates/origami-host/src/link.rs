//! The runner's one connection to QEMU's console chardev.
//!
//! QEMU's socket chardev serves a single client and leaves a second one
//! waiting in the backlog, so exactly one thing may connect: this link. It
//! connects when something first wants the console, not at launch, so a
//! guest that has not been looked at yet has nothing to lose: QEMU writes
//! every byte to `logs/serial.log` whether or not anyone is connected, and
//! the link feeds what the log gained meanwhile into the ring before it
//! starts reading live.

use crate::broker::{run_producer, InputSink, RingWriter};
use std::fs;
use std::io::{self, Read, Seek, SeekFrom};
use std::net::{Ipv4Addr, SocketAddrV4, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

/// How much of the serial log primes the ring before the machine starts.
pub const PRIME_BYTES: u64 = 1 << 20;

pub struct Link {
    port: AtomicU16,
    log: PathBuf,
    /// Where the serial log stood when the runner began: later bytes are the
    /// guest's and reach the ring on first connect.
    log_from: u64,
    inner: Mutex<Inner>,
}

struct Inner {
    writer: Option<RingWriter>,
    stream: Option<TcpStream>,
}

impl Link {
    /// The tail of the serial log (to prime the ring with) and a link that
    /// has not connected yet.
    pub fn new(dir: &Path) -> (Arc<Link>, Vec<u8>) {
        let log = dir.join("logs/serial.log");
        let (tail, len) = read_tail(&log, PRIME_BYTES);
        let link = Arc::new(Link {
            port: AtomicU16::new(0),
            log,
            log_from: len,
            inner: Mutex::new(Inner {
                writer: None,
                stream: None,
            }),
        });
        (link, tail)
    }

    /// The single writer end of the ring, once the broker has made it.
    pub fn set_writer(&self, writer: RingWriter) {
        self.lock().writer = Some(writer);
    }

    /// QEMU's chardev port, known once QEMU has been launched.
    pub fn set_port(&self, port: u16) {
        self.port.store(port, Ordering::Release);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Connect if nothing has yet. Cheap once connected.
    pub fn ensure(&self) -> io::Result<()> {
        let mut g = self.lock();
        if g.stream.is_some() {
            return Ok(());
        }
        let port = self.port.load(Ordering::Acquire);
        if port == 0 {
            return Err(io::Error::other("the console port is not known yet"));
        }
        let Some(mut writer) = g.writer.take() else {
            return Err(io::Error::other("the console ring is closed"));
        };
        let addr = SocketAddrV4::new(Ipv4Addr::LOCALHOST, port);
        // Read before connecting, so no byte is both here and on the socket:
        // one logged in between is lost to the ring, never to the log.
        let (missed, _) = read_range(&self.log, self.log_from);
        let stream = match TcpStream::connect_timeout(&addr.into(), Duration::from_secs(2)) {
            Ok(s) => s,
            Err(e) => {
                g.writer = Some(writer);
                return Err(e);
            }
        };
        let _ = stream.set_nodelay(true);
        let reader = match stream.try_clone() {
            Ok(r) => r,
            Err(e) => {
                g.writer = Some(writer);
                return Err(e);
            }
        };
        writer.append(&missed);
        g.stream = Some(stream);
        drop(g);
        thread::Builder::new()
            .name("console-producer".into())
            .spawn(move || {
                let _ = run_producer(writer, reader);
            })?;
        Ok(())
    }

    /// Send `data` to the guest; connects first if need be.
    fn write(&self, data: &[u8], timeout: Duration) -> io::Result<()> {
        self.ensure()?;
        let mut stream = match self.lock().stream.as_ref().map(TcpStream::try_clone) {
            Some(s) => s?,
            None => return Err(io::Error::other("the console is not connected")),
        };
        stream.write_input(data, timeout)
    }
}

/// The broker's input sink: the link, connecting on first use.
pub struct LinkSink(pub Arc<Link>);

impl InputSink for LinkSink {
    fn write_input(&mut self, data: &[u8], timeout: Duration) -> io::Result<()> {
        self.0.write(data, timeout)
    }
}

/// The last `max` bytes of the file and its length; nothing if it is absent.
fn read_tail(path: &Path, max: u64) -> (Vec<u8>, u64) {
    let Ok(mut f) = fs::File::open(path) else {
        return (Vec::new(), 0);
    };
    let len = f.metadata().map_or(0, |m| m.len());
    let from = len.saturating_sub(max);
    let mut out = Vec::new();
    if f.seek(SeekFrom::Start(from)).is_ok() {
        let _ = f.take(max).read_to_end(&mut out);
    }
    (out, len)
}

/// The file's bytes from `from` on, capped at the priming size.
fn read_range(path: &Path, from: u64) -> (Vec<u8>, u64) {
    let Ok(mut f) = fs::File::open(path) else {
        return (Vec::new(), from);
    };
    let len = f.metadata().map_or(0, |m| m.len());
    let start = from.max(len.saturating_sub(PRIME_BYTES));
    let mut out = Vec::new();
    if len > start && f.seek(SeekFrom::Start(start)).is_ok() {
        let _ = f.take(PRIME_BYTES).read_to_end(&mut out);
    }
    (out, len)
}
