//! The viewer side of a version 4 session: handshake, regions, heads, and
//! frame leases that release themselves.

use super::regions::{Expect, Mapping, StateView};
use super::transport::{send_all, Fault, Reader};
use super::wire::{self, ErrorMsg, Frame, Head, Hello, Offer, Region, ToViewer, Welcome};
use std::collections::{HashMap, VecDeque};
use std::io;
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Debug)]
pub enum SessionError {
    Io(io::Error),
    /// The producer broke the protocol.
    Protocol(String),
    /// The producer sent `ERROR` and closed.
    Producer(ErrorMsg),
    Closed,
}

impl std::fmt::Display for SessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SessionError::Io(e) => write!(f, "{e}"),
            SessionError::Protocol(s) => write!(f, "producer broke the protocol: {s}"),
            SessionError::Producer(e) => write!(f, "producer error {}: {}", e.code, e.text),
            SessionError::Closed => f.write_str("the producer closed the connection"),
        }
    }
}
impl std::error::Error for SessionError {}

impl From<io::Error> for SessionError {
    fn from(e: io::Error) -> Self {
        SessionError::Io(e)
    }
}
impl From<Fault> for SessionError {
    fn from(f: Fault) -> Self {
        match f {
            Fault::Io(e) => SessionError::Io(e),
            Fault::Protocol(s) => SessionError::Protocol(s.into()),
        }
    }
}
impl From<wire::WireError> for SessionError {
    fn from(e: wire::WireError) -> Self {
        SessionError::Protocol(e.0.into())
    }
}

/// What the viewer tells the producer in `HELLO`.
#[derive(Debug, Clone)]
pub struct HelloParams {
    pub name: String,
    pub features: u32,
    /// Preference order; `shm` (1) is appended if missing.
    pub storage: Vec<u32>,
    pub credit: u32,
    pub max_width: u32,
    pub max_height: u32,
    pub max_slot_bytes: u32,
    pub offers: Vec<Offer>,
}

impl Default for HelloParams {
    fn default() -> Self {
        HelloParams {
            name: "origami".into(),
            features: wire::feat::INPUT,
            storage: vec![wire::STORAGE_SHM],
            credit: 1,
            max_width: 16384,
            max_height: 16384,
            max_slot_bytes: 0,
            offers: vec![],
        }
    }
}

#[derive(Default, Clone)]
struct HeadState {
    info: Option<Head>,
    frame_seen: u64,
    /// A frame request is outstanding: the release rule re-sends it.
    want_frame: bool,
    cursor_seen: u32,
}

struct Shared {
    sock: Mutex<UnixStream>,
    feedback: Mapping,
    heads: Mutex<Vec<HeadState>>,
    closed: Arc<AtomicBool>,
}

impl Shared {
    fn write(&self, m: &[u8]) -> io::Result<()> {
        let s = self.sock.lock().unwrap();
        send_all(&*s, m, None)
    }

    /// A write failed: the producer would never hear the request, so end the
    /// session. `recv` then sees the stream end.
    fn fail(&self) {
        self.closed.store(true, Ordering::Release);
        let _ = self.sock.lock().unwrap().shutdown(std::net::Shutdown::Both);
    }

    fn store_release(&self, head: u32, lease: u16, seq: u64) {
        let off = wire::feedback::HEADER
            + head as usize * wire::feedback::HEAD_STRIDE
            + lease as usize * 8;
        self.feedback.store64(off, seq, Ordering::Release);
    }
}

/// A slot lent to the viewer. Dropping it ends the lease: the release is
/// stored and a request still outstanding is sent again, so the last frame
/// before the screen goes still is never lost.
pub struct FrameLease {
    shared: Arc<Shared>,
    pub frame: Frame,
    pub slot: Arc<Mapping>,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub fourcc: u32,
    /// The frame belongs to a mode that is no longer current: release, don't draw.
    pub stale_mode: bool,
    live: bool,
}

impl FrameLease {
    /// The slot's pixels, `stride * height` bytes. The producer never writes a lent slot.
    pub fn pixels(&self) -> &[u8] {
        // SAFETY: the lease is the producer's promise not to write this slot.
        unsafe {
            self.slot
                .bytes(0, self.stride as usize * self.height as usize)
        }
    }

    /// End the lease and re-send an outstanding request.
    pub fn release(mut self) {
        self.end(true);
    }

    /// End the lease without the release rule's re-sent request.
    pub fn release_quiet(mut self) {
        self.end(false);
    }

    fn end(&mut self, resend: bool) {
        if !std::mem::replace(&mut self.live, false) {
            return;
        }
        let f = &self.frame;
        self.shared.store_release(f.head, f.lease, f.lease_seq);
        if !resend {
            return;
        }
        let req = {
            let heads = self.shared.heads.lock().unwrap();
            heads
                .get(f.head as usize)
                .filter(|h| h.want_frame)
                .map(|h| wire::Request {
                    head: f.head,
                    kinds: wire::REQ_FRAME,
                    frame_seen: h.frame_seen,
                    cursor_seen: h.cursor_seen,
                    flags: 0,
                })
        };
        if let Some(r) = req {
            if self.shared.write(&r.encode()).is_err() {
                self.shared.fail();
            }
        }
    }
}

impl Drop for FrameLease {
    fn drop(&mut self) {
        self.end(true);
    }
}

pub enum Event {
    Frame(FrameLease),
    Head(Head),
    CursorNotify {
        head: u32,
        cursor_seq: u32,
    },
    CursorImage(wire::CursorImage),
    InputAck(u32),
    InputState {
        flags: u32,
        leds: u32,
    },
    RegionGone(u32),
    /// An extension message, whole, header included.
    Ext {
        ty: u16,
        msg: Vec<u8>,
    },
    /// A core message the producer sent that this version does not know.
    Unknown {
        ty: u16,
    },
}

/// The part of a session other threads use: requests and encoded messages go
/// out on the shared socket while one thread reads events.
#[derive(Clone)]
pub struct SessionHandle(Arc<Shared>);

impl SessionHandle {
    /// The latest description of a head.
    pub fn head(&self, head: u32) -> Option<Head> {
        self.0
            .heads
            .lock()
            .unwrap()
            .get(head as usize)?
            .info
            .clone()
    }

    pub fn head_count(&self) -> usize {
        self.0.heads.lock().unwrap().len()
    }

    /// Ask for a frame. Records the request so a release re-sends it.
    pub fn request(&self, head: u32, sample_now: bool) -> io::Result<()> {
        let r = {
            let mut heads = self.0.heads.lock().unwrap();
            let h = heads
                .get_mut(head as usize)
                .ok_or(io::ErrorKind::NotFound)?;
            h.want_frame = true;
            wire::Request {
                head,
                kinds: wire::REQ_FRAME,
                frame_seen: h.frame_seen,
                cursor_seen: h.cursor_seen,
                flags: if sample_now { wire::REQ_SAMPLE_NOW } else { 0 },
            }
        };
        self.0.write(&r.encode())
    }

    /// Ask to be told when the cursor differs from `cursor_seen`.
    pub fn request_cursor(&self, head: u32, cursor_seen: u32) -> io::Result<()> {
        let frame_seen = {
            let mut heads = self.0.heads.lock().unwrap();
            let h = heads
                .get_mut(head as usize)
                .ok_or(io::ErrorKind::NotFound)?;
            h.cursor_seen = cursor_seen;
            h.frame_seen
        };
        let r = wire::Request {
            head,
            kinds: wire::REQ_CURSOR,
            frame_seen,
            cursor_seen,
            flags: 0,
        };
        self.0.write(&r.encode())
    }

    /// Send any message already encoded, as one write.
    pub fn send(&self, m: &[u8]) -> io::Result<()> {
        self.0.write(m)
    }

    /// End the session: the thread reading events sees the stream end.
    pub fn close(&self) {
        self.0.fail();
    }
}

/// A connected, handshaken session.
pub struct Session {
    shared: Arc<Shared>,
    sock: UnixStream,
    reader: Reader,
    pending: VecDeque<Event>,
    pub welcome: Welcome,
    pub state: Arc<StateView>,
    regions: HashMap<u32, Arc<Mapping>>,
    ready: bool,
    handles_seen_sent: u64,
    pub frames_received: u64,
}

fn ms(t: Option<Duration>) -> i32 {
    t.map_or(-1, |d| d.as_millis().min(i32::MAX as u128) as i32)
}

/// Wait up to `t` for `fd` to be readable. True when it is.
pub fn wait_readable(fd: &impl AsRawFd, t: Option<Duration>) -> io::Result<bool> {
    let mut p = libc::pollfd {
        fd: fd.as_raw_fd(),
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

impl Session {
    /// Say `HELLO` and read until `READY`.
    pub fn connect(stream: UnixStream, p: &HelloParams) -> Result<Session, SessionError> {
        let mut storage = [0u32; 8];
        let mut list = p.storage.clone();
        if !list.contains(&wire::STORAGE_SHM) {
            list.push(wire::STORAGE_SHM);
        }
        for (i, s) in list.iter().take(7).enumerate() {
            storage[i] = *s;
        }
        let hello = Hello {
            major: wire::MAJOR,
            minor: wire::MINOR,
            features: p.features,
            name: p.name.clone(),
            storage,
            credit: p.credit,
            max_width: p.max_width,
            max_height: p.max_height,
            prev_viewer_id: 0,
            max_slot_bytes: p.max_slot_bytes,
            prev_handles_taken: 0,
            prev_session_id: 0,
            offers: p.offers.clone(),
        };
        #[cfg(not(target_os = "linux"))]
        super::transport::no_sigpipe(&stream)?;
        send_all(&stream, &hello.encode(), None)?;
        Session::handshake(stream)
    }

    fn handshake(stream: UnixStream) -> Result<Session, SessionError> {
        let mut reader = Reader::new();
        let mut welcome: Option<Welcome> = None;
        let mut regions: HashMap<u32, (Region, Mapping)> = HashMap::new();
        let mut heads: Vec<HeadState> = vec![];
        let mut pre_cursor = vec![];
        loop {
            loop {
                let got = reader.next_with(|h, m, fd| (ToViewer::decode(h, m), fd))?;
                let Some((msg, fd)) = got else { break };
                match msg? {
                    ToViewer::Welcome(w) => welcome = Some(w),
                    ToViewer::Error(e) => return Err(SessionError::Producer(e)),
                    ToViewer::Region(r) => {
                        let m = map_region(&r, fd)?;
                        regions.insert(r.id, (r, m));
                    }
                    ToViewer::Head(h) => {
                        let n = h.head as usize;
                        if n >= wire::FEEDBACK_HEADS {
                            return Err(SessionError::Protocol("head beyond 511".into()));
                        }
                        if heads.len() <= n {
                            heads.resize(n + 1, HeadState::default());
                        }
                        heads[n].info = Some(h);
                    }
                    ToViewer::CursorImage(c) => pre_cursor.push(c),
                    ToViewer::Ready => {
                        let w = welcome
                            .ok_or_else(|| SessionError::Protocol("READY before WELCOME".into()))?;
                        return Session::finish(stream, reader, w, regions, heads, pre_cursor);
                    }
                    _ => {}
                }
            }
            if reader.at_eof() {
                return Err(SessionError::Closed);
            }
            match reader.fill(stream.as_raw_fd()) {
                Ok(_) => {}
                Err(Fault::Io(e)) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e.into()),
            }
        }
    }

    fn finish(
        stream: UnixStream,
        reader: Reader,
        welcome: Welcome,
        mut regions: HashMap<u32, (Region, Mapping)>,
        heads: Vec<HeadState>,
        pre_cursor: Vec<wire::CursorImage>,
    ) -> Result<Session, SessionError> {
        let closed = Arc::new(AtomicBool::new(false));
        let (_, state_map) = regions
            .remove(&welcome.state_region)
            .ok_or_else(|| SessionError::Protocol("no state region".into()))?;
        let (_, fb_map) = regions
            .remove(&welcome.feedback_region)
            .ok_or_else(|| SessionError::Protocol("no feedback region".into()))?;
        let state = Arc::new(
            StateView::new(state_map, closed.clone(), welcome.session_id)
                .map_err(|e| SessionError::Protocol(e.into()))?,
        );
        if fb_map.len() < wire::FEEDBACK_SIZE || !fb_map.writable() {
            return Err(SessionError::Protocol("feedback region".into()));
        }
        let shared = Arc::new(Shared {
            sock: Mutex::new(stream.try_clone()?),
            feedback: fb_map,
            heads: Mutex::new(heads),
            closed,
        });
        let mut s = Session {
            shared,
            sock: stream,
            reader,
            pending: VecDeque::new(),
            welcome,
            state,
            regions: regions
                .into_iter()
                .map(|(k, (_, m))| (k, Arc::new(m)))
                .collect(),
            ready: true,
            handles_seen_sent: 0,
            frames_received: 0,
        };
        for c in pre_cursor {
            s.pending.push_back(Event::CursorImage(c));
        }
        s.send_handles_seen(true)?;
        Ok(s)
    }

    fn send_handles_seen(&mut self, force: bool) -> io::Result<()> {
        let n = self.reader.handles_parsed;
        if n != self.handles_seen_sent && (force || n - self.handles_seen_sent >= 16) {
            self.handles_seen_sent = n;
            self.shared.write(&wire::handles_seen(n))?;
        }
        Ok(())
    }

    /// The latest description of a head.
    pub fn head(&self, head: u32) -> Option<Head> {
        self.handle().head(head)
    }

    pub fn head_count(&self) -> usize {
        self.handle().head_count()
    }

    /// Ask for a frame. Records the request so a release re-sends it.
    pub fn request(&self, head: u32, sample_now: bool) -> io::Result<()> {
        self.handle().request(head, sample_now)
    }

    /// Ask to be told when the cursor differs from `cursor_seen`.
    pub fn request_cursor(&self, head: u32, cursor_seen: u32) -> io::Result<()> {
        self.handle().request_cursor(head, cursor_seen)
    }

    /// Send any message already encoded.
    pub fn send(&self, m: &[u8]) -> io::Result<()> {
        self.shared.write(m)
    }

    /// A clonable end for other threads to ask and send through.
    pub fn handle(&self) -> SessionHandle {
        SessionHandle(self.shared.clone())
    }

    pub fn socket(&self) -> &UnixStream {
        &self.sock
    }

    pub fn credit(&self) -> u32 {
        self.welcome.credit
    }

    /// The next event, waiting up to `timeout` (`None`: forever). `Ok(None)` on timeout.
    pub fn recv(&mut self, timeout: Option<Duration>) -> Result<Option<Event>, SessionError> {
        let deadline = timeout.map(|t| std::time::Instant::now() + t);
        loop {
            if let Some(e) = self.pending.pop_front() {
                return Ok(Some(e));
            }
            if self.step()? {
                continue;
            }
            if self.reader.at_eof() {
                self.shared.closed.store(true, Ordering::Release);
                return Err(SessionError::Closed);
            }
            let left = match deadline {
                Some(d) => {
                    let now = std::time::Instant::now();
                    if now >= d {
                        return Ok(None);
                    }
                    Some(d - now)
                }
                None => None,
            };
            if wait_readable(&self.sock, left)? {
                match self.reader.fill(self.sock.as_raw_fd()) {
                    Ok(_) => {}
                    Err(Fault::Io(e)) if e.kind() == io::ErrorKind::Interrupted => {}
                    Err(e) => return Err(e.into()),
                }
            }
        }
    }

    /// Process one buffered message. False when none is buffered.
    fn step(&mut self) -> Result<bool, SessionError> {
        let was = self.reader.handles_parsed;
        let got = self
            .reader
            .next_with(|h, m, fd| (ToViewer::decode(h, m), fd))?;
        let Some((msg, fd)) = got else {
            return Ok(false);
        };
        match msg? {
            ToViewer::Frame(f) => self.on_frame(f)?,
            ToViewer::Region(r) => {
                let m = map_region(&r, fd)?;
                self.regions.insert(r.id, Arc::new(m));
            }
            ToViewer::RegionGone(id) => {
                self.regions.remove(&id);
                self.pending.push_back(Event::RegionGone(id));
            }
            ToViewer::Head(h) => {
                let n = h.head as usize;
                if n >= wire::FEEDBACK_HEADS {
                    return Err(SessionError::Protocol("head beyond 511".into()));
                }
                let mut heads = self.shared.heads.lock().unwrap();
                if heads.len() <= n {
                    heads.resize(n + 1, HeadState::default());
                }
                heads[n].info = Some(h.clone());
                drop(heads);
                self.pending.push_back(Event::Head(h));
            }
            ToViewer::CursorImage(c) => self.pending.push_back(Event::CursorImage(c)),
            ToViewer::CursorNotify { head, cursor_seq } => self
                .pending
                .push_back(Event::CursorNotify { head, cursor_seq }),
            ToViewer::InputAck(s) => self.pending.push_back(Event::InputAck(s)),
            ToViewer::InputState { flags, leds } => {
                self.pending.push_back(Event::InputState { flags, leds })
            }
            ToViewer::Error(e) => {
                self.shared.closed.store(true, Ordering::Release);
                return Err(SessionError::Producer(e));
            }
            ToViewer::Other { ty, msg, .. } => {
                if ty >= 0x0100 {
                    self.pending.push_back(Event::Ext { ty, msg });
                } else {
                    self.pending.push_back(Event::Unknown { ty });
                }
            }
            ToViewer::Welcome(_) | ToViewer::Orphans(_) | ToViewer::Ready => {}
        }
        if self.reader.handles_parsed != was {
            self.send_handles_seen(false)?;
        }
        Ok(true)
    }

    fn on_frame(&mut self, f: Frame) -> Result<(), SessionError> {
        let slot = self
            .regions
            .get(&f.region)
            .ok_or_else(|| SessionError::Protocol("FRAME names an unknown region".into()))?
            .clone();
        let (info, stale) = {
            let mut heads = self.shared.heads.lock().unwrap();
            let h = heads
                .get_mut(f.head as usize)
                .ok_or_else(|| SessionError::Protocol("FRAME for an unknown head".into()))?;
            let info = h
                .info
                .clone()
                .ok_or_else(|| SessionError::Protocol("FRAME before HEAD".into()))?;
            if f.lease as u32 >= self.welcome.credit.max(1) {
                return Err(SessionError::Protocol("lease index beyond credit".into()));
            }
            h.frame_seen = h.frame_seen.max(f.serial);
            h.want_frame = false;
            let stale = f.mode_serial != info.mode_serial;
            (info, stale)
        };
        if !stale && slot.len() < info.stride as usize * info.height as usize {
            return Err(SessionError::Protocol("slot smaller than its head".into()));
        }
        self.frames_received += 1;
        self.pending.push_back(Event::Frame(FrameLease {
            shared: self.shared.clone(),
            slot,
            width: info.width,
            height: info.height,
            stride: info.stride,
            fourcc: info.fourcc,
            stale_mode: stale,
            frame: f,
            live: true,
        }));
        Ok(())
    }

    pub fn is_ready(&self) -> bool {
        self.ready
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.shared.closed.store(true, Ordering::Release);
    }
}

fn map_region(r: &Region, fd: Option<OwnedFd>) -> Result<Mapping, SessionError> {
    let fd = fd.ok_or_else(|| SessionError::Protocol("REGION without a descriptor".into()))?;
    if r.size == 0 || !r.size.is_multiple_of(wire::REGION_ALIGN) {
        return Err(SessionError::Protocol(
            "region size is not a multiple of 64 KiB".into(),
        ));
    }
    if r.storage != wire::STORAGE_SHM {
        return Err(SessionError::Protocol(
            "storage class this viewer did not list".into(),
        ));
    }
    let writable = r.access & wire::ACCESS_WRITE != 0;
    let expect = if writable {
        Expect::Writable
    } else {
        Expect::ReadOnly
    };
    Mapping::map(fd, r.size, writable, expect)
        .map_err(|e| SessionError::Protocol(format!("region {}: {e}", r.id)))
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::io::Read;
    use std::os::fd::FromRawFd;

    fn memfd(size: u64) -> Arc<Mapping> {
        // SAFETY: a fresh memfd, sized and owned here.
        let fd = unsafe {
            let raw = libc::memfd_create(c"t".as_ptr(), libc::MFD_CLOEXEC);
            assert!(raw >= 0);
            assert_eq!(libc::ftruncate(raw, size as libc::off_t), 0);
            OwnedFd::from_raw_fd(raw)
        };
        Arc::new(Mapping::map(fd, size, true, Expect::Nothing).unwrap())
    }

    fn lease(want: bool) -> (FrameLease, UnixStream) {
        let (a, b) = UnixStream::pair().unwrap();
        b.set_read_timeout(Some(Duration::from_millis(200)))
            .unwrap();
        let fb = Arc::try_unwrap(memfd(65536)).ok().unwrap();
        let shared = Arc::new(Shared {
            sock: Mutex::new(a),
            feedback: fb,
            heads: Mutex::new(vec![HeadState {
                want_frame: want,
                frame_seen: 5,
                ..Default::default()
            }]),
            closed: Arc::new(AtomicBool::new(false)),
        });
        let frame = Frame {
            head: 0,
            region: 9,
            serial: 5,
            mode_serial: 1,
            sample_ns: 0,
            publish_ns: 0,
            guest_ns: 0,
            lease: 2,
            frame_flags: 0,
            lease_seq: 77,
            damage: wire::Damage::Whole,
        };
        let l = FrameLease {
            shared,
            frame,
            slot: memfd(65536),
            width: 16,
            height: 4,
            stride: 256,
            fourcc: wire::FOURCC_XR24,
            stale_mode: false,
            live: true,
        };
        (l, b)
    }

    fn released(l: &FrameLease) -> u64 {
        l.shared
            .feedback
            .load64(wire::feedback::HEADER + 2 * 8, Ordering::Acquire)
    }

    fn sent(mut peer: UnixStream) -> Vec<u8> {
        let mut v = vec![];
        let mut b = [0u8; 64];
        while let Ok(n) = peer.read(&mut b) {
            if n == 0 {
                break;
            }
            v.extend_from_slice(&b[..n]);
        }
        v
    }

    #[test]
    fn a_release_stores_the_lease_token_and_resends_an_outstanding_request() {
        let (l, peer) = lease(true);
        let shared = l.shared.clone();
        assert_eq!(released(&l), 0);
        l.release();
        assert_eq!(shared.feedback.load64(64 + 2 * 8, Ordering::Acquire), 77);
        drop(shared);
        let m = sent(peer);
        assert_eq!(m.len(), 32);
        assert_eq!(u16::from_le_bytes([m[0], m[1]]), wire::ty::REQUEST);
        assert_eq!(u64::from_le_bytes(m[16..24].try_into().unwrap()), 5);
        assert_eq!(
            u32::from_le_bytes(m[28..32].try_into().unwrap()),
            0,
            "a retry never carries SAMPLE_NOW"
        );
    }

    #[test]
    fn a_quiet_release_stores_but_sends_nothing() {
        let (l, peer) = lease(true);
        let shared = l.shared.clone();
        l.release_quiet();
        assert_eq!(shared.feedback.load64(64 + 2 * 8, Ordering::Acquire), 77);
        drop(shared);
        assert!(sent(peer).is_empty());
    }

    #[test]
    fn dropping_a_lease_releases_it_once() {
        let (l, peer) = lease(false);
        let shared = l.shared.clone();
        drop(l);
        assert_eq!(shared.feedback.load64(64 + 2 * 8, Ordering::Acquire), 77);
        drop(shared);
        assert!(
            sent(peer).is_empty(),
            "no request outstanding, none re-sent"
        );
    }

    #[test]
    fn a_failed_resend_ends_the_session() {
        let (l, peer) = lease(true);
        let shared = l.shared.clone();
        drop(peer);
        l.release();
        assert!(
            shared.closed.load(Ordering::Acquire),
            "a lost request must not leave the viewer waiting"
        );
    }
}
