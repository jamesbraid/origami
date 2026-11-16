//! The producer's state: heads, viewers, queues. Methods that decide what to
//! send live in `flow` (frames and sets), `input` and `control`.

use crate::lines::{Lines, Marks, Waker};
use crate::storage::{self, Shm};
use origami_display::v4::testext::{self, Stats, Stimulus};
use origami_display::v4::transport::{self, Fault, Reader};
use origami_display::v4::wire::{self, ErrorMsg, Header, Hello, ToProducer};
use std::collections::{BTreeSet, HashSet, VecDeque};
use std::io;
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::sync::atomic::Ordering;
use std::sync::Arc;

pub fn now_ns() -> u64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: valid timespec out-pointer.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
}

#[derive(Clone, Debug)]
pub struct HeadSpec {
    pub width: u32,
    pub height: u32,
    pub refresh_mhz: u32,
    pub label: String,
    pub pointer_mode: u32,
    /// The producer pulls lines from the host at sample time. False: the host
    /// pushes whole pictures when asked.
    pub pull: bool,
    pub source_index: u32,
    pub unsupported: bool,
    /// The core pixel format of the head's slots: `XR24` or `XB24`.
    pub fourcc: u32,
}

impl Default for HeadSpec {
    fn default() -> Self {
        HeadSpec {
            width: 256,
            height: 128,
            refresh_mhz: 60_000,
            label: "head".into(),
            pointer_mode: wire::POINTER_ABSOLUTE,
            pull: true,
            source_index: 0,
            unsupported: false,
            fourcc: wire::FOURCC_XR24,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputEvent {
    Key { head: u32, code: u32, down: u32 },
    Button { head: u32, button: u32, down: u32 },
    Rel { head: u32, dx: i32, dy: i32 },
    Abs { head: u32, x: i32, y: i32 },
    ReportEnd,
}

/// The emulator behind the producer.
pub trait Host: Send {
    /// A head exists; raise `marks` when its picture changes.
    fn attach(&mut self, _head: u32, _marks: Arc<Marks>) {}
    /// Complete the source's pending work before lines are taken.
    fn acquire(&mut self, _head: u32) {}
    /// Pull style: produce line `y` of the head as `XR24` words.
    fn resolve_line(&mut self, _head: u32, _y: u32, _row: &mut [u32]) {}
    /// Push style: compose and hand the picture to `Engine::push_begin` and
    /// `push_complete`.
    fn request_frame(&mut self, _head: u32) {}
    fn stimulus(&mut self, _s: &Stimulus) {}
    /// Make the next stimulus mark from another thread, pausing `micros`
    /// between the bit and the flag.
    fn delay_next_mark(&mut self, _head: u32, _micros: u32) {}
    fn inject(&mut self, _ev: &InputEvent) {}
}

pub struct Config {
    pub name: String,
    pub max_viewers: u32,
    pub max_head_bytes: u64,
    pub max_sets_per_head: usize,
    pub max_retained_bytes: u64,
    pub max_queue_bytes: usize,
    pub head_capacity: u32,
    pub test: bool,
    pub allow_uids: Vec<u32>,
    pub test_head: HeadSpec,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            name: "origami-display-producer".into(),
            max_viewers: 4,
            max_head_bytes: 256 << 20,
            max_sets_per_head: 3,
            max_retained_bytes: 1 << 30,
            max_queue_bytes: 4 << 20,
            head_capacity: 16,
            test: false,
            allow_uids: vec![],
            test_head: HeadSpec::default(),
        }
    }
}

pub(crate) struct Slot {
    pub region: u32,
    pub shm: Shm,
    pub stale: Lines,
    pub lent: u32,
    pub retired: bool,
}

pub(crate) struct Set {
    pub id: u32,
    pub writable: bool,
    /// Only this viewer joins it (a re-created set after device loss).
    pub exclusive: Option<u32>,
    pub slots: Vec<Slot>,
    pub viewers: Vec<u32>,
    pub target: usize,
    pub dead: bool,
}

#[derive(Default)]
pub(crate) struct Cursor {
    pub seq: u32,
    pub flags: u32,
    pub x: i32,
    pub y: i32,
    pub image_serial: u32,
    pub image: Option<wire::CursorImage>,
}

pub(crate) struct HeadData {
    pub present: bool,
    pub spec: HeadSpec,
    pub w: u32,
    pub h: u32,
    pub stride: u32,
    pub reference: Vec<u8>,
    pub n: u64,
    pub sample_ns: u64,
    pub publish_ns: u64,
    pub guest_ns: u64,
    pub marks: Arc<Marks>,
    /// Bumped on every mode change.
    pub mode_gen: u32,
    /// The current mode's first sample has completed.
    pub mode_ready: bool,
    pub sample_ctr: u64,
    pub done_ctr: u64,
    /// The newest sample started: its id and start time.
    pub latest: Option<(u64, u64)>,
    pub hold_next: bool,
    pub held: Option<(u64, u64, u32)>,
    pub held_pic: Option<Vec<u8>>,
    /// Viewer whose `HOLD_SAMPLE` is in force; its leaving ends the hold.
    pub hold_owner: Option<u32>,
    /// Push style: an id reserved for the host's next composition, and the one open.
    pub reserved: Option<u64>,
    pub push_open: Option<(u64, u64)>,
    pub sets: Vec<Set>,
    pub cursor: Cursor,
    pub skipped: u32,
}

pub(crate) struct Lease {
    pub seq: u64,
    pub region: u32,
    pub set: u32,
}

pub(crate) struct Queued {
    pub msg_id: u64,
    pub lease: usize,
    pub prev_last_lent: u64,
    pub damage: Lines,
    pub damage_all: bool,
    pub req: Rq,
}

#[derive(Clone, Copy)]
pub(crate) struct Rq {
    pub frame_seen: u64,
    pub bound: Option<u64>,
    pub sample_now_pending: bool,
}

pub(crate) struct VHead {
    pub set: Option<u32>,
    pub mode_serial: u32,
    pub announced_mode: (u32, u32),
    pub leases: [Option<Lease>; 16],
    pub next_lease_seq: u64,
    pub last_lent: u64,
    pub request: Option<Rq>,
    pub cursor_req: Option<u32>,
    pub damage: Lines,
    pub damage_all: bool,
    pub reset_pending: bool,
    pub queued: Option<Queued>,
    pub too_large: bool,
    pub failed: Vec<u32>,
    pub credit: u32,
}

impl VHead {
    pub fn new() -> VHead {
        VHead {
            set: None,
            mode_serial: 0,
            announced_mode: (0, 0),
            leases: Default::default(),
            next_lease_seq: 1,
            last_lent: 0,
            request: None,
            cursor_req: None,
            damage: Lines::default(),
            damage_all: true,
            reset_pending: false,
            queued: None,
            too_large: false,
            failed: vec![],
            credit: 1,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Tag {
    Plain,
    Region(u32),
    Head(u32),
    Frame { head: u32, set: u32, region: u32 },
    CursorImage(u32),
}

pub(crate) struct OutMsg {
    pub id: u64,
    pub bytes: Vec<u8>,
    pub fd: Option<Arc<OwnedFd>>,
    pub sent: usize,
    pub tag: Tag,
}

pub(crate) struct Viewer {
    pub conn: u64,
    pub id: u32,
    pub sock: UnixStream,
    pub reader: Reader,
    pub live: bool,
    pub hello: Option<Hello>,
    pub features: u32,
    pub credit: u32,
    pub storage: Vec<u32>,
    pub out: VecDeque<OutMsg>,
    pub out_bytes: usize,
    pub handles_started: u64,
    pub handles_acked: u64,
    pub feedback: Option<Shm>,
    pub heads: Vec<VHead>,
    pub announced: HashSet<u32>,
    pub test_index: Option<u8>,
    pub hold_writes: bool,
    pub max_queued: u32,
    pub dev_lost_used: bool,
    pub held_keys: BTreeSet<(u32, u32)>,
    pub held_buttons: BTreeSet<(u32, u32)>,
    pub pending_rel: Option<(u32, i64, i64)>,
    pub report_events: u32,
    pub closing: Option<ErrorMsg>,
    pub gone: bool,
}

impl Viewer {
    pub(crate) fn serial(&self) -> u64 {
        self.conn
    }
}

pub struct Engine {
    pub(crate) cfg: Config,
    pub(crate) session_id: u64,
    pub(crate) next_region: u32,
    pub(crate) next_viewer: u32,
    pub(crate) next_set: u32,
    pub(crate) next_msg: u64,
    pub(crate) state: Shm,
    pub(crate) state_region: u32,
    pub(crate) heads: Vec<HeadData>,
    pub(crate) viewers: Vec<Viewer>,
    pub(crate) host: Box<dyn Host>,
    pub(crate) waker: Arc<Waker>,
    pub(crate) stats: Stats,
    pub(crate) paused: bool,
    pub(crate) owner: Option<u32>,
    pub(crate) echo_seq: u32,
    pub(crate) next_conn: u64,
}

fn random_u64() -> u64 {
    let mut b = [0u8; 8];
    // SAFETY: getrandom fills our buffer.
    let r = unsafe { libc::getrandom(b.as_mut_ptr().cast(), 8, 0) };
    if r == 8 {
        u64::from_le_bytes(b) | 1
    } else {
        now_ns() | 1
    }
}

pub(crate) fn align_up(v: u64, a: u64) -> u64 {
    v.div_ceil(a) * a
}

impl Engine {
    pub fn new(cfg: Config, host: Box<dyn Host>, waker: Arc<Waker>) -> io::Result<Engine> {
        let cap = cfg.head_capacity.min(wire::FEEDBACK_HEADS as u32);
        let size = align_up(
            (wire::state::HEADER + cap as usize * wire::state::HEAD_STRIDE) as u64,
            65_536,
        );
        let state = storage::create(size, false)?;
        let session_id = random_u64();
        let m = &state.map;
        m.store32(16, wire::state::HEADER as u32, Ordering::Relaxed);
        m.store32(20, wire::state::HEAD_STRIDE as u32, Ordering::Relaxed);
        m.store32(24, cap, Ordering::Relaxed);
        m.store64(8, session_id, Ordering::Relaxed);
        m.store32(
            4,
            wire::MAJOR as u32 | (wire::MINOR as u32) << 16,
            Ordering::Relaxed,
        );
        m.store32(0, wire::STATE_MAGIC, Ordering::Release);
        Ok(Engine {
            cfg,
            session_id,
            next_region: 1,
            next_viewer: 1,
            next_set: 1,
            next_msg: 1,
            state,
            state_region: 0,
            heads: vec![],
            viewers: vec![],
            host,
            waker,
            stats: Stats::default(),
            paused: false,
            owner: None,
            echo_seq: 0,
            next_conn: 1,
        })
        .map(|mut e| {
            e.state_region = e.alloc_region();
            e
        })
    }

    pub(crate) fn alloc_region(&mut self) -> u32 {
        let r = self.next_region;
        self.next_region += 1;
        r
    }

    pub fn head_count(&self) -> usize {
        self.heads.len()
    }

    /// The head's current mode, (0, 0) while it has none.
    pub fn head_size(&self, head: u32) -> (u32, u32) {
        self.heads.get(head as usize).map_or((0, 0), |h| (h.w, h.h))
    }

    // --- heads ---

    pub fn add_head(&mut self, spec: HeadSpec) -> io::Result<u32> {
        if self.heads.len() as u32 >= self.cfg.head_capacity.min(wire::FEEDBACK_HEADS as u32) {
            return Err(io::Error::other("head capacity reached"));
        }
        let n = self.heads.len() as u32;
        let marks = Marks::new(self.waker.clone());
        let (w, h) = (spec.width, spec.height);
        let stride = align_up(w as u64 * 4, wire::STRIDE_ALIGN as u64) as u32;
        let mut hd = HeadData {
            present: true,
            w,
            h,
            stride,
            reference: vec![0; stride as usize * h as usize],
            n: 0,
            sample_ns: 0,
            publish_ns: 0,
            guest_ns: 0,
            marks: marks.clone(),
            mode_gen: 1,
            mode_ready: false,
            sample_ctr: 0,
            done_ctr: 0,
            latest: None,
            hold_next: false,
            held: None,
            held_pic: None,
            hold_owner: None,
            reserved: None,
            push_open: None,
            sets: vec![],
            cursor: Cursor::default(),
            skipped: 0,
            spec,
        };
        if hd.spec.unsupported {
            hd.w = 0;
            hd.h = 0;
        }
        self.heads.push(hd);
        self.host.attach(n, marks.clone());
        marks.mark_all();
        for vi in 0..self.viewers.len() {
            if self.viewers[vi].live {
                self.viewers[vi].heads.push(VHead::new());
                self.join_head(vi, n);
            }
        }
        Ok(n)
    }

    pub fn remove_head(&mut self, head: u32) {
        let Some(hd) = self.heads.get_mut(head as usize).filter(|h| h.present) else {
            return;
        };
        hd.present = false;
        hd.w = 0;
        hd.h = 0;
        for vi in 0..self.viewers.len() {
            if !self.viewers[vi].live {
                continue;
            }
            self.viewers[vi].heads[head as usize].request = None;
            self.leave_set(vi, head);
            self.send_head(vi, head);
        }
        self.gc_sets(head);
    }

    // --- output ---

    pub(crate) fn queue(
        &mut self,
        vi: usize,
        bytes: Vec<u8>,
        fd: Option<Arc<OwnedFd>>,
        tag: Tag,
    ) -> u64 {
        let id = self.next_msg;
        self.next_msg += 1;
        let limit = self.cfg.max_queue_bytes;
        let v = &mut self.viewers[vi];
        v.out_bytes += bytes.len();
        v.out.push_back(OutMsg {
            id,
            bytes,
            fd,
            sent: 0,
            tag,
        });
        if v.out_bytes > limit && v.closing.is_none() {
            v.closing = Some(ErrorMsg::new(wire::err::LIMIT, "output queue limit"));
        }
        id
    }

    pub(crate) fn plain(&mut self, vi: usize, bytes: Vec<u8>) {
        self.queue(vi, bytes, None, Tag::Plain);
    }

    pub(crate) fn wholly_queued(&self, vi: usize, msg_id: u64) -> bool {
        self.viewers[vi]
            .out
            .iter()
            .any(|m| m.id == msg_id && m.sent == 0)
    }

    pub(crate) fn fail(&mut self, vi: usize, code: u32, text: &str) {
        let v = &mut self.viewers[vi];
        if v.closing.is_none() {
            v.closing = Some(ErrorMsg::new(code, text));
        }
    }

    /// Write queued messages while the socket takes them.
    pub(crate) fn flush(&mut self, vi: usize) {
        loop {
            let v = &mut self.viewers[vi];
            if v.closing.is_some() {
                // The error goes out regardless of any write hold; the rest is dropped.
                let e = v.closing.take().unwrap();
                // Best effort: the connection is closing whether or not the peer reads it.
                let _ = transport::send(v.sock.as_raw_fd(), &e.encode(), None);
                v.gone = true;
                v.out.clear();
                return;
            }
            if v.hold_writes || v.gone {
                return;
            }
            let Some(front) = v.out.front_mut() else {
                return;
            };
            let mut started = None;
            if front.sent == 0 {
                if front.fd.is_some() && v.handles_started - v.handles_acked >= wire::HANDLE_WINDOW
                {
                    return;
                }
                started = Some((front.id, front.tag.clone()));
            }
            let fd = if front.sent == 0 {
                front.fd.as_ref().map(|f| f.as_raw_fd())
            } else {
                None
            };
            match transport::send(v.sock.as_raw_fd(), &front.bytes[front.sent..], fd) {
                Ok(n) => {
                    if front.sent == 0 && front.fd.is_some() {
                        v.handles_started += 1;
                    }
                    front.sent += n;
                    if front.sent == front.bytes.len() {
                        v.out_bytes -= front.bytes.len();
                        v.out.pop_front();
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => {
                    v.gone = true;
                    return;
                }
            }
            if let Some((id, tag)) = started {
                self.on_msg_started(vi, id, &tag);
            }
        }
    }

    fn on_msg_started(&mut self, vi: usize, id: u64, tag: &Tag) {
        if let Tag::Frame { head, set, .. } = tag {
            let vh = &mut self.viewers[vi].heads[*head as usize];
            if vh.queued.as_ref().is_some_and(|q| q.msg_id == id) {
                vh.queued = None;
            }
            if vh.set == Some(*set) {
                vh.reset_pending = false;
            }
            self.stats.frames_sent += 1;
        }
    }

    // --- connections ---

    /// A new connection, before its `HELLO`. Refuses other users.
    pub fn accept(&mut self, sock: UnixStream) {
        let nonblocking = sock.set_nonblocking(true);
        self.next_conn += 1;
        let mut v = Viewer {
            conn: self.next_conn,
            id: 0,
            sock,
            reader: Reader::new(),
            live: false,
            hello: None,
            features: 0,
            credit: 1,
            storage: vec![],
            out: VecDeque::new(),
            out_bytes: 0,
            handles_started: 0,
            handles_acked: 0,
            feedback: None,
            heads: vec![],
            announced: HashSet::new(),
            test_index: None,
            hold_writes: false,
            max_queued: 0,
            dev_lost_used: false,
            held_keys: BTreeSet::new(),
            held_buttons: BTreeSet::new(),
            pending_rel: None,
            report_events: 0,
            closing: None,
            gone: false,
        };
        if !peer_allowed(&v.sock, &self.cfg.allow_uids) {
            v.closing = Some(ErrorMsg::new(
                wire::err::PERMISSION,
                "peer user not allowed",
            ));
        }
        if nonblocking.is_err() {
            // A blocking socket would stall the whole producer thread.
            v.gone = true;
        }
        self.viewers.push(v);
        let vi = self.viewers.len() - 1;
        if self.viewers[vi].closing.is_some() {
            self.flush(vi);
        }
    }

    /// Read everything available from viewer `vi` and act on it.
    pub fn on_readable(&mut self, vi: usize) {
        loop {
            if self.viewers[vi].closing.is_some() || self.viewers[vi].gone {
                return;
            }
            let v = &mut self.viewers[vi];
            let r = v.reader.fill(v.sock.as_raw_fd());
            match r {
                Ok(0) => {
                    self.viewers[vi].gone = true;
                    return;
                }
                Ok(_) => {}
                Err(Fault::Io(e)) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(Fault::Io(e)) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(Fault::Io(_)) => {
                    self.viewers[vi].gone = true;
                    return;
                }
                Err(Fault::Protocol(s)) => {
                    self.fail(vi, wire::err::PROTOCOL, s);
                    return;
                }
            }
            if !self.drain_messages(vi) {
                return;
            }
        }
        self.drain_messages(vi);
    }

    fn drain_messages(&mut self, vi: usize) -> bool {
        loop {
            if self.viewers[vi].closing.is_some() {
                return false;
            }
            let got = self.viewers[vi]
                .reader
                .next_with(|h, m, fd| (h, ToProducer::decode(h, m), fd));
            match got {
                Ok(None) => return true,
                Ok(Some((h, msg, fd))) => match msg {
                    Ok(m) => self.process(vi, h, m, fd.is_some()),
                    Err(e) => self.fail(vi, wire::err::PROTOCOL, e.0),
                },
                Err(Fault::Protocol(s)) => {
                    self.fail(vi, wire::err::PROTOCOL, s);
                    return false;
                }
                Err(Fault::Io(_)) => {
                    self.viewers[vi].gone = true;
                    return false;
                }
            }
        }
    }

    fn process(&mut self, vi: usize, h: Header, msg: ToProducer, had_fd: bool) {
        use ToProducer as M;
        if !self.viewers[vi].live {
            match msg {
                M::Hello(hello) => {
                    if had_fd {
                        return self.fail(vi, wire::err::PROTOCOL, "HELLO carries a handle");
                    }
                    return self.on_hello(vi, hello);
                }
                M::Error(_) => {
                    self.viewers[vi].gone = true;
                    return;
                }
                M::Other { .. } if !h.must() => return,
                _ => return self.fail(vi, wire::err::PROTOCOL, "HELLO must come first"),
            }
        }
        if had_fd && !matches!(msg, M::Other { .. }) {
            return self.fail(vi, wire::err::PROTOCOL, "viewer message with a handle");
        }
        match msg {
            M::Hello(_) => self.fail(vi, wire::err::PROTOCOL, "second HELLO"),
            M::Request(r) => self.on_request(vi, r),
            M::ImportFailed { region, reason } => self.on_import_failed(vi, region, reason),
            M::HandlesSeen(n) => {
                let v = &mut self.viewers[vi];
                if n < v.handles_acked || n > v.handles_started {
                    self.fail(vi, wire::err::PROTOCOL, "bad HANDLES_SEEN");
                } else {
                    v.handles_acked = n;
                }
            }
            M::SetStorage { head, storage } => self.on_set_storage(vi, head, storage),
            M::Error(_) => self.viewers[vi].gone = true,
            M::Other { ty, flags, msg } => {
                let idx = (ty >> 8) as u8;
                if ty >= 0x0100 && self.viewers[vi].test_index == Some(idx) {
                    self.on_test_message(vi, (ty & 0xFF) as u8, &msg);
                } else if flags & wire::F_MUST != 0 {
                    self.fail(vi, wire::err::UNSUPPORTED, "unsupported message type");
                }
            }
            other => self.on_input(vi, other),
        }
    }

    fn on_hello(&mut self, vi: usize, hello: Hello) {
        use wire::err;
        if hello.major != wire::MAJOR {
            let mut e = ErrorMsg::new(err::VERSION, "unsupported major version");
            e.major_min = wire::MAJOR;
            e.major_max = wire::MAJOR;
            self.viewers[vi].closing = Some(e);
            return;
        }
        if hello.credit == 0 || hello.credit > wire::MAX_CREDIT {
            return self.fail(vi, err::PROTOCOL, "credit outside 1 to 4");
        }
        let list: Vec<u32> = hello
            .storage
            .iter()
            .copied()
            .take_while(|&s| s != 0)
            .collect();
        if !list.contains(&wire::STORAGE_SHM) {
            return self.fail(vi, err::PROTOCOL, "storage list lacks shm");
        }
        if self.viewers.iter().filter(|v| v.live).count() as u32 >= self.cfg.max_viewers {
            return self.fail(vi, err::LIMIT, "too many viewers");
        }
        let mut features = hello.features
            & (wire::feat::INPUT
                | wire::feat::RW_PIXELS
                | wire::feat::LEDS
                | wire::feat::CURSOR_OVERLAY);
        let mut accepts = vec![];
        let offer = hello.offers.iter().find(|o| o.name == testext::NAME);
        if let Some(o) = offer {
            if self.cfg.test
                && hello.features & wire::feat::TEST != 0
                && o.version_min <= testext::VERSION
                && testext::VERSION <= o.version_max
            {
                features |= wire::feat::TEST;
                accepts.push(wire::Accept {
                    name: testext::NAME.into(),
                    version: testext::VERSION,
                    index: 1,
                    payload: vec![],
                });
                self.viewers[vi].test_index = Some(1);
            }
        }
        let id = self.next_viewer;
        self.next_viewer += 1;
        let feedback_region = self.alloc_region();
        let fb = match storage::create(wire::FEEDBACK_SIZE as u64, true) {
            Ok(s) => s,
            Err(_) => return self.fail(vi, err::INTERNAL, "cannot allocate the feedback region"),
        };
        {
            let m = &fb.map;
            m.store32(4, wire::MAJOR as u32, Ordering::Relaxed);
            m.store32(8, wire::feedback::HEADER as u32, Ordering::Relaxed);
            m.store32(12, wire::feedback::HEAD_STRIDE as u32, Ordering::Relaxed);
            m.store32(16, wire::FEEDBACK_HEADS as u32, Ordering::Relaxed);
            m.store32(0, wire::FEEDBACK_MAGIC, Ordering::Release);
        }
        let granted = hello.credit;
        {
            let v = &mut self.viewers[vi];
            v.id = id;
            v.live = true;
            v.features = features;
            v.credit = granted;
            v.storage = list;
            v.feedback = Some(fb);
            v.heads = (0..self.heads.len()).map(|_| VHead::new()).collect();
            v.hello = Some(hello);
        }
        let welcome = wire::Welcome {
            major: wire::MAJOR,
            minor: wire::MINOR,
            features,
            session_id: self.session_id,
            viewer_id: id,
            state_region: self.state_region,
            feedback_region,
            credit: granted,
            max_viewers: self.cfg.max_viewers,
            name: self.cfg.name.clone(),
            accepts,
        };
        self.plain(vi, welcome.encode());
        self.announce_shm(
            vi,
            self.state_region,
            wire::KIND_STATE,
            wire::ACCESS_READ,
            None,
        );
        self.announce_fb(vi, feedback_region);
        for h in 0..self.heads.len() as u32 {
            self.join_head(vi, h);
            if let Some(c) = self.heads[h as usize].cursor.image.clone() {
                self.queue(vi, c.encode(), None, Tag::CursorImage(h));
            }
        }
        if features & wire::feat::INPUT != 0 && self.owner.is_none() {
            self.owner = Some(id);
        }
        self.plain(vi, wire::input_state((self.owner == Some(id)) as u32, 0));
        self.plain(vi, wire::ready());
    }

    fn announce_shm(&mut self, vi: usize, id: u32, kind: u32, access: u32, shm: Option<&Shm>) {
        let (fd, size) = match shm {
            Some(s) => (s.fd.clone(), s.size),
            None => (self.state.fd.clone(), self.state.size),
        };
        let r = wire::Region {
            id,
            kind,
            storage: wire::STORAGE_SHM,
            access,
            size,
            payload: vec![],
        };
        self.viewers[vi].announced.insert(id);
        self.queue(vi, r.encode(), Some(fd), Tag::Region(id));
    }

    fn announce_fb(&mut self, vi: usize, id: u32) {
        let fb = self.viewers[vi].feedback.as_ref().unwrap();
        let (fd, size) = (fb.fd.clone(), fb.size);
        let r = wire::Region {
            id,
            kind: wire::KIND_FEEDBACK,
            storage: wire::STORAGE_SHM,
            access: wire::ACCESS_READ | wire::ACCESS_WRITE,
            size,
            payload: vec![],
        };
        self.viewers[vi].announced.insert(id);
        self.queue(vi, r.encode(), Some(fd), Tag::Region(id));
    }

    /// Tell `vi` about a slot region, once.
    pub(crate) fn announce_slot(&mut self, vi: usize, set: &Set, slot_ix: usize) {
        let s = &set.slots[slot_ix];
        if self.viewers[vi].announced.contains(&s.region) {
            return;
        }
        let access = wire::ACCESS_READ | if set.writable { wire::ACCESS_WRITE } else { 0 };
        let r = wire::Region {
            id: s.region,
            kind: wire::KIND_SLOT,
            storage: wire::STORAGE_SHM,
            access,
            size: s.shm.size,
            payload: vec![],
        };
        self.viewers[vi].announced.insert(s.region);
        self.queue(
            vi,
            r.encode(),
            Some(s.shm.fd.clone()),
            Tag::Region(s.region),
        );
    }

    /// Remove viewer `vi` (already marked gone): release its input, retire
    /// the slots it held, free its sets.
    pub(crate) fn drop_viewer(&mut self, vi: usize) {
        let id = self.viewers[vi].id;
        if self.viewers[vi].live {
            self.release_input(vi);
            if self.owner == Some(id) {
                self.owner = None;
            }
            for h in 0..self.heads.len() as u32 {
                self.depart_head(vi, h);
            }
            for hd in self.heads.iter_mut().filter(|h| h.hold_owner == Some(id)) {
                hd.hold_owner = None;
                hd.hold_next = false;
                hd.held = None;
                hd.held_pic = None;
            }
        }
        self.viewers.remove(vi);
    }

    /// Stats for `STATS`.
    pub(crate) fn stats_snapshot(&self, vi: usize) -> Stats {
        let mut s = self.stats;
        s.sets = self
            .heads
            .iter()
            .map(|h| h.sets.iter().filter(|x| !x.dead).count() as u32)
            .sum();
        s.max_queued = self.viewers[vi].max_queued;
        s.retained_bytes = self
            .heads
            .iter()
            .map(|h| {
                h.reference.len() as u64
                    + h.sets
                        .iter()
                        .map(|x| x.slots.iter().map(|s| s.shm.size).sum::<u64>())
                        .sum::<u64>()
            })
            .sum();
        s
    }

    pub(crate) fn viewer_index(&self, id: u32) -> Option<usize> {
        self.viewers.iter().position(|v| v.live && v.id == id)
    }
}

fn peer_allowed(sock: &UnixStream, allow: &[u32]) -> bool {
    let mut cred = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: SO_PEERCRED fills a ucred of the stated size.
    let r = unsafe {
        libc::getsockopt(
            sock.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut cred as *mut libc::ucred).cast(),
            &mut len,
        )
    };
    // SAFETY: geteuid has no preconditions.
    r == 0 && (cred.uid == unsafe { libc::geteuid() } || allow.contains(&cred.uid))
}
