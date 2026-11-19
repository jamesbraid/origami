//! Display protocol version 4: every message and region layout, as bytes.
//! Offsets include the 8-byte header, as in the design's tables.

use std::fmt;

pub const MAJOR: u16 = 4;
pub const MINOR: u16 = 0;
pub const MSG_MAX: usize = 131_072;
pub const REGION_ALIGN: u64 = 65_536;
pub const STRIDE_ALIGN: u32 = 256;
pub const STATE_MAGIC: u32 = 0x5053_4451;
pub const FEEDBACK_MAGIC: u32 = 0x4246_4451;
pub const FEEDBACK_SIZE: usize = 65_536;
pub const FEEDBACK_HEADS: usize = 511;
pub const MAX_SLOTS: usize = 16;
pub const MAX_LEASES: usize = 16;
pub const MAX_CREDIT: u32 = 4;
pub const MAX_RECTS: usize = 64;
pub const HANDLE_WINDOW: u64 = 32;

pub const F_MUST: u16 = 1;
pub const F_HANDLE: u16 = 2;

pub mod ty {
    pub const HELLO: u16 = 0x0001;
    pub const REQUEST: u16 = 0x0002;
    pub const IMPORT_FAILED: u16 = 0x0003;
    pub const HANDLES_SEEN: u16 = 0x0004;
    pub const SET_STORAGE: u16 = 0x0005;
    pub const INPUT_CLAIM: u16 = 0x0010;
    pub const KEY: u16 = 0x0011;
    pub const BUTTON: u16 = 0x0012;
    pub const REL: u16 = 0x0013;
    pub const ABS: u16 = 0x0014;
    pub const SYNC: u16 = 0x0015;
    pub const INPUT_RESET: u16 = 0x0016;
    pub const WELCOME: u16 = 0x0081;
    pub const REGION: u16 = 0x0082;
    pub const REGION_GONE: u16 = 0x0083;
    pub const READY: u16 = 0x0084;
    pub const HEAD: u16 = 0x0085;
    pub const FRAME: u16 = 0x0086;
    pub const CURSOR_IMAGE: u16 = 0x0087;
    pub const CURSOR_NOTIFY: u16 = 0x0088;
    pub const INPUT_ACK: u16 = 0x0089;
    pub const INPUT_STATE: u16 = 0x008A;
    pub const ORPHANS: u16 = 0x008B;
    pub const ERROR: u16 = 0x00F0;
}

pub mod err {
    pub const VERSION: u32 = 1;
    pub const PROTOCOL: u32 = 2;
    pub const UNSUPPORTED: u32 = 3;
    pub const PERMISSION: u32 = 4;
    pub const LIMIT: u32 = 5;
    pub const SHUTDOWN: u32 = 7;
    pub const INTERNAL: u32 = 8;
}

pub mod feat {
    pub const INPUT: u32 = 1;
    pub const RW_PIXELS: u32 = 2;
    pub const LEDS: u32 = 4;
    pub const CURSOR_OVERLAY: u32 = 8;
    pub const TEST: u32 = 16;
}

/// The extension that publishes where the guest drew its cursor.
pub const STEERING_EXT: &str = "qemu.pointer-steering";
pub const STEER_BLOCK_BYTES: usize = 32;
pub const STORAGE_SHM: u32 = 1;
pub const POINTER_ABSOLUTE: u32 = 1;
pub const POINTER_RELATIVE: u32 = 2;
pub const POINTER_STEERED: u32 = 3;
pub const KIND_STATE: u32 = 1;
pub const KIND_FEEDBACK: u32 = 2;
pub const KIND_SLOT: u32 = 3;
pub const ACCESS_READ: u32 = 1;
pub const ACCESS_WRITE: u32 = 2;
pub const HEAD_PRESENT: u32 = 1;
pub const HEAD_TOO_LARGE: u32 = 2;
pub const HEAD_UNSUPPORTED: u32 = 4;
pub const HEAD_CURSOR_OUT: u32 = 8;
pub const FOURCC_XR24: u32 = 0x3432_5258;
pub const FOURCC_XB24: u32 = 0x3432_4258;
pub const FOURCC_AR24: u32 = 0x3432_5241;
pub const REQ_FRAME: u32 = 1;
pub const REQ_CURSOR: u32 = 2;
pub const REQ_SAMPLE_NOW: u32 = 1;
pub const DAMAGE_WHOLE: u16 = 0;
pub const DAMAGE_RECTS: u16 = 1;
pub const DAMAGE_LINES: u16 = 2;
pub const FRAME_CURSOR_EMBEDDED: u16 = 1;

/// A malformed message: the receiver answers `ERROR PROTOCOL`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WireError(pub &'static str);

impl fmt::Display for WireError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for WireError {}

type R<T> = Result<T, WireError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub ty: u16,
    pub flags: u16,
    pub len: u32,
}

impl Header {
    pub fn parse(b: &[u8]) -> R<Header> {
        let h = Header {
            ty: u16::from_le_bytes([b[0], b[1]]),
            flags: u16::from_le_bytes([b[2], b[3]]),
            len: u32::from_le_bytes([b[4], b[5], b[6], b[7]]),
        };
        if h.len < 8 || h.len as usize > MSG_MAX || !h.len.is_multiple_of(8) {
            return Err(WireError("bad message length"));
        }
        Ok(h)
    }
    pub fn must(&self) -> bool {
        self.flags & F_MUST != 0
    }
    pub fn handle(&self) -> bool {
        self.flags & F_HANDLE != 0
    }
}

/// Appends little-endian fields to a message under construction.
pub struct Out(pub Vec<u8>);

impl Out {
    pub fn new(ty: u16, flags: u16) -> Out {
        let mut v = Vec::with_capacity(64);
        v.extend_from_slice(&ty.to_le_bytes());
        v.extend_from_slice(&flags.to_le_bytes());
        v.extend_from_slice(&[0; 4]);
        Out(v)
    }
    pub fn u16(&mut self, v: u16) -> &mut Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    pub fn u32(&mut self, v: u32) -> &mut Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    pub fn i32(&mut self, v: i32) -> &mut Self {
        self.u32(v as u32)
    }
    pub fn u64(&mut self, v: u64) -> &mut Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    pub fn bytes(&mut self, b: &[u8]) -> &mut Self {
        self.0.extend_from_slice(b);
        self
    }
    /// A NUL-padded fixed field.
    pub fn name(&mut self, s: &str, n: usize) -> &mut Self {
        let b = s.as_bytes();
        let k = b.len().min(n);
        self.0.extend_from_slice(&b[..k]);
        self.0.resize(self.0.len() + n - k, 0);
        self
    }
    pub fn pad(&mut self) -> &mut Self {
        while !self.0.len().is_multiple_of(8) {
            self.0.push(0);
        }
        self
    }
    pub fn zeros(&mut self, n: usize) -> &mut Self {
        self.0.resize(self.0.len() + n, 0);
        self
    }
    pub fn finish(mut self) -> Vec<u8> {
        self.pad();
        let n = self.0.len() as u32;
        self.0[4..8].copy_from_slice(&n.to_le_bytes());
        self.0
    }
}

/// Reads fields from a whole message (header included).
pub struct Rd<'a> {
    b: &'a [u8],
    pos: usize,
}

impl<'a> Rd<'a> {
    pub fn new(b: &'a [u8]) -> Rd<'a> {
        Rd { b, pos: 8 }
    }
    pub fn at(b: &'a [u8], pos: usize) -> Rd<'a> {
        Rd { b, pos }
    }
    pub fn pos(&self) -> usize {
        self.pos
    }
    pub fn take(&mut self, n: usize) -> R<&'a [u8]> {
        let end = self.pos.checked_add(n).ok_or(WireError("overflow"))?;
        if end > self.b.len() {
            return Err(WireError("message shorter than its fields"));
        }
        let s = &self.b[self.pos..end];
        self.pos = end;
        Ok(s)
    }
    pub fn u16(&mut self) -> R<u16> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    pub fn u32(&mut self) -> R<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    pub fn i32(&mut self) -> R<i32> {
        Ok(self.u32()? as i32)
    }
    pub fn u64(&mut self) -> R<u64> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    pub fn skip(&mut self, n: usize) -> R<()> {
        self.take(n).map(|_| ())
    }
    pub fn name(&mut self, n: usize) -> R<String> {
        let b = self.take(n)?;
        let end = b.iter().position(|&c| c == 0).unwrap_or(n);
        Ok(String::from_utf8_lossy(&b[..end]).into_owned())
    }
    pub fn u32s<const N: usize>(&mut self) -> R<[u32; N]> {
        let mut a = [0; N];
        for v in &mut a {
            *v = self.u32()?;
        }
        Ok(a)
    }
}

fn pad8(n: usize) -> usize {
    (n + 7) & !7
}

/// An extension the viewer offers, or the producer accepts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Offer {
    pub name: String,
    pub version_min: u16,
    pub version_max: u16,
    pub payload: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Accept {
    pub name: String,
    pub version: u16,
    pub index: u8,
    pub payload: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hello {
    pub major: u16,
    pub minor: u16,
    pub features: u32,
    pub name: String,
    pub storage: [u32; 8],
    pub credit: u32,
    pub max_width: u32,
    pub max_height: u32,
    pub prev_viewer_id: u32,
    pub max_slot_bytes: u32,
    pub prev_handles_taken: u64,
    pub prev_session_id: u64,
    pub offers: Vec<Offer>,
}

impl Hello {
    pub fn encode(&self) -> Vec<u8> {
        let mut o = Out::new(ty::HELLO, F_MUST);
        o.u16(self.major).u16(self.minor).u32(self.features);
        o.name(&self.name, 32);
        for s in self.storage {
            o.u32(s);
        }
        o.u32(self.credit).u32(self.max_width).u32(self.max_height);
        o.u32(self.prev_viewer_id).u32(self.max_slot_bytes);
        o.u32(self.offers.len() as u32);
        o.u64(self.prev_handles_taken).u64(self.prev_session_id);
        for of in &self.offers {
            o.name(&of.name, 32).u16(of.version_min).u16(of.version_max);
            o.u32(of.payload.len() as u32).bytes(&of.payload).pad();
        }
        o.finish()
    }

    pub fn decode(m: &[u8]) -> R<Hello> {
        if m.len() < 120 {
            return Err(WireError("HELLO too short"));
        }
        let mut r = Rd::new(m);
        let major = r.u16()?;
        let minor = r.u16()?;
        let features = r.u32()?;
        let name = r.name(32)?;
        let storage = r.u32s::<8>()?;
        let credit = r.u32()?;
        let max_width = r.u32()?;
        let max_height = r.u32()?;
        let prev_viewer_id = r.u32()?;
        let max_slot_bytes = r.u32()?;
        let n_ext = r.u32()?;
        let prev_handles_taken = r.u64()?;
        let prev_session_id = r.u64()?;
        let mut offers = Vec::new();
        for _ in 0..n_ext {
            let name = r.name(32)?;
            let version_min = r.u16()?;
            let version_max = r.u16()?;
            let n = r.u32()? as usize;
            let payload = r.take(n)?.to_vec();
            r.skip(pad8(n) - n)?;
            offers.push(Offer {
                name,
                version_min,
                version_max,
                payload,
            });
        }
        Ok(Hello {
            major,
            minor,
            features,
            name,
            storage,
            credit,
            max_width,
            max_height,
            prev_viewer_id,
            max_slot_bytes,
            prev_handles_taken,
            prev_session_id,
            offers,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Request {
    pub head: u32,
    pub kinds: u32,
    pub frame_seen: u64,
    pub cursor_seen: u32,
    pub flags: u32,
}

impl Request {
    pub fn encode(&self) -> Vec<u8> {
        let mut o = Out::new(ty::REQUEST, F_MUST);
        o.u32(self.head).u32(self.kinds).u64(self.frame_seen);
        o.u32(self.cursor_seen).u32(self.flags);
        o.finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Welcome {
    pub major: u16,
    pub minor: u16,
    pub features: u32,
    pub session_id: u64,
    pub viewer_id: u32,
    pub state_region: u32,
    pub feedback_region: u32,
    pub credit: u32,
    pub max_viewers: u32,
    pub name: String,
    pub accepts: Vec<Accept>,
}

impl Welcome {
    pub fn encode(&self) -> Vec<u8> {
        let mut o = Out::new(ty::WELCOME, F_MUST);
        o.u16(self.major).u16(self.minor).u32(self.features);
        o.u64(self.session_id).u32(self.viewer_id);
        o.u32(self.state_region).u32(self.feedback_region);
        o.u32(self.credit).u32(self.max_viewers);
        o.u32(self.accepts.len() as u32);
        o.name(&self.name, 32);
        for a in &self.accepts {
            o.name(&a.name, 32).u16(a.version);
            o.0.push(a.index);
            o.0.push(0);
            o.u32(a.payload.len() as u32).bytes(&a.payload).pad();
        }
        o.finish()
    }

    pub fn decode(m: &[u8]) -> R<Welcome> {
        if m.len() < 80 {
            return Err(WireError("WELCOME too short"));
        }
        let mut r = Rd::new(m);
        let major = r.u16()?;
        let minor = r.u16()?;
        let features = r.u32()?;
        let session_id = r.u64()?;
        let viewer_id = r.u32()?;
        let state_region = r.u32()?;
        let feedback_region = r.u32()?;
        let credit = r.u32()?;
        let max_viewers = r.u32()?;
        let n_ext = r.u32()?;
        let name = r.name(32)?;
        let mut accepts = Vec::new();
        for _ in 0..n_ext {
            let name = r.name(32)?;
            let version = r.u16()?;
            let index = r.take(2)?[0];
            let n = r.u32()? as usize;
            let payload = r.take(n)?.to_vec();
            r.skip(pad8(n) - n)?;
            accepts.push(Accept {
                name,
                version,
                index,
                payload,
            });
        }
        Ok(Welcome {
            major,
            minor,
            features,
            session_id,
            viewer_id,
            state_region,
            feedback_region,
            credit,
            max_viewers,
            name,
            accepts,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Region {
    pub id: u32,
    pub kind: u32,
    pub storage: u32,
    pub access: u32,
    pub size: u64,
    pub payload: Vec<u8>,
}

impl Region {
    /// The trailing handle field is 0 on POSIX; the descriptor rides the socket.
    pub fn encode(&self) -> Vec<u8> {
        let mut o = Out::new(ty::REGION, F_MUST | F_HANDLE);
        o.u32(self.id)
            .u32(self.kind)
            .u32(self.storage)
            .u32(self.access);
        o.u64(self.size).u32(self.payload.len() as u32).u32(0);
        o.bytes(&self.payload).pad().u64(0);
        o.finish()
    }

    pub fn decode(m: &[u8]) -> R<Region> {
        if m.len() < 48 {
            return Err(WireError("REGION too short"));
        }
        let mut r = Rd::new(m);
        let id = r.u32()?;
        let kind = r.u32()?;
        let storage = r.u32()?;
        let access = r.u32()?;
        let size = r.u64()?;
        let n = r.u32()? as usize;
        r.skip(4)?;
        if 40 + pad8(n) + 8 > m.len() {
            return Err(WireError("REGION payload overruns the message"));
        }
        let payload = r.take(n)?.to_vec();
        Ok(Region {
            id,
            kind,
            storage,
            access,
            size,
            payload,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Head {
    pub head: u32,
    pub flags: u32,
    pub mode_serial: u32,
    pub source_index: u32,
    pub width: u32,
    pub height: u32,
    pub refresh_mhz: u32,
    pub fourcc: u32,
    pub stride: u32,
    pub slot_region: Vec<u32>,
    pub pointer_mode: u32,
    pub credit: u32,
    pub label: String,
}

impl Head {
    pub fn encode(&self) -> Vec<u8> {
        let mut o = Out::new(ty::HEAD, F_MUST);
        o.u32(self.head)
            .u32(self.flags)
            .u32(self.mode_serial)
            .u32(self.source_index);
        o.u32(self.width)
            .u32(self.height)
            .u32(self.refresh_mhz)
            .u32(self.fourcc);
        o.u32(self.stride).u32(self.slot_region.len() as u32);
        for i in 0..MAX_SLOTS {
            o.u32(self.slot_region.get(i).copied().unwrap_or(0));
        }
        o.u32(self.pointer_mode)
            .u32(self.credit)
            .name(&self.label, 32)
            .u64(0);
        o.finish()
    }

    pub fn decode(m: &[u8]) -> R<Head> {
        if m.len() < 160 {
            return Err(WireError("HEAD too short"));
        }
        let mut r = Rd::new(m);
        let head = r.u32()?;
        let flags = r.u32()?;
        let mode_serial = r.u32()?;
        let source_index = r.u32()?;
        let width = r.u32()?;
        let height = r.u32()?;
        let refresh_mhz = r.u32()?;
        let fourcc = r.u32()?;
        let stride = r.u32()?;
        let n = r.u32()? as usize;
        if n > MAX_SLOTS {
            return Err(WireError("HEAD lists more than 16 slots"));
        }
        let all = r.u32s::<16>()?;
        let pointer_mode = r.u32()?;
        let credit = r.u32()?;
        let label = r.name(32)?;
        Ok(Head {
            head,
            flags,
            mode_serial,
            source_index,
            width,
            height,
            refresh_mhz,
            fourcc,
            stride,
            slot_region: all[..n].to_vec(),
            pointer_mode,
            credit,
            label,
        })
    }
}

/// What changed in a frame since the viewer's previous one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Damage {
    Whole,
    Rects(Vec<[u32; 4]>),
    /// Bit `y` of the little-endian word array is display line `y`.
    Lines(Vec<u64>),
}

impl Damage {
    /// The lines the damage covers, clipped to `height`.
    pub fn covers_line(&self, y: u32, height: u32) -> bool {
        if y >= height {
            return false;
        }
        match self {
            Damage::Whole => true,
            Damage::Rects(r) => r.iter().any(|&[_, ry, _, rh]| y >= ry && y - ry < rh),
            Damage::Lines(w) => w
                .get(y as usize / 64)
                .is_some_and(|w| w >> (y % 64) & 1 != 0),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub head: u32,
    pub region: u32,
    pub serial: u64,
    pub mode_serial: u32,
    pub sample_ns: u64,
    pub publish_ns: u64,
    pub guest_ns: u64,
    pub lease: u16,
    pub frame_flags: u16,
    pub lease_seq: u64,
    pub damage: Damage,
}

impl Frame {
    pub fn encode(&self) -> Vec<u8> {
        let mut o = Out::new(ty::FRAME, F_MUST);
        let (kind, count) = match &self.damage {
            Damage::Whole => (DAMAGE_WHOLE, 0),
            Damage::Rects(r) => (DAMAGE_RECTS, r.len()),
            Damage::Lines(w) => (DAMAGE_LINES, w.len()),
        };
        o.u32(self.head)
            .u32(self.region)
            .u64(self.serial)
            .u32(self.mode_serial);
        o.u16(kind).u16(count as u16);
        o.u64(self.sample_ns)
            .u64(self.publish_ns)
            .u64(self.guest_ns);
        o.u16(self.lease)
            .u16(self.frame_flags)
            .u32(0)
            .u64(self.lease_seq);
        match &self.damage {
            Damage::Whole => {}
            Damage::Rects(r) => {
                for rc in r {
                    for v in rc {
                        o.u32(*v);
                    }
                }
            }
            Damage::Lines(w) => {
                for v in w {
                    o.u64(*v);
                }
            }
        }
        o.finish()
    }

    pub fn decode(m: &[u8]) -> R<Frame> {
        if m.len() < 72 {
            return Err(WireError("FRAME too short"));
        }
        let mut r = Rd::new(m);
        let head = r.u32()?;
        let region = r.u32()?;
        let serial = r.u64()?;
        let mode_serial = r.u32()?;
        let kind = r.u16()?;
        let count = r.u16()? as usize;
        let sample_ns = r.u64()?;
        let publish_ns = r.u64()?;
        let guest_ns = r.u64()?;
        let lease = r.u16()?;
        let frame_flags = r.u16()?;
        r.skip(4)?;
        let lease_seq = r.u64()?;
        let damage = match kind {
            DAMAGE_WHOLE => Damage::Whole,
            DAMAGE_RECTS => {
                if count > MAX_RECTS {
                    return Err(WireError("more than 64 damage rectangles"));
                }
                let mut v = Vec::with_capacity(count);
                for _ in 0..count {
                    v.push([r.u32()?, r.u32()?, r.u32()?, r.u32()?]);
                }
                Damage::Rects(v)
            }
            DAMAGE_LINES => {
                let mut v = Vec::with_capacity(count);
                for _ in 0..count {
                    v.push(r.u64()?);
                }
                Damage::Lines(v)
            }
            _ => return Err(WireError("unknown damage kind")),
        };
        Ok(Frame {
            head,
            region,
            serial,
            mode_serial,
            sample_ns,
            publish_ns,
            guest_ns,
            lease,
            frame_flags,
            lease_seq,
            damage,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorImage {
    pub head: u32,
    pub image_serial: u32,
    pub width: u32,
    pub height: u32,
    pub hot_x: i32,
    pub hot_y: i32,
    pub fourcc: u32,
    pub stride: u32,
    pub pixels: Vec<u8>,
}

impl CursorImage {
    pub fn encode(&self) -> Vec<u8> {
        let mut o = Out::new(ty::CURSOR_IMAGE, 0);
        o.u32(self.head)
            .u32(self.image_serial)
            .u32(self.width)
            .u32(self.height);
        o.i32(self.hot_x)
            .i32(self.hot_y)
            .u32(self.fourcc)
            .u32(self.stride);
        o.bytes(&self.pixels);
        o.finish()
    }

    pub fn decode(m: &[u8]) -> R<CursorImage> {
        if m.len() < 40 {
            return Err(WireError("CURSOR_IMAGE too short"));
        }
        let mut r = Rd::new(m);
        let head = r.u32()?;
        let image_serial = r.u32()?;
        let width = r.u32()?;
        let height = r.u32()?;
        let hot_x = r.i32()?;
        let hot_y = r.i32()?;
        let fourcc = r.u32()?;
        let stride = r.u32()?;
        if width > 128 || height > 128 {
            return Err(WireError("cursor larger than 128"));
        }
        let n = (height as usize) * (stride as usize);
        let pixels = r.take(n)?.to_vec();
        Ok(CursorImage {
            head,
            image_serial,
            width,
            height,
            hot_x,
            hot_y,
            fourcc,
            stride,
            pixels,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorMsg {
    pub code: u32,
    pub major_min: u16,
    pub major_max: u16,
    pub text: String,
}

impl ErrorMsg {
    pub fn new(code: u32, text: &str) -> ErrorMsg {
        ErrorMsg {
            code,
            major_min: MAJOR,
            major_max: MAJOR,
            text: text.into(),
        }
    }
    pub fn encode(&self) -> Vec<u8> {
        let mut o = Out::new(ty::ERROR, 0);
        o.u32(self.code).u16(self.major_min).u16(self.major_max);
        o.u32(self.text.len() as u32)
            .u32(0)
            .bytes(self.text.as_bytes());
        o.finish()
    }
    pub fn decode(m: &[u8]) -> R<ErrorMsg> {
        if m.len() < 24 {
            return Err(WireError("ERROR too short"));
        }
        let mut r = Rd::new(m);
        let code = r.u32()?;
        let major_min = r.u16()?;
        let major_max = r.u16()?;
        let n = r.u32()? as usize;
        r.skip(4)?;
        let text = String::from_utf8_lossy(r.take(n)?).into_owned();
        Ok(ErrorMsg {
            code,
            major_min,
            major_max,
            text,
        })
    }
}

/// Messages a viewer sends, once the producer has parsed them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToProducer {
    Hello(Hello),
    Request(Request),
    ImportFailed {
        region: u32,
        reason: u32,
    },
    HandlesSeen(u64),
    SetStorage {
        head: u32,
        storage: [u32; 8],
    },
    InputClaim {
        claim: bool,
    },
    Key {
        head: u32,
        code: u32,
        down: u32,
    },
    Button {
        head: u32,
        button: u32,
        down: u32,
    },
    Rel {
        head: u32,
        dx: i32,
        dy: i32,
    },
    Abs {
        head: u32,
        x: i32,
        y: i32,
    },
    Sync {
        flags: u32,
        serial: u32,
    },
    InputReset,
    Error(ErrorMsg),
    /// A type outside the core; the transport decides whether to skip it.
    Other {
        ty: u16,
        flags: u16,
        msg: Vec<u8>,
    },
}

fn fixed<'a>(m: &'a [u8], n: usize, what: &'static str) -> R<Rd<'a>> {
    if m.len() < n {
        return Err(WireError(what));
    }
    Ok(Rd::new(m))
}

impl ToProducer {
    pub fn decode(h: Header, m: &[u8]) -> R<ToProducer> {
        Ok(match h.ty {
            ty::HELLO => ToProducer::Hello(Hello::decode(m)?),
            ty::REQUEST => {
                let mut r = fixed(m, 32, "REQUEST too short")?;
                ToProducer::Request(Request {
                    head: r.u32()?,
                    kinds: r.u32()?,
                    frame_seen: r.u64()?,
                    cursor_seen: r.u32()?,
                    flags: r.u32()?,
                })
            }
            ty::IMPORT_FAILED => {
                let mut r = fixed(m, 16, "IMPORT_FAILED too short")?;
                ToProducer::ImportFailed {
                    region: r.u32()?,
                    reason: r.u32()?,
                }
            }
            ty::HANDLES_SEEN => {
                let mut r = fixed(m, 16, "HANDLES_SEEN too short")?;
                ToProducer::HandlesSeen(r.u64()?)
            }
            ty::SET_STORAGE => {
                let mut r = fixed(m, 48, "SET_STORAGE too short")?;
                let head = r.u32()?;
                r.skip(4)?;
                ToProducer::SetStorage {
                    head,
                    storage: r.u32s::<8>()?,
                }
            }
            ty::INPUT_CLAIM => {
                let mut r = fixed(m, 16, "INPUT_CLAIM too short")?;
                ToProducer::InputClaim {
                    claim: r.u32()? & 1 != 0,
                }
            }
            ty::KEY | ty::BUTTON => {
                let mut r = fixed(m, 24, "KEY too short")?;
                let (head, code, down) = (r.u32()?, r.u32()?, r.u32()?);
                if h.ty == ty::KEY {
                    ToProducer::Key { head, code, down }
                } else {
                    ToProducer::Button {
                        head,
                        button: code,
                        down,
                    }
                }
            }
            ty::REL | ty::ABS => {
                let mut r = fixed(m, 24, "motion too short")?;
                let head = r.u32()?;
                r.skip(4)?;
                let (a, b) = (r.i32()?, r.i32()?);
                if h.ty == ty::REL {
                    ToProducer::Rel { head, dx: a, dy: b }
                } else {
                    ToProducer::Abs { head, x: a, y: b }
                }
            }
            ty::SYNC => {
                let mut r = fixed(m, 16, "SYNC too short")?;
                ToProducer::Sync {
                    flags: r.u32()?,
                    serial: r.u32()?,
                }
            }
            ty::INPUT_RESET => ToProducer::InputReset,
            ty::ERROR => ToProducer::Error(ErrorMsg::decode(m)?),
            t => ToProducer::Other {
                ty: t,
                flags: h.flags,
                msg: m.to_vec(),
            },
        })
    }
}

/// Messages a producer sends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToViewer {
    Welcome(Welcome),
    Orphans(Vec<(u64, u64)>),
    Region(Region),
    RegionGone(u32),
    Ready,
    Head(Head),
    Frame(Frame),
    CursorImage(CursorImage),
    CursorNotify { head: u32, cursor_seq: u32 },
    InputAck(u32),
    InputState { flags: u32, leds: u32 },
    Error(ErrorMsg),
    Other { ty: u16, flags: u16, msg: Vec<u8> },
}

impl ToViewer {
    pub fn decode(h: Header, m: &[u8]) -> R<ToViewer> {
        Ok(match h.ty {
            ty::WELCOME => ToViewer::Welcome(Welcome::decode(m)?),
            ty::ORPHANS => {
                let mut r = fixed(m, 16, "ORPHANS too short")?;
                let n = r.u32()? as usize;
                r.skip(4)?;
                let mut v = Vec::new();
                for _ in 0..n {
                    v.push((r.u64()?, r.u64()?));
                }
                ToViewer::Orphans(v)
            }
            ty::REGION => ToViewer::Region(Region::decode(m)?),
            ty::REGION_GONE => ToViewer::RegionGone(fixed(m, 16, "REGION_GONE too short")?.u32()?),
            ty::READY => ToViewer::Ready,
            ty::HEAD => ToViewer::Head(Head::decode(m)?),
            ty::FRAME => ToViewer::Frame(Frame::decode(m)?),
            ty::CURSOR_IMAGE => ToViewer::CursorImage(CursorImage::decode(m)?),
            ty::CURSOR_NOTIFY => {
                let mut r = fixed(m, 16, "CURSOR_NOTIFY too short")?;
                ToViewer::CursorNotify {
                    head: r.u32()?,
                    cursor_seq: r.u32()?,
                }
            }
            ty::INPUT_ACK => ToViewer::InputAck(fixed(m, 16, "INPUT_ACK too short")?.u32()?),
            ty::INPUT_STATE => {
                let mut r = fixed(m, 16, "INPUT_STATE too short")?;
                ToViewer::InputState {
                    flags: r.u32()?,
                    leds: r.u32()?,
                }
            }
            ty::ERROR => ToViewer::Error(ErrorMsg::decode(m)?),
            t => ToViewer::Other {
                ty: t,
                flags: h.flags,
                msg: m.to_vec(),
            },
        })
    }
}

pub fn orphans(entries: &[(u64, u64)]) -> Vec<u8> {
    let mut o = Out::new(ty::ORPHANS, 0);
    o.u32(entries.len() as u32).u32(0);
    for &(a, b) in entries {
        o.u64(a).u64(b);
    }
    o.finish()
}

pub fn ready() -> Vec<u8> {
    Out::new(ty::READY, 0).finish()
}

pub fn region_gone(id: u32) -> Vec<u8> {
    let mut o = Out::new(ty::REGION_GONE, 0);
    o.u32(id).u32(0);
    o.finish()
}

pub fn cursor_notify(head: u32, seq: u32) -> Vec<u8> {
    let mut o = Out::new(ty::CURSOR_NOTIFY, 0);
    o.u32(head).u32(seq);
    o.finish()
}

pub fn input_ack(serial: u32) -> Vec<u8> {
    let mut o = Out::new(ty::INPUT_ACK, 0);
    o.u32(serial).u32(0);
    o.finish()
}

pub fn input_state(flags: u32, leds: u32) -> Vec<u8> {
    let mut o = Out::new(ty::INPUT_STATE, 0);
    o.u32(flags).u32(leds);
    o.finish()
}

pub fn import_failed(region: u32, reason: u32) -> Vec<u8> {
    let mut o = Out::new(ty::IMPORT_FAILED, F_MUST);
    o.u32(region).u32(reason);
    o.finish()
}

pub fn handles_seen(count: u64) -> Vec<u8> {
    let mut o = Out::new(ty::HANDLES_SEEN, 0);
    o.u64(count);
    o.finish()
}

pub fn set_storage(head: u32, storage: [u32; 8]) -> Vec<u8> {
    let mut o = Out::new(ty::SET_STORAGE, F_MUST);
    o.u32(head).u32(0);
    for s in storage {
        o.u32(s);
    }
    o.finish()
}

pub fn input_claim(claim: bool) -> Vec<u8> {
    let mut o = Out::new(ty::INPUT_CLAIM, 0);
    o.u32(claim as u32).u32(0);
    o.finish()
}

pub fn key(head: u32, code: u32, down: u32) -> Vec<u8> {
    let mut o = Out::new(ty::KEY, 0);
    o.u32(head).u32(code).u32(down).u32(0);
    o.finish()
}

pub fn button(head: u32, button: u32, down: u32) -> Vec<u8> {
    let mut o = Out::new(ty::BUTTON, 0);
    o.u32(head).u32(button).u32(down).u32(0);
    o.finish()
}

pub fn rel(head: u32, dx: i32, dy: i32) -> Vec<u8> {
    let mut o = Out::new(ty::REL, 0);
    o.u32(head).u32(0).i32(dx).i32(dy);
    o.finish()
}

pub fn abs(head: u32, x: i32, y: i32) -> Vec<u8> {
    let mut o = Out::new(ty::ABS, 0);
    o.u32(head).u32(0).i32(x).i32(y);
    o.finish()
}

pub fn sync(flags: u32, serial: u32) -> Vec<u8> {
    let mut o = Out::new(ty::SYNC, 0);
    o.u32(flags).u32(serial);
    o.finish()
}

pub fn input_reset() -> Vec<u8> {
    Out::new(ty::INPUT_RESET, 0).finish()
}

/// An extension message: the index `WELCOME` assigned, then its local number.
pub fn ext_type(index: u8, local: u8) -> u16 {
    (index as u16) << 8 | local as u16
}

/// The state region's header and head records ([`STATE_MAGIC`]).
pub mod state {
    pub const HEADER: usize = 64;
    pub const HEAD_STRIDE: usize = 64;
    pub const CURSOR_SEQ: usize = 0;
    pub const CURSOR_FLAGS: usize = 4;
    pub const CURSOR_X: usize = 8;
    pub const CURSOR_Y: usize = 12;
    pub const CURSOR_IMAGE_SERIAL: usize = 16;
    pub const FRAME_SEQ: usize = 20;
    pub const FRAME_SERIAL: usize = 24;
    pub const SAMPLE_NS: usize = 32;
    pub const PUBLISH_NS: usize = 40;
    pub const GUEST_NS: usize = 48;
    pub const SKIPPED: usize = 56;
}

/// The feedback region: a header, then one record of 16 release words per head.
pub mod feedback {
    pub const HEADER: usize = 64;
    pub const HEAD_STRIDE: usize = 128;
}
