//! A reader of the control block (region 0): which heads exist, what each
//! shows, which lines changed and where the guest's cursor is. QEMU writes
//! everything but `armed` and `dirty`, so every field is read as untrusted.

use super::proto::*;
use super::region::Memory;
use std::fmt;
use std::sync::atomic::{fence, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;

#[derive(Debug, PartialEq, Eq)]
pub enum ControlError {
    /// Not a control block we can read.
    Layout(String),
    /// QEMU kept rewriting the layout under every attempt.
    Torn,
}

impl fmt::Display for ControlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ControlError::Layout(why) => write!(f, "display control block: {why}"),
            ControlError::Torn => f.write_str("display control block: the layout never settled"),
        }
    }
}

impl std::error::Error for ControlError {}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Plane {
    pub region: u32,
    pub format: u32,
    pub offset: u64,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub flags: u32,
}

/// The fields `layout_gen` covers, as one consistent read.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Layout {
    pub layout_gen: u32,
    pub active: bool,
    pub family: u32,
    pub family_version: u32,
    pub width: u32,
    pub height: u32,
    pub refresh_mhz: u32,
    pub flags: u32,
    pub plane0: Plane,
    pub cursor_region: u32,
    pub cursor_offset: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CursorState {
    pub on: bool,
    pub x: i32,
    pub y: i32,
    pub hot_x: i32,
    pub hot_y: i32,
    pub width: u32,
    pub height: u32,
    pub gen: u32,
}

pub struct Control {
    mem: Arc<dyn Memory>,
}

/// Attempts at a consistent layout before giving up for now.
const TRIES: u32 = 1000;

impl Control {
    pub fn new(mem: Arc<dyn Memory>) -> Result<Control, ControlError> {
        if mem.len() < CONTROL_BYTES {
            return Err(ControlError::Layout(format!(
                "{} bytes, need {CONTROL_BYTES}",
                mem.len()
            )));
        }
        let c = Control { mem };
        // The writer stores the magic last.
        let magic = c.u32_at(H_MAGIC);
        if magic != MAGIC {
            return Err(ControlError::Layout(format!("magic {magic:#x}")));
        }
        let version = c.u32_at(H_VERSION);
        if version != VERSION {
            return Err(ControlError::Layout(format!(
                "version {version}, this reader reads {VERSION}"
            )));
        }
        if c.u32_at(H_HEAD_BYTES) as usize != HEAD_BYTES
            || c.u32_at(H_HEAD_OFFSET) as usize != HEAD_OFFSET
        {
            return Err(ControlError::Layout(
                "head records are not where version 3 puts them".into(),
            ));
        }
        Ok(c)
    }

    fn atom32(&self, off: usize) -> &AtomicU32 {
        debug_assert!(off % 4 == 0 && off + 4 <= self.mem.len());
        // SAFETY: in bounds (the block is CONTROL_BYTES long and every offset
        // used is inside it) and aligned; another process stores to it, which
        // atomics permit.
        unsafe { &*(self.mem.base().add(off) as *const AtomicU32) }
    }

    fn atom64(&self, off: usize) -> &AtomicU64 {
        debug_assert!(off % 8 == 0 && off + 8 <= self.mem.len());
        // SAFETY: as above.
        unsafe { &*(self.mem.base().add(off) as *const AtomicU64) }
    }

    fn u32_at(&self, off: usize) -> u32 {
        self.atom32(off).load(Ordering::Acquire)
    }

    fn head_off(h: usize) -> usize {
        HEAD_OFFSET + h * HEAD_BYTES
    }

    /// Heads published now, 0 to 8.
    pub fn nheads(&self) -> usize {
        (self.u32_at(H_NHEADS) as usize).min(MAX_HEADS)
    }

    pub fn layout_gen(&self, h: usize) -> u32 {
        self.u32_at(Self::head_off(h) + R_LAYOUT_GEN)
    }

    pub fn cursor_gen(&self, h: usize) -> u32 {
        self.u32_at(Self::head_off(h) + R_CURSOR_GEN)
    }

    pub fn content_gen(&self, h: usize) -> u64 {
        self.atom64(Self::head_off(h) + R_CONTENT_GEN)
            .load(Ordering::Acquire)
    }

    /// The layout under its seqlock: retried while QEMU is inside it or moved
    /// it during the read.
    pub fn layout(&self, h: usize) -> Result<Layout, ControlError> {
        let o = Self::head_off(h);
        for _ in 0..TRIES {
            let g0 = self.u32_at(o + R_LAYOUT_GEN);
            if g0 & 1 == 1 {
                std::hint::spin_loop();
                continue;
            }
            let p = o + R_PLANE;
            let l = Layout {
                layout_gen: g0,
                active: self.u32_at(o + R_ACTIVE) != 0,
                family: self.u32_at(o + R_FAMILY),
                family_version: self.u32_at(o + R_FAMILY_VERSION),
                width: self.u32_at(o + R_WIDTH),
                height: self.u32_at(o + R_HEIGHT),
                refresh_mhz: self.u32_at(o + R_REFRESH),
                flags: self.u32_at(o + R_FLAGS),
                plane0: Plane {
                    region: self.u32_at(p + P_REGION),
                    format: self.u32_at(p + P_FORMAT),
                    offset: self.atom64(p + P_OFFSET).load(Ordering::Acquire),
                    width: self.u32_at(p + P_WIDTH),
                    height: self.u32_at(p + P_HEIGHT),
                    stride: self.u32_at(p + P_STRIDE),
                    flags: self.u32_at(p + P_FLAGS),
                },
                cursor_region: self.u32_at(o + R_CURSOR_IMAGE),
                cursor_offset: self.atom64(o + R_CURSOR_IMAGE + 8).load(Ordering::Acquire),
            };
            fence(Ordering::Acquire);
            if self.u32_at(o + R_LAYOUT_GEN) == g0 {
                return Ok(l);
            }
        }
        Err(ControlError::Torn)
    }

    /// Take the dirty bits of lines `0..lines` (at most 4,096): each word is
    /// exchanged with 0, so a line QEMU marks afterwards is for the next take.
    /// Returns the lines that were set, in order.
    pub fn take_dirty(&self, h: usize, lines: u32) -> Vec<u32> {
        let o = Self::head_off(h) + R_DIRTY;
        let lines = lines.min(MAX_LINES);
        let mut out = Vec::new();
        for w in 0..(lines as usize).div_ceil(64) {
            let mut bits = self.atom64(o + w * 8).swap(0, Ordering::AcqRel);
            while bits != 0 {
                let b = bits.trailing_zeros();
                bits &= bits - 1;
                let y = w as u32 * 64 + b;
                if y < lines {
                    out.push(y);
                }
            }
        }
        out
    }

    /// Ask for a `WAKE` the next time the head changes.
    pub fn arm(&self, h: usize) {
        self.atom32(Self::head_off(h) + R_ARMED)
            .store(1, Ordering::SeqCst);
    }

    /// The cursor fields, which change with no generation of their own, and
    /// `cursor_gen`, which covers the image and hot spot.
    pub fn cursor(&self, h: usize) -> CursorState {
        let o = Self::head_off(h);
        let i = |off: usize| self.u32_at(o + off) as i32;
        // QEMU writes the fields and then advances the generation, so the
        // generation is read first: what is read after it is at least as new.
        let gen = self.u32_at(o + R_CURSOR_GEN);
        CursorState {
            on: self.u32_at(o + R_CURSOR_ON) != 0,
            x: i(R_CURSOR_X),
            y: i(R_CURSOR_Y),
            hot_x: i(R_CURSOR_HOT_X),
            hot_y: i(R_CURSOR_HOT_Y),
            width: self.u32_at(o + R_CURSOR_W).min(CURSOR_MAX),
            height: self.u32_at(o + R_CURSOR_H).min(CURSOR_MAX),
            gen,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::region::testing::Block;
    use super::*;

    fn control(b: &Block) -> Control {
        Control::new(b.mem.clone()).unwrap()
    }

    #[test]
    fn a_block_that_is_not_ready_or_not_ours_is_refused() {
        let b = Block::new(1);
        b.mem.put32(H_MAGIC, 0);
        assert!(
            matches!(Control::new(b.mem.clone()), Err(ControlError::Layout(m)) if m.contains("magic"))
        );
        b.mem.put32(H_MAGIC, MAGIC);
        b.mem.put32(H_VERSION, 2);
        assert!(
            matches!(Control::new(b.mem.clone()), Err(ControlError::Layout(m)) if m.contains("version 2"))
        );
        b.mem.put32(H_VERSION, VERSION);
        b.mem.put32(H_HEAD_BYTES, 2048);
        assert!(matches!(
            Control::new(b.mem.clone()),
            Err(ControlError::Layout(_))
        ));
        let small: Arc<dyn Memory> = super::super::region::testing::Heap::new(4096);
        assert!(matches!(Control::new(small), Err(ControlError::Layout(_))));
    }

    #[test]
    fn nheads_is_read_live_and_capped() {
        let b = Block::new(2);
        let c = control(&b);
        assert_eq!(c.nheads(), 2);
        b.mem.put32(H_NHEADS, 99);
        assert_eq!(c.nheads(), 8);
    }

    #[test]
    fn the_layout_is_read_whole() {
        let b = Block::new(2);
        b.set_layout(1, 1280, 1024, 4, 256, 5120, HEAD_DIRTY_EXACT);
        let l = control(&b).layout(1).unwrap();
        assert!(l.active);
        assert_eq!(
            (l.family, l.width, l.height, l.flags),
            (FAMILY_XRGB, 1280, 1024, HEAD_DIRTY_EXACT)
        );
        assert_eq!(
            l.plane0,
            Plane {
                region: 4,
                format: FMT_X8R8G8B8,
                offset: 256,
                width: 1280,
                height: 1024,
                stride: 5120,
                flags: 0
            }
        );
        assert_eq!(l.layout_gen % 2, 0);
        assert!(
            !control(&b).layout(0).unwrap().active,
            "a head that does not exist is zero"
        );
    }

    #[test]
    fn a_layout_the_writer_is_inside_is_not_returned() {
        let b = Block::new(1);
        b.set_layout(0, 8, 8, 1, 0, 32, 0);
        let g = b.get(0, R_LAYOUT_GEN);
        b.put(0, R_LAYOUT_GEN, g + 1);
        assert_eq!(control(&b).layout(0), Err(ControlError::Torn));
        b.put(0, R_LAYOUT_GEN, g + 2);
        assert!(control(&b).layout(0).is_ok());
    }

    #[test]
    fn a_layout_rewritten_during_the_read_is_read_again() {
        // A writer thread flips between two whole layouts; every read must be
        // one of them, never a mix.
        let b = Arc::new(Block::new(1));
        b.set_layout(0, 8, 4, 1, 0, 32, 0);
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let t = {
            let (b, stop) = (b.clone(), stop.clone());
            std::thread::spawn(move || {
                let mut n = 0u32;
                while !stop.load(Ordering::Relaxed) {
                    n += 1;
                    if n % 2 == 0 {
                        b.set_layout(0, 8, 4, 1, 0, 32, 0);
                    } else {
                        b.set_layout(0, 16, 8, 2, 256, 64, 0);
                    }
                }
            })
        };
        let c = control(&b);
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(300);
        let mut seen = [0u32; 2];
        while std::time::Instant::now() < deadline {
            // The writer may hold the layout odd for all our tries; that is a
            // retry for the caller, never a mix.
            let Ok(l) = c.layout(0) else { continue };
            match (
                l.width,
                l.height,
                l.plane0.region,
                l.plane0.stride,
                l.plane0.offset,
            ) {
                (8, 4, 1, 32, 0) => seen[0] += 1,
                (16, 8, 2, 64, 256) => seen[1] += 1,
                other => panic!("a torn layout: {other:?}"),
            }
        }
        stop.store(true, Ordering::Relaxed);
        t.join().unwrap();
        assert!(seen[0] + seen[1] > 100, "{seen:?}");
    }

    #[test]
    fn dirty_bits_are_taken_once_and_clear_to_zero() {
        let b = Block::new(1);
        b.set_layout(0, 64, 200, 1, 0, 256, HEAD_DIRTY_EXACT);
        let c = control(&b);
        assert_eq!(
            c.take_dirty(0, 200).len(),
            200,
            "a new layout dirties every line"
        );
        assert_eq!(c.take_dirty(0, 200), Vec::<u32>::new());
        b.mark(0, 3, 5);
        b.mark(0, 63, 66);
        b.mark(0, 199, 200);
        assert_eq!(c.take_dirty(0, 200), vec![3, 4, 63, 64, 65, 199]);
        assert_eq!(b.dirty_word(0, 0), 0, "taken");
        // A bit past the head's last line is not a line.
        b.mark(0, 210, 212);
        assert_eq!(c.take_dirty(0, 200), Vec::<u32>::new());
        // Lines are capped at 4,096.
        let tall = Block::new(1);
        tall.mark(0, 0, 5000);
        assert_eq!(control(&tall).take_dirty(0, 5000).len(), 4096);
    }

    #[test]
    fn content_generation_and_arming_are_plain_words() {
        let b = Block::new(1);
        let c = control(&b);
        assert_eq!(c.content_gen(0), 0);
        b.bump(0);
        b.bump(0);
        assert_eq!(c.content_gen(0), 2);
        assert_eq!(b.get(0, R_ARMED), 0);
        c.arm(0);
        assert_eq!(b.get(0, R_ARMED), 1);
    }

    #[test]
    fn the_cursor_fields_are_read_and_clamped() {
        let b = Block::new(1);
        b.put(0, R_CURSOR_ON, 1);
        b.put(0, R_CURSOR_X, (-3i32) as u32);
        b.put(0, R_CURSOR_Y, 40);
        b.put(0, R_CURSOR_HOT_X, 1);
        b.put(0, R_CURSOR_HOT_Y, 2);
        b.put(0, R_CURSOR_W, 9999);
        b.put(0, R_CURSOR_H, 16);
        b.put(0, R_CURSOR_GEN, 6);
        assert_eq!(
            control(&b).cursor(0),
            CursorState {
                on: true,
                x: -3,
                y: 40,
                hot_x: 1,
                hot_y: 2,
                width: 64,
                height: 16,
                gen: 6
            }
        );
    }
}
