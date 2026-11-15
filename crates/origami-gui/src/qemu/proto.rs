//! The version 3 display protocol: the socket messages and the control block
//! layout, from QEMU's `include/ui/sgi-display-shm.h` and
//! `docs/system/mips/sgi/display-shm.rst`. All integers are little endian.
//! Nothing here touches a socket or memory, so it is all unit-tested.

pub const MAGIC: u32 = 0x3353_5347;
pub const VERSION: u32 = 3;
pub const MAX_HEADS: usize = 8;
pub const MAX_LINES: u32 = 4096;
pub const CONTROL_BYTES: usize = 65536;
pub const REGION_ALIGN: u64 = 65536;
pub const CURSOR_MAX: u32 = 64;

// Region backends.
pub const BACKEND_VK_FD: u32 = 1;
pub const BACKEND_VK_WIN32: u32 = 2;
pub const BACKEND_SHM_FD: u32 = 3;
pub const BACKEND_SHM_WIN32: u32 = 4;

// Head families and plane formats.
pub const FAMILY_XRGB: u32 = 1;
pub const FMT_X8R8G8B8: u32 = 1;

// Head flags.
pub const HEAD_DIRTY_EXACT: u32 = 1;

// Plane flags.
pub const PLANE_BOTTOM_UP: u32 = 2;

// Message types.
pub const MSG_HELLO: u16 = 0x0001;
pub const MSG_INPUT: u16 = 0x0002;
pub const MSG_WELCOME: u16 = 0x0081;
pub const MSG_REGION: u16 = 0x0082;
pub const MSG_REGION_GONE: u16 = 0x0083;
pub const MSG_WAKE: u16 = 0x0084;
pub const MSG_INPUT_ACK: u16 = 0x0085;

// HELLO capabilities.
#[cfg_attr(target_os = "macos", allow(dead_code))]
pub const CAP_VULKAN: u32 = 1;
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub const CAP_METAL: u32 = 2;
pub const CAP_WAKE: u32 = 4;

// Region kinds: 1 to 3 are scanout memory, which a GPU may import; the
// cursor image is copied into a texture, so it is always plain shared memory.
pub const KIND_CURSOR_IMAGE: u32 = 4;

// REGION flags.
#[cfg_attr(target_os = "macos", allow(dead_code))]
pub const REGION_DEDICATED: u32 = 1;

pub const HEADER_SIZE: usize = 8;

/// The total size of a message the reader receives, by type; None for a type
/// the protocol does not have (the receiver closes the connection).
pub fn incoming_size(kind: u16) -> Option<usize> {
    match kind {
        MSG_WELCOME => Some(64),
        MSG_REGION => Some(56),
        MSG_REGION_GONE | MSG_WAKE => Some(16),
        MSG_INPUT_ACK => Some(8),
        _ => None,
    }
}

fn u16_at(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes(b[o..o + 2].try_into().unwrap())
}
fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}
fn u64_at(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}

/// What a message is, before its body.
pub fn message_type(header: &[u8]) -> u16 {
    u16_at(header, 0)
}

/// `HELLO`: the first message, 48 bytes. `caps` says how the reader can use a
/// region without copying it (`CAP_VULKAN`, `CAP_METAL`; `CAP_WAKE` is always
/// set) and `device_uuid` is its Vulkan adapter's, zero for Metal and when it
/// imports nothing.
pub fn hello(pid: u32, caps: u32, device_uuid: [u8; 16]) -> [u8; 48] {
    let mut b = [0u8; 48];
    b[0..2].copy_from_slice(&MSG_HELLO.to_le_bytes());
    b[8..12].copy_from_slice(&VERSION.to_le_bytes());
    b[12..16].copy_from_slice(&(caps | CAP_WAKE).to_le_bytes());
    b[16..20].copy_from_slice(&pid.to_le_bytes());
    b[24..40].copy_from_slice(&device_uuid);
    b
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputKind {
    Key = 1,
    Button = 2,
    Relative = 3,
    Absolute = 4,
}

/// One `INPUT` message, 32 bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Input {
    pub kind: InputKind,
    pub head: u32,
    pub code: u32,
    pub value: u32,
    pub x: i32,
    pub y: i32,
}

impl Input {
    pub fn encode(&self, ack: bool, seq: u32) -> [u8; 32] {
        let mut b = [0u8; 32];
        b[0..2].copy_from_slice(&MSG_INPUT.to_le_bytes());
        b[2..4].copy_from_slice(&u16::from(ack).to_le_bytes());
        b[4..8].copy_from_slice(&seq.to_le_bytes());
        b[8..12].copy_from_slice(&(self.kind as u32).to_le_bytes());
        b[12..16].copy_from_slice(&self.head.to_le_bytes());
        b[16..20].copy_from_slice(&self.code.to_le_bytes());
        b[20..24].copy_from_slice(&self.value.to_le_bytes());
        b[24..28].copy_from_slice(&self.x.to_le_bytes());
        b[28..32].copy_from_slice(&self.y.to_le_bytes());
        b
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Welcome {
    pub version: u32,
    pub backend: u32,
    pub nheads: u32,
    pub control_backend: u32,
    /// The Vulkan adapter QEMU exported on, zero when it exports nothing.
    pub device_uuid: [u8; 16],
    pub driver_uuid: [u8; 16],
    /// The Windows handle value for the control block, 0 on POSIX.
    pub control_handle: u64,
}

pub fn parse_welcome(b: &[u8]) -> Welcome {
    Welcome {
        version: u32_at(b, 8),
        backend: u32_at(b, 12),
        nheads: u32_at(b, 16),
        control_backend: u32_at(b, 20),
        device_uuid: b[24..40].try_into().unwrap(),
        driver_uuid: b[40..56].try_into().unwrap(),
        control_handle: u64_at(b, 56),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Region {
    pub id: u32,
    pub kind: u32,
    pub backend: u32,
    /// The Vulkan memory type QEMU allocated from (backends 1 and 2).
    pub memory_type: u32,
    pub size: u64,
    pub alloc_size: u64,
    /// `REGION_DEDICATED`.
    pub flags: u32,
    pub handle: u64,
}

pub fn parse_region(b: &[u8]) -> Region {
    Region {
        id: u32_at(b, 8),
        kind: u32_at(b, 12),
        backend: u32_at(b, 16),
        memory_type: u32_at(b, 20),
        size: u64_at(b, 24),
        alloc_size: u64_at(b, 32),
        flags: u32_at(b, 40),
        handle: u64_at(b, 48),
    }
}

pub fn parse_id(b: &[u8]) -> u32 {
    u32_at(b, 8)
}

// ---- The control block ---------------------------------------------------

pub const HEAD_OFFSET: usize = 4096;
pub const HEAD_BYTES: usize = 4096;

// Header fields.
pub const H_MAGIC: usize = 0;
pub const H_VERSION: usize = 4;
pub const H_NHEADS: usize = 8;
pub const H_HEAD_BYTES: usize = 12;
pub const H_HEAD_OFFSET: usize = 16;

// Head record fields, relative to the head's start.
pub const R_ACTIVE: usize = 0;
pub const R_FAMILY: usize = 4;
pub const R_FAMILY_VERSION: usize = 8;
pub const R_LAYOUT_GEN: usize = 12;
pub const R_WIDTH: usize = 16;
pub const R_HEIGHT: usize = 20;
pub const R_REFRESH: usize = 24;
pub const R_FLAGS: usize = 28;
pub const R_CONTENT_GEN: usize = 32;
pub const R_ARMED: usize = 44;
pub const R_CURSOR_GEN: usize = 48;
pub const R_CURSOR_ON: usize = 52;
pub const R_CURSOR_X: usize = 56;
pub const R_CURSOR_Y: usize = 60;
pub const R_CURSOR_HOT_X: usize = 64;
pub const R_CURSOR_HOT_Y: usize = 68;
pub const R_CURSOR_W: usize = 72;
pub const R_CURSOR_H: usize = 76;
pub const R_PLANE: usize = 80;
#[cfg(test)]
pub const PLANE_BYTES: usize = 32;
pub const R_CURSOR_IMAGE: usize = 400;
pub const R_DIRTY: usize = 1536;
#[cfg(test)]
pub const DIRTY_WORDS: usize = 64;

// Plane descriptor fields.
pub const P_REGION: usize = 0;
pub const P_FORMAT: usize = 4;
pub const P_OFFSET: usize = 8;
pub const P_WIDTH: usize = 16;
pub const P_HEIGHT: usize = 20;
pub const P_STRIDE: usize = 24;
pub const P_FLAGS: usize = 28;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hello_has_the_fields_at_their_offsets() {
        let h = hello(4242, 0, [0; 16]);
        assert_eq!(h.len(), 48);
        assert_eq!(message_type(&h), MSG_HELLO);
        assert_eq!(u32_at(&h, 8), 3);
        assert_eq!(u32_at(&h, 12), CAP_WAKE);
        assert_eq!(u32_at(&h, 16), 4242);
        assert!(
            h[20..].iter().all(|&b| b == 0),
            "uuid and reserved are zero"
        );
        let uuid: [u8; 16] = std::array::from_fn(|i| i as u8 + 1);
        let h = hello(7, CAP_VULKAN, uuid);
        assert_eq!(u32_at(&h, 12), CAP_VULKAN | CAP_WAKE);
        assert_eq!(&h[24..40], &uuid);
        assert!(h[20..24].iter().all(|&b| b == 0) && h[40..].iter().all(|&b| b == 0));
    }

    #[test]
    fn input_encodes_every_field() {
        let i = Input {
            kind: InputKind::Absolute,
            head: 1,
            code: 7,
            value: 1,
            x: -5,
            y: 300,
        };
        let b = i.encode(true, 9);
        assert_eq!(message_type(&b), MSG_INPUT);
        assert_eq!(u16_at(&b, 2), 1, "ack requested");
        assert_eq!(u32_at(&b, 4), 9);
        assert_eq!(u32_at(&b, 8), 4);
        assert_eq!((u32_at(&b, 12), u32_at(&b, 16), u32_at(&b, 20)), (1, 7, 1));
        assert_eq!((u32_at(&b, 24) as i32, u32_at(&b, 28) as i32), (-5, 300));
        assert_eq!(u16_at(&i.encode(false, 0), 2), 0);
    }

    #[test]
    fn incoming_messages_parse_from_their_offsets() {
        let mut w = [0u8; 64];
        w[0..2].copy_from_slice(&MSG_WELCOME.to_le_bytes());
        w[8..12].copy_from_slice(&3u32.to_le_bytes());
        w[12..16].copy_from_slice(&3u32.to_le_bytes());
        w[16..20].copy_from_slice(&2u32.to_le_bytes());
        w[20..24].copy_from_slice(&4u32.to_le_bytes());
        w[56..64].copy_from_slice(&0x1234_5678_9abc_u64.to_le_bytes());
        w[24..40].copy_from_slice(&[0xaa; 16]);
        w[40..56].copy_from_slice(&[0xbb; 16]);
        assert_eq!(
            parse_welcome(&w),
            Welcome {
                version: 3,
                backend: 3,
                nheads: 2,
                control_backend: 4,
                device_uuid: [0xaa; 16],
                driver_uuid: [0xbb; 16],
                control_handle: 0x1234_5678_9abc
            }
        );
        let mut r = [0u8; 56];
        r[8..12].copy_from_slice(&5u32.to_le_bytes());
        r[12..16].copy_from_slice(&1u32.to_le_bytes());
        r[16..20].copy_from_slice(&3u32.to_le_bytes());
        r[24..32].copy_from_slice(&(1u64 << 21).to_le_bytes());
        r[32..40].copy_from_slice(&(1u64 << 21).to_le_bytes());
        r[48..56].copy_from_slice(&77u64.to_le_bytes());
        r[20..24].copy_from_slice(&6u32.to_le_bytes());
        r[40..44].copy_from_slice(&REGION_DEDICATED.to_le_bytes());
        assert_eq!(
            parse_region(&r),
            Region {
                id: 5,
                kind: 1,
                backend: 3,
                memory_type: 6,
                size: 1 << 21,
                alloc_size: 1 << 21,
                flags: REGION_DEDICATED,
                handle: 77
            }
        );
        let mut g = [0u8; 16];
        g[8..12].copy_from_slice(&6u32.to_le_bytes());
        assert_eq!(parse_id(&g), 6);
    }

    #[test]
    fn only_the_protocols_incoming_types_have_a_size() {
        for (t, n) in [
            (MSG_WELCOME, 64),
            (MSG_REGION, 56),
            (MSG_REGION_GONE, 16),
            (MSG_WAKE, 16),
            (MSG_INPUT_ACK, 8),
        ] {
            assert_eq!(incoming_size(t), Some(n));
        }
        for t in [MSG_HELLO, MSG_INPUT, 0, 0x0086, 0xffff] {
            assert_eq!(incoming_size(t), None, "{t:#x}");
        }
    }

    #[test]
    fn the_control_block_offsets_match_the_header() {
        // Spot checks against include/ui/sgi-display-shm.h's BUILD_BUG_ONs.
        assert_eq!(R_PLANE + 8 * PLANE_BYTES, 336, "tables follow the 8 planes");
        assert_eq!(R_CURSOR_IMAGE, 336 + 4 * 16);
        assert_eq!(R_DIRTY + DIRTY_WORDS * 8, 2048);
        const { assert!(HEAD_OFFSET + MAX_HEADS * HEAD_BYTES <= CONTROL_BYTES) };
    }
}
