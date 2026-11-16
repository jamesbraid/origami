//! The `qemu.test` extension, version 1: the conformance suite's stimulus,
//! control, statistics and input echo, and the deterministic test pictures.

use super::wire::{ext_type, Out, Rd, WireError};

pub const NAME: &str = "qemu.test";
pub const VERSION: u16 = 1;

pub const STIMULUS: u8 = 0x01;
pub const INPUT_ECHO: u8 = 0x02;
pub const STATS_REQUEST: u8 = 0x03;
pub const STATS: u8 = 0x04;
pub const CONTROL: u8 = 0x05;
pub const CONTROL_DONE: u8 = 0x06;

pub mod kind {
    pub const PATTERN: u32 = 1;
    pub const CURSOR_MOVE: u32 = 2;
    pub const CURSOR_IMAGE: u32 = 3;
    pub const MODE: u32 = 4;
    pub const HEAD: u32 = 5;
}

pub mod pattern {
    pub const SOLID: u32 = 1;
    pub const BAR: u32 = 2;
    pub const LINES: u32 = 3;
}

/// `CONTROL` operations.
pub mod op {
    pub const HOLD_SAMPLE: u32 = 1;
    pub const RELEASE_SAMPLE: u32 = 2;
    pub const RETIRE_QUEUED: u32 = 3;
    pub const DELAYED_MARK: u32 = 4;
    pub const HOLD_WRITES: u32 = 5;
    pub const RESUME_WRITES: u32 = 6;
}

pub mod echo {
    pub const KEY: u32 = 1;
    pub const BUTTON: u32 = 2;
    pub const REL: u32 = 3;
    pub const ABS: u32 = 4;
    pub const REPORT_END: u32 = 5;
    pub const STEERING: u32 = 6;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stimulus {
    pub head: u32,
    pub kind: u32,
    pub seed: u64,
    pub step: u32,
    pub arg: u32,
}

impl Stimulus {
    pub fn encode(&self, index: u8) -> Vec<u8> {
        let mut o = Out::new(ext_type(index, STIMULUS), 0);
        o.u32(self.head)
            .u32(self.kind)
            .u64(self.seed)
            .u32(self.step)
            .u32(self.arg);
        o.finish()
    }
    pub fn decode(m: &[u8]) -> Result<Stimulus, WireError> {
        if m.len() < 32 {
            return Err(WireError("STIMULUS too short"));
        }
        let mut r = Rd::new(m);
        Ok(Stimulus {
            head: r.u32()?,
            kind: r.u32()?,
            seed: r.u64()?,
            step: r.u32()?,
            arg: r.u32()?,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Control {
    pub op: u32,
    pub head: u32,
    pub target_viewer: u32,
    pub arg: u32,
}

impl Control {
    pub fn encode(&self, index: u8) -> Vec<u8> {
        let mut o = Out::new(ext_type(index, CONTROL), 0);
        o.u32(self.op)
            .u32(self.head)
            .u32(self.target_viewer)
            .u32(self.arg);
        o.finish()
    }
    pub fn decode(m: &[u8]) -> Result<Control, WireError> {
        if m.len() < 24 {
            return Err(WireError("CONTROL too short"));
        }
        let mut r = Rd::new(m);
        Ok(Control {
            op: r.u32()?,
            head: r.u32()?,
            target_viewer: r.u32()?,
            arg: r.u32()?,
        })
    }
}

pub fn control_done(index: u8, op: u32, result: u32) -> Vec<u8> {
    let mut o = Out::new(ext_type(index, CONTROL_DONE), 0);
    o.u32(op).u32(result);
    o.finish()
}

pub fn stats_request(index: u8) -> Vec<u8> {
    Out::new(ext_type(index, STATS_REQUEST), 0).finish()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct InputEcho {
    pub head: u32,
    pub kind: u32,
    pub code: u32,
    pub a: i32,
    pub b: i32,
    pub seq: u32,
}

impl InputEcho {
    pub fn encode(&self, index: u8) -> Vec<u8> {
        let mut o = Out::new(ext_type(index, INPUT_ECHO), 0);
        o.u32(self.head)
            .u32(self.kind)
            .u32(self.code)
            .i32(self.a)
            .i32(self.b);
        o.u32(self.seq).u64(0);
        o.finish()
    }
    pub fn decode(m: &[u8]) -> Result<InputEcho, WireError> {
        if m.len() < 40 {
            return Err(WireError("INPUT_ECHO too short"));
        }
        let mut r = Rd::new(m);
        Ok(InputEcho {
            head: r.u32()?,
            kind: r.u32()?,
            code: r.u32()?,
            a: r.i32()?,
            b: r.i32()?,
            seq: r.u32()?,
        })
    }
}

/// Counters since the producer started. 96 bytes: ten u64, then u32 `sets` and
/// u32 `max_queued`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Stats {
    pub samples: u64,
    pub fills: u64,
    pub lines_produced: u64,
    pub lines_stored: u64,
    pub lines_copied: u64,
    pub bytes_copied: u64,
    pub publishes: u64,
    pub frames_sent: u64,
    pub wakeups: u64,
    pub sets: u32,
    pub max_queued: u32,
    pub retained_bytes: u64,
}

impl Stats {
    pub fn encode(&self, index: u8) -> Vec<u8> {
        let mut o = Out::new(ext_type(index, STATS), 0);
        o.u64(self.samples)
            .u64(self.fills)
            .u64(self.lines_produced)
            .u64(self.lines_stored);
        o.u64(self.lines_copied)
            .u64(self.bytes_copied)
            .u64(self.publishes);
        o.u64(self.frames_sent).u64(self.wakeups);
        o.u64(self.retained_bytes)
            .u32(self.sets)
            .u32(self.max_queued);
        o.finish()
    }
    pub fn decode(m: &[u8]) -> Result<Stats, WireError> {
        if m.len() < 96 {
            return Err(WireError("STATS too short"));
        }
        let mut r = Rd::new(m);
        Ok(Stats {
            samples: r.u64()?,
            fills: r.u64()?,
            lines_produced: r.u64()?,
            lines_stored: r.u64()?,
            lines_copied: r.u64()?,
            bytes_copied: r.u64()?,
            publishes: r.u64()?,
            frames_sent: r.u64()?,
            wakeups: r.u64()?,
            retained_bytes: r.u64()?,
            sets: r.u32()?,
            max_queued: r.u32()?,
        })
    }
}

fn fnv(seed: u64, y: u32, s: u32) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut put = |bytes: &[u8]| {
        for &b in bytes {
            h ^= b as u64;
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    };
    put(&seed.to_le_bytes());
    put(&y.to_le_bytes());
    put(&s.to_le_bytes());
    h
}

/// Line `y` of a test picture, as `XR24` words (`0x00RRGGBB`). A pure
/// function of its arguments; `row` is one line, `h` the head height.
pub fn pattern_line(pat: u32, seed: u64, step: u32, h: u32, y: u32, row: &mut [u32]) {
    match pat {
        pattern::SOLID => {
            let v = (seed.wrapping_mul(2_654_435_761).wrapping_add(step as u64) & 0xFF_FFFF) as u32;
            row.fill(v);
        }
        pattern::BAR => {
            let white = h != 0 && (y as u64 + step as u64 * 8) % h as u64 <= 7;
            row.fill(if white { 0xFF_FFFF } else { 0 });
        }
        pattern::LINES => {
            let m = y % 7;
            if step >= m {
                let s = step - (step - m) % 7;
                row.fill((fnv(seed, y, s) & 0xFF_FFFF) as u32);
            } else {
                row.fill(0);
            }
        }
        _ => row.fill(0),
    }
    if y == 0 {
        for (i, px) in row.iter_mut().take(8).enumerate() {
            *px = (step >> (4 * i)) & 0xF;
        }
    }
}

/// The whole picture, `w` by `h`, tightly packed.
pub fn pattern_picture(pat: u32, seed: u64, step: u32, w: u32, h: u32) -> Vec<u32> {
    let mut v = vec![0; (w * h) as usize];
    for y in 0..h {
        pattern_line(
            pat,
            seed,
            step,
            h,
            y,
            &mut v[(y * w) as usize..((y + 1) * w) as usize],
        );
    }
    v
}
