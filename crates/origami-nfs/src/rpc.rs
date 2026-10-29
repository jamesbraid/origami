//! ONC RPC (RFC 5531): call dispatch, the UDP reply cache and TCP record
//! marking. The core takes a datagram or a byte stream, so a transport
//! other than host sockets can carry it.

use std::io::{self, Read, Write};
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use crate::fs::FileSystem;
use crate::xdr::{Reader, Writer};

const CALL_MSG: u32 = 0;
const REPLY_MSG: u32 = 1;
const MSG_ACCEPTED: u32 = 0;
const MSG_DENIED: u32 = 1;

// accept_stat
const SUCCESS: u32 = 0;
const PROG_UNAVAIL: u32 = 1;
const PROG_MISMATCH: u32 = 2;
const PROC_UNAVAIL: u32 = 3;
const GARBAGE_ARGS: u32 = 4;

// reject_stat
const RPC_MISMATCH: u32 = 0;

pub(crate) const PROG_PORTMAP: u32 = 100000;
pub(crate) const PROG_NFS: u32 = 100003;
pub(crate) const PROG_MOUNT: u32 = 100005;

/// The ports portmap advertises on one transport. A zero port leaves that
/// program unregistered there.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Ports {
    pub portmap: u32,
    pub mount: u32,
    pub nfs: u32,
}

/// Where a log line comes from, least to most important.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum LogLevel {
    Debug,
    Info,
    Warn,
}

pub type Logger = Arc<dyn Fn(LogLevel, &str) + Send + Sync>;

/// Serves the portmap, MOUNT and NFS protocols over one filesystem. Every
/// transport answers all three, so a client that knows the port can skip
/// portmap entirely.
pub struct Server {
    pub(crate) fs: Arc<dyn FileSystem>,
    allow_ip: Option<Box<dyn Fn(IpAddr) -> bool + Send + Sync>>,
    logger: Option<Logger>,
    /// The write verifier WRITE and COMMIT return (RFC 1813 section
    /// 3.3.7). It must change when the server restarts, so a client knows
    /// to resend unstable writes; every write here is stable, so it only
    /// matters to clients that check.
    pub(crate) verifier: [u8; 8],
    /// What portmap advertises, for UDP then TCP.
    ports: RwLock<Option<[Ports; 2]>>,
    drc: Mutex<Drc>,
}

impl Server {
    /// A server for `fs`. The write verifier comes from the start time.
    pub fn new(fs: Arc<dyn FileSystem>) -> Server {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(1);
        Server {
            fs,
            allow_ip: None,
            logger: None,
            verifier: nanos.to_be_bytes(),
            ports: RwLock::new(None),
            drc: Mutex::new(Drc::default()),
        }
    }

    /// Fixes the write verifier, as a test needs.
    pub fn with_verifier(mut self, verifier: [u8; 8]) -> Server {
        self.verifier = verifier;
        self
    }

    /// Admits only clients `allow` accepts. Without it everyone is let in.
    pub fn with_allow(mut self, allow: impl Fn(IpAddr) -> bool + Send + Sync + 'static) -> Server {
        self.allow_ip = Some(Box::new(allow));
        self
    }

    /// Sends leveled log output to `logger`: debug for every call, info
    /// for a mount and each file opened, warn for a refused client.
    pub fn with_logger(mut self, logger: Logger) -> Server {
        self.logger = Some(logger);
        self
    }

    /// Records what portmap GETPORT and DUMP report for UDP and TCP.
    pub fn set_ports(&self, udp: Ports, tcp: Ports) {
        *self.ports.write().unwrap() = Some([udp, tcp]);
    }

    pub(crate) fn ports(&self) -> Option<[Ports; 2]> {
        *self.ports.read().unwrap()
    }

    pub(crate) fn log(&self, level: LogLevel, msg: impl FnOnce() -> String) {
        if let Some(l) = &self.logger {
            l(level, &msg());
        }
    }

    fn allowed(&self, addr: SocketAddr) -> bool {
        let Some(allow) = &self.allow_ip else {
            return true;
        };
        let ip = match addr.ip() {
            IpAddr::V6(v6) => v6
                .to_ipv4_mapped()
                .map(IpAddr::V4)
                .unwrap_or(IpAddr::V6(v6)),
            ip => ip,
        };
        if !allow(ip) {
            self.log(LogLevel::Warn, || {
                format!("nfs: {addr}: denied by client filter")
            });
            return false;
        }
        true
    }

    /// Answers one UDP datagram from `from`, or returns None when nothing
    /// is to be sent: the client is not admitted, or the packet is not a
    /// call. A retransmitted call that changes the share is answered from
    /// the reply cache.
    pub fn handle_datagram(&self, from: SocketAddr, pkt: &[u8]) -> Option<Vec<u8>> {
        if !self.allowed(from) {
            return None;
        }
        if let Some(reply) = self.drc.lock().unwrap().lookup(from, pkt) {
            return reply;
        }
        let reply = self.dispatch(from, pkt);
        self.drc.lock().unwrap().store(from, pkt, &reply);
        reply
    }

    /// Whether a TCP peer is admitted. A transport checks this on accept
    /// before calling [`serve_stream`](Server::serve_stream).
    pub fn admits(&self, peer: SocketAddr) -> bool {
        self.allowed(peer)
    }

    /// Answers calls on a byte stream with RFC 5531 record marking, one at
    /// a time, until the peer closes it or `stop` is set. The stream must
    /// make a blocked read return, with a timeout or an error, for `stop`
    /// to take effect; without a `stop` it runs until the stream ends.
    pub fn serve_stream<S: Read + Write>(
        &self,
        peer: SocketAddr,
        stream: &mut S,
        stop: Option<&AtomicBool>,
    ) -> io::Result<()> {
        loop {
            let call = match read_record(stream, stop)? {
                Some(c) => c,
                None => return Ok(()),
            };
            let Some(reply) = self.dispatch(peer, &call) else {
                continue;
            };
            stream.write_all(&record_mark(&reply))?;
        }
    }

    /// Decodes one call message and returns the reply message, or None
    /// when the message is not a call.
    pub fn dispatch(&self, addr: SocketAddr, pkt: &[u8]) -> Option<Vec<u8>> {
        let mut r = Reader::new(pkt);
        let xid = r.u32();
        if r.u32() != CALL_MSG || r.err {
            return None;
        }
        if r.u32() != 2 {
            return Some(reply_denied(xid));
        }
        let prog = r.u32();
        let vers = r.u32();
        let proc_ = r.u32();
        // Credential and verifier, each a flavor and an opaque body. Every
        // caller is treated alike, so neither is examined.
        r.u32();
        skip_opaque(&mut r);
        r.u32();
        skip_opaque(&mut r);
        if r.err {
            return Some(reply_accepted(xid, GARBAGE_ARGS, &[]));
        }

        let Some((lo, hi)) = version_range(prog) else {
            return Some(reply_accepted(xid, PROG_UNAVAIL, &[]));
        };
        // Every served version answers NULL, so a missing NULL is a
        // missing version.
        if !serves(prog, vers) {
            let mut w = Writer::new();
            w.u32(lo);
            w.u32(hi);
            return Some(reply_accepted(xid, PROG_MISMATCH, &w.b));
        }
        let Some(body) = self.call(addr, prog, vers, proc_, &mut r) else {
            self.log(LogLevel::Debug, || {
                format!("nfs: {addr}: prog={prog} vers={vers} proc={proc_} unavailable")
            });
            return Some(reply_accepted(xid, PROC_UNAVAIL, &[]));
        };
        self.log(LogLevel::Debug, || {
            format!("nfs: {addr}: prog={prog} vers={vers} proc={proc_}")
        });
        if r.err {
            return Some(reply_accepted(xid, GARBAGE_ARGS, &[]));
        }
        Some(reply_accepted(xid, SUCCESS, &body))
    }

    /// Runs a procedure, or returns None when the program and version
    /// don't have it.
    fn call(
        &self,
        caller: SocketAddr,
        prog: u32,
        vers: u32,
        proc_: u32,
        r: &mut Reader,
    ) -> Option<Vec<u8>> {
        match (prog, vers) {
            (PROG_PORTMAP, 2) => self.portmap_call(proc_, r),
            (PROG_MOUNT, 1 | 3) => self.mount_call(caller, vers, proc_, r),
            (PROG_NFS, 2) => self.nfs2_call(proc_, r),
            (PROG_NFS, 3) => self.nfs3_call(proc_, r),
            _ => None,
        }
    }
}

/// The version range of each program, for PROG_MISMATCH replies.
fn version_range(prog: u32) -> Option<(u32, u32)> {
    match prog {
        PROG_PORTMAP => Some((2, 2)),
        PROG_MOUNT => Some((1, 3)),
        PROG_NFS => Some((2, 3)),
        _ => None,
    }
}

fn serves(prog: u32, vers: u32) -> bool {
    matches!(
        (prog, vers),
        (PROG_PORTMAP, 2) | (PROG_MOUNT, 1 | 3) | (PROG_NFS, 2 | 3)
    )
}

fn skip_opaque(r: &mut Reader) {
    let n = r.u32() as usize;
    if n > r.b.len() {
        r.err = true;
        return;
    }
    r.off += (n + 3) & !3;
    if r.off > r.b.len() {
        r.err = true;
    }
}

/// Frames an accepted RPC reply: xid, REPLY, MSG_ACCEPTED, null verifier,
/// accept_stat, then the body.
fn reply_accepted(xid: u32, accept_stat: u32, body: &[u8]) -> Vec<u8> {
    let mut w = Writer::new();
    w.u32(xid);
    w.u32(REPLY_MSG);
    w.u32(MSG_ACCEPTED);
    w.u32(0); // verf flavor AUTH_NULL
    w.u32(0); // verf length
    w.u32(accept_stat);
    w.fixed(body);
    w.b
}

/// Refuses a call for an RPC version other than 2, naming the one version
/// served.
fn reply_denied(xid: u32) -> Vec<u8> {
    let mut w = Writer::new();
    w.u32(xid);
    w.u32(REPLY_MSG);
    w.u32(MSG_DENIED);
    w.u32(RPC_MISMATCH);
    w.u32(2);
    w.u32(2);
    w.b
}

/// A duplicate request cache for UDP. A client that times out resends the
/// same call, and a second REMOVE or RENAME would fail although the first
/// succeeded, so the replies to those calls are kept and resent instead.
/// Idempotent calls (reads, and a WRITE of the same bytes to the same
/// place) just run again.
#[derive(Default)]
pub(crate) struct Drc {
    ent: Vec<DrcEntry>,
    next: usize,
}

struct DrcEntry {
    addr: SocketAddr,
    call: Vec<u8>,
    reply: Option<Vec<u8>>,
}

const DRC_SIZE: usize = 64;

/// Whether `pkt` is a call the cache should keep: an NFS SETATTR, CREATE,
/// REMOVE, RENAME, LINK, SYMLINK, MKDIR, RMDIR or MKNOD.
pub(crate) fn non_idempotent(pkt: &[u8]) -> bool {
    if pkt.len() < 24 {
        return false;
    }
    let word = |off: usize| u32::from_be_bytes(pkt[off..off + 4].try_into().unwrap());
    if word(12) != PROG_NFS {
        return false;
    }
    let proc_ = word(20);
    match word(16) {
        2 => proc_ == 2 || (9..=15).contains(&proc_),
        3 => proc_ == 2 || (8..=15).contains(&proc_),
        _ => false,
    }
}

impl Drc {
    /// The kept reply for a retransmission: Some(reply) when the call was
    /// seen, where the reply itself may be None for a call that got none.
    pub fn lookup(&self, addr: SocketAddr, call: &[u8]) -> Option<Option<Vec<u8>>> {
        if !non_idempotent(call) {
            return None;
        }
        self.ent
            .iter()
            .find(|e| e.addr == addr && e.call == call)
            .map(|e| e.reply.clone())
    }

    pub fn store(&mut self, addr: SocketAddr, call: &[u8], reply: &Option<Vec<u8>>) {
        if !non_idempotent(call) {
            return;
        }
        let e = DrcEntry {
            addr,
            call: call.to_vec(),
            reply: reply.clone(),
        };
        if self.ent.len() < DRC_SIZE {
            self.ent.push(e);
        } else {
            self.ent[self.next] = e;
        }
        self.next = (self.next + 1) % DRC_SIZE;
    }
}

/// Bounds one TCP call. The largest legitimate call is a WRITE of wtmax
/// bytes plus its header; anything far beyond that is a broken or hostile
/// client, and the connection is dropped.
pub(crate) const MAX_RECORD: usize = 1 << 20;

/// Reads until `buf` is full. Returns the count read, which is short only
/// at end of stream. A read that times out is retried unless `stop` is
/// set.
fn read_full<R: Read>(r: &mut R, buf: &mut [u8], stop: Option<&AtomicBool>) -> io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match r.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                if stop.is_some_and(|s| s.load(Ordering::Relaxed)) {
                    return Err(io::Error::new(
                        io::ErrorKind::Interrupted,
                        "server stopping",
                    ));
                }
            }
            Err(e) => return Err(e),
        }
    }
    Ok(n)
}

/// Reads one record: fragments, each behind a 4-byte header whose top bit
/// marks the last and whose low 31 bits are its length (RFC 5531 section
/// 11). Returns None at a clean end of stream between records.
pub(crate) fn read_record<R: Read>(
    r: &mut R,
    stop: Option<&AtomicBool>,
) -> io::Result<Option<Vec<u8>>> {
    let mut rec: Vec<u8> = Vec::new();
    loop {
        let mut hdr = [0u8; 4];
        match read_full(r, &mut hdr, stop)? {
            4 => {}
            0 if rec.is_empty() => return Ok(None),
            _ => return Err(io::ErrorKind::UnexpectedEof.into()),
        }
        let h = u32::from_be_bytes(hdr);
        let n = (h & 0x7fff_ffff) as usize;
        if rec.len() + n > MAX_RECORD {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "rpc record too large",
            ));
        }
        let start = rec.len();
        rec.resize(start + n, 0);
        if read_full(r, &mut rec[start..], stop)? != n {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        if h & 0x8000_0000 != 0 {
            return Ok(Some(rec));
        }
    }
}

/// Frames `msg` as a single last fragment.
pub(crate) fn record_mark(msg: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + msg.len());
    out.extend_from_slice(&(0x8000_0000 | msg.len() as u32).to_be_bytes());
    out.extend_from_slice(msg);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fragment(last: bool, p: &[u8]) -> Vec<u8> {
        let mut h = p.len() as u32;
        if last {
            h |= 0x8000_0000;
        }
        let mut v = h.to_be_bytes().to_vec();
        v.extend_from_slice(p);
        v
    }

    // A record may arrive in several fragments; only the last carries the
    // top bit (RFC 5531 section 11).
    #[test]
    fn read_record_joins_fragments() {
        let mut input = Vec::new();
        input.extend(fragment(false, b"abcd"));
        input.extend(fragment(false, b""));
        input.extend(fragment(true, b"ef"));
        input.extend(fragment(true, b"next"));
        let mut r = &input[..];
        assert_eq!(read_record(&mut r, None).unwrap().unwrap(), b"abcdef");
        assert_eq!(read_record(&mut r, None).unwrap().unwrap(), b"next");
        assert!(read_record(&mut r, None).unwrap().is_none());
    }

    #[test]
    fn read_record_errors() {
        let big = (0x8000_0000u32 | (MAX_RECORD as u32 + 1)).to_be_bytes();
        let e = read_record(&mut &big[..], None).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidData);

        let cut = fragment(true, b"abcd");
        let e = read_record(&mut &cut[..6], None).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::UnexpectedEof);

        let open = fragment(false, b"abcd");
        let e = read_record(&mut &open[..], None).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::UnexpectedEof);
    }
}
