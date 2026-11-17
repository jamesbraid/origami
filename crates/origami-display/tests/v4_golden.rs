//! Golden bytes for version 4, written from the design's tables rather than
//! from the encoder.

use origami_display::v4::testext::{self, pattern};
use origami_display::v4::wire::*;

/// `(offset, width in bytes, value)`.
type Field = (usize, usize, u64);

fn at(m: &[u8], off: usize, n: usize) -> u64 {
    m[off..off + n]
        .iter()
        .rev()
        .fold(0, |a, &b| a << 8 | b as u64)
}

fn check(m: &[u8], ty: u16, flags: u16, len: usize, fields: &[Field]) {
    assert_eq!(at(m, 0, 2), ty as u64, "type");
    assert_eq!(at(m, 2, 2), flags as u64, "flags");
    assert_eq!(at(m, 4, 4), len as u64, "length field");
    assert_eq!(m.len(), len, "size");
    for &(o, n, v) in fields {
        assert_eq!(at(m, o, n), v, "offset {o} width {n}");
    }
}

#[test]
fn viewer_messages_sit_where_the_tables_put_them() {
    let h = Hello {
        major: 4,
        minor: 0,
        features: 0x11,
        name: "v".into(),
        storage: [1, 2, 0, 0, 0, 0, 0, 0],
        credit: 2,
        max_width: 3000,
        max_height: 2000,
        prev_viewer_id: 9,
        max_slot_bytes: 5,
        prev_handles_taken: 7,
        prev_session_id: 0xAA,
        offers: vec![Offer {
            name: "qemu.test".into(),
            version_min: 1,
            version_max: 1,
            payload: vec![1, 2, 3],
        }],
    };
    let m = h.encode();
    check(
        &m,
        0x0001,
        1,
        120 + 40 + 8,
        &[
            (8, 2, 4),
            (10, 2, 0),
            (12, 4, 0x11),
            (16, 1, b'v' as u64),
            (48, 4, 1),
            (52, 4, 2),
            (56, 4, 0),
            (80, 4, 2),
            (84, 4, 3000),
            (88, 4, 2000),
            (92, 4, 9),
            (96, 4, 5),
            (100, 4, 1),
            (104, 8, 7),
            (112, 8, 0xAA),
            (120, 1, b'q' as u64),
            (152, 2, 1),
            (154, 2, 1),
            (156, 4, 3),
            (160, 3, 0x030201),
        ],
    );
    assert_eq!(Hello::decode(&m).unwrap(), h);

    let r = Request {
        head: 3,
        kinds: 3,
        frame_seen: 0x1122334455667788,
        cursor_seen: 6,
        flags: 1,
    };
    check(
        &r.encode(),
        0x0002,
        1,
        32,
        &[
            (8, 4, 3),
            (12, 4, 3),
            (16, 8, 0x1122334455667788),
            (24, 4, 6),
            (28, 4, 1),
        ],
    );
    check(
        &import_failed(5, 3),
        0x0003,
        1,
        16,
        &[(8, 4, 5), (12, 4, 3)],
    );
    check(&handles_seen(16), 0x0004, 0, 16, &[(8, 8, 16)]);
    check(
        &set_storage(2, [1, 2, 3, 0, 0, 0, 0, 9]),
        0x0005,
        1,
        48,
        &[(8, 4, 2), (16, 4, 1), (20, 4, 2), (24, 4, 3), (44, 4, 9)],
    );
    check(&input_claim(true), 0x0010, 0, 16, &[(8, 4, 1)]);
    check(
        &key(1, 30, 1),
        0x0011,
        0,
        24,
        &[(8, 4, 1), (12, 4, 30), (16, 4, 1)],
    );
    check(
        &button(1, 2, 0),
        0x0012,
        0,
        24,
        &[(8, 4, 1), (12, 4, 2), (16, 4, 0)],
    );
    check(
        &rel(1, -1, 7),
        0x0013,
        0,
        24,
        &[(8, 4, 1), (16, 4, 0xFFFF_FFFF), (20, 4, 7)],
    );
    check(
        &abs(1, 640, -2),
        0x0014,
        0,
        24,
        &[(8, 4, 1), (16, 4, 640), (20, 4, 0xFFFF_FFFE)],
    );
    check(&sync(1, 77), 0x0015, 0, 16, &[(8, 4, 1), (12, 4, 77)]);
    check(&input_reset(), 0x0016, 0, 8, &[]);
}

#[test]
fn producer_messages_sit_where_the_tables_put_them() {
    let w = Welcome {
        major: 4,
        minor: 0,
        features: 3,
        session_id: 0xABCDEF,
        viewer_id: 4,
        state_region: 1,
        feedback_region: 2,
        credit: 2,
        max_viewers: 4,
        name: "p".into(),
        accepts: vec![Accept {
            name: "qemu.test".into(),
            version: 1,
            index: 1,
            payload: vec![],
        }],
    };
    let m = w.encode();
    check(
        &m,
        0x0081,
        1,
        80 + 40,
        &[
            (8, 2, 4),
            (12, 4, 3),
            (16, 8, 0xABCDEF),
            (24, 4, 4),
            (28, 4, 1),
            (32, 4, 2),
            (36, 4, 2),
            (40, 4, 4),
            (44, 4, 1),
            (48, 1, b'p' as u64),
            (80, 1, b'q' as u64),
            (112, 2, 1),
            (114, 1, 1),
            (116, 4, 0),
        ],
    );
    assert_eq!(Welcome::decode(&m).unwrap(), w);

    let r = Region {
        id: 7,
        kind: 3,
        storage: 1,
        access: 1,
        size: 65536,
        payload: vec![],
    };
    check(
        &r.encode(),
        0x0082,
        3,
        48,
        &[
            (8, 4, 7),
            (12, 4, 3),
            (16, 4, 1),
            (20, 4, 1),
            (24, 8, 65536),
            (32, 4, 0),
            (40, 8, 0),
        ],
    );
    let r = Region {
        payload: vec![9; 12],
        ..r
    };
    let m = r.encode();
    check(
        &m,
        0x0082,
        3,
        40 + 16 + 8,
        &[(32, 4, 12), (40, 1, 9), (56, 8, 0)],
    );
    assert_eq!(Region::decode(&m).unwrap(), r);
    check(&region_gone(7), 0x0083, 0, 16, &[(8, 4, 7)]);
    check(&ready(), 0x0084, 0, 8, &[]);

    let h = Head {
        head: 1,
        flags: 1,
        mode_serial: 5,
        source_index: 2,
        width: 1280,
        height: 1024,
        refresh_mhz: 60000,
        fourcc: FOURCC_XR24,
        stride: 5120,
        slot_region: vec![10, 11, 12],
        pointer_mode: 2,
        credit: 1,
        label: "main".into(),
    };
    let m = h.encode();
    check(
        &m,
        0x0085,
        1,
        160,
        &[
            (8, 4, 1),
            (12, 4, 1),
            (16, 4, 5),
            (20, 4, 2),
            (24, 4, 1280),
            (28, 4, 1024),
            (32, 4, 60000),
            (36, 4, 0x3432_5258),
            (40, 4, 5120),
            (44, 4, 3),
            (48, 4, 10),
            (52, 4, 11),
            (56, 4, 12),
            (60, 4, 0),
            (112, 4, 2),
            (116, 4, 1),
            (120, 4, u32::from_le_bytes(*b"main") as u64),
            (152, 8, 0),
        ],
    );
    assert_eq!(Head::decode(&m).unwrap(), h);

    let c = CursorImage {
        head: 0,
        image_serial: 3,
        width: 2,
        height: 1,
        hot_x: -1,
        hot_y: 2,
        fourcc: FOURCC_AR24,
        stride: 8,
        pixels: vec![1, 2, 3, 4, 5, 6, 7, 8],
    };
    let m = c.encode();
    check(
        &m,
        0x0087,
        0,
        48,
        &[
            (12, 4, 3),
            (16, 4, 2),
            (20, 4, 1),
            (24, 4, 0xFFFF_FFFF),
            (28, 4, 2),
            (32, 4, 0x3432_5241),
            (36, 4, 8),
            (40, 8, 0x0807060504030201),
        ],
    );
    assert_eq!(CursorImage::decode(&m).unwrap(), c);
    check(
        &cursor_notify(1, 8),
        0x0088,
        0,
        16,
        &[(8, 4, 1), (12, 4, 8)],
    );
    check(&input_ack(9), 0x0089, 0, 16, &[(8, 4, 9)]);
    check(&input_state(1, 4), 0x008A, 0, 16, &[(8, 4, 1), (12, 4, 4)]);
    check(
        &orphans(&[(5, 99)]),
        0x008B,
        0,
        32,
        &[(8, 4, 1), (16, 8, 5), (24, 8, 99)],
    );
    let e = ErrorMsg {
        code: 1,
        major_min: 3,
        major_max: 4,
        text: "no".into(),
    };
    let m = e.encode();
    check(
        &m,
        0x00F0,
        0,
        32,
        &[
            (8, 4, 1),
            (12, 2, 3),
            (14, 2, 4),
            (16, 4, 2),
            (24, 2, u16::from_le_bytes(*b"no") as u64),
        ],
    );
    assert_eq!(ErrorMsg::decode(&m).unwrap(), e);
}

#[test]
fn a_frame_is_72_bytes_with_the_lease_token_last() {
    let f = Frame {
        head: 1,
        region: 9,
        serial: 0x0102030405060708,
        mode_serial: 3,
        sample_ns: 100,
        publish_ns: 200,
        guest_ns: 300,
        lease: 1,
        frame_flags: 0,
        lease_seq: 0x1000_0000_0000_0007,
        damage: Damage::Whole,
    };
    let m = f.encode();
    #[rustfmt::skip]
    let want: [u8; 72] = [
        0x86, 0x00, 0x01, 0x00, 72, 0, 0, 0,          // type FRAME, MUST, length
        1, 0, 0, 0,  9, 0, 0, 0,                        // head, region
        8, 7, 6, 5, 4, 3, 2, 1,                         // serial
        3, 0, 0, 0,  0, 0, 0, 0,                        // mode_serial, damage kind 0, count 0
        100, 0, 0, 0, 0, 0, 0, 0,                       // sample_ns
        200, 0, 0, 0, 0, 0, 0, 0,                       // publish_ns
        0x2C, 1, 0, 0, 0, 0, 0, 0,                      // guest_ns 300
        1, 0, 0, 0,  0, 0, 0, 0,                        // lease 1, flags 0, reserved
        7, 0, 0, 0, 0, 0, 0, 0x10,                      // lease_seq
    ];
    assert_eq!(m, want);
    assert_eq!(Frame::decode(&m).unwrap(), f);

    let r = Frame {
        damage: Damage::Rects(vec![[1, 2, 3, 4]]),
        ..f.clone()
    };
    let m = r.encode();
    check(
        &m,
        0x0086,
        1,
        88,
        &[(28, 2, 1), (30, 2, 1), (72, 4, 1), (84, 4, 4)],
    );
    assert_eq!(Frame::decode(&m).unwrap(), r);
    let l = Frame {
        damage: Damage::Lines(vec![0b101, 1 << 63]),
        ..f
    };
    let m = l.encode();
    check(
        &m,
        0x0086,
        1,
        88,
        &[(28, 2, 2), (30, 2, 2), (72, 8, 0b101), (80, 8, 1 << 63)],
    );
    assert_eq!(Frame::decode(&m).unwrap(), l);
    assert!(
        l.damage.covers_line(0, 100)
            && !l.damage.covers_line(1, 100)
            && l.damage.covers_line(127, 128)
    );
}

#[test]
fn state_and_feedback_offsets_match_the_tables() {
    assert_eq!((state::HEADER, state::HEAD_STRIDE), (64, 64));
    assert_eq!(
        [
            state::CURSOR_SEQ,
            state::CURSOR_FLAGS,
            state::CURSOR_X,
            state::CURSOR_Y,
            state::CURSOR_IMAGE_SERIAL
        ],
        [0, 4, 8, 12, 16]
    );
    assert_eq!(
        [
            state::FRAME_SEQ,
            state::FRAME_SERIAL,
            state::SAMPLE_NS,
            state::PUBLISH_NS,
            state::GUEST_NS,
            state::SKIPPED
        ],
        [20, 24, 32, 40, 48, 56]
    );
    assert_eq!((feedback::HEADER, feedback::HEAD_STRIDE), (64, 128));
    // 65,536 bytes hold 511 head records of 128 bytes after the 64-byte header.
    assert_eq!(FEEDBACK_HEADS, (FEEDBACK_SIZE - 64) / 128);
}

#[test]
fn headers_outside_the_rules_are_refused() {
    for len in [0u32, 7, 12, 131_080] {
        let mut h = [0u8; 8];
        h[4..].copy_from_slice(&len.to_le_bytes());
        assert!(Header::parse(&h).is_err(), "length {len}");
    }
    let mut h = [0u8; 8];
    h[4..].copy_from_slice(&131_072u32.to_le_bytes());
    assert!(Header::parse(&h).is_ok());
}

/// FNV-1a of the little-endian picture bytes, computed independently of the
/// library by a script following the design's text.
fn fnv_picture(p: &[u32]) -> u64 {
    p.iter()
        .flat_map(|v| v.to_le_bytes())
        .fold(0xcbf2_9ce4_8422_2325u64, |h, b| {
            (h ^ b as u64).wrapping_mul(0x0000_0100_0000_01b3)
        })
}

#[test]
fn each_pattern_has_a_golden_picture() {
    let golden: [(u32, u64, u32, u64); 10] = [
        (1, 0x0, 0x0, 0x7da144b97d054b25),
        (1, 0x7, 0x3, 0xa43f127fd907fb76),
        (1, 0xc0ffee, 0x12345, 0xccbe3fb7530aa6e4),
        (2, 0x0, 0x0, 0xe5be99d0ceba0165),
        (2, 0x5, 0x3, 0x3e98ce38ab848b66),
        (2, 0x5, 0x11, 0xb37257fb600951d5),
        (3, 0x1234, 0x0, 0xfaf5676ffff9f935),
        (3, 0x1234, 0x5, 0xc2bc2a196516fcd0),
        (3, 0x1234, 0x9, 0xf2911f3dfccdc1dc),
        (3, 0xc0ffee, 0x64, 0xfae8f7e8619ca0e7),
    ];
    for (pat, seed, step, want) in golden {
        let p = testext::pattern_picture(pat, seed, step, 16, 8);
        assert_eq!(
            fnv_picture(&p),
            want,
            "pattern {pat} seed {seed:#x} step {step:#x}"
        );
    }
    assert_eq!([pattern::SOLID, pattern::BAR, pattern::LINES], [1, 2, 3]);
}
