//! What the engine knows about a machine's display: the control block, the
//! regions QEMU has passed, and for each head what the UI has taken. Nothing
//! here touches a process or a socket, so a synthetic control block and heap
//! regions exercise all of it.
//!
//! This is the generic xrgb path of protocol version 3: the head's plane 0 is
//! a x8r8g8b8 frame QEMU has already resolved. Where the region is memory the
//! GPU imported, the changed lines are only named and the UI draws them from
//! it; where it is plain shared memory, they are copied out of the mapping for
//! the UI to upload. It sits behind [`Engine::take_frame`] and
//! [`Engine::cursor`]; a shader for each model's own family replaces it.
//!
//! [`Engine::take_frame`]: crate::core::Engine::take_frame
//! [`Engine::cursor`]: crate::core::Engine::cursor

use super::control::{Control, ControlError, Layout, Plane};
use super::import::Importer;
use super::proto::*;
use super::region::Memory;
use crate::core::status::LinkFacts;
use crate::core::{CursorInfo, FrameUpdate, GpuMemory, HeadInfo, ImportedUpdate};
use std::collections::HashMap;
use std::sync::atomic::{fence, AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

/// Largest picture the reader accepts on either side.
const MAX_DIM: u32 = 8192;

/// Runs of dirty lines closer together than this are copied as one run, so a
/// scattered change is a few uploads, not hundreds.
const RUN_GAP: u32 = 8;

/// What a take found.
#[derive(Debug)]
pub enum Taken {
    /// The rows were copied out of a mapping.
    Frame(FrameUpdate),
    /// The rows are in memory the GPU has imported; nothing was copied.
    Imported(ImportedUpdate),
    /// Nothing the UI does not already have.
    Nothing,
    /// QEMU rewrote the layout under the copy; the head is repainted whole
    /// on the next take and the caller should ask again.
    Retry,
}

impl PartialEq for Taken {
    fn eq(&self, other: &Taken) -> bool {
        match (self, other) {
            (Taken::Frame(a), Taken::Frame(b)) => a == b,
            (Taken::Imported(a), Taken::Imported(b)) => {
                a.update == b.update
                    && Arc::ptr_eq(&a.memory, &b.memory)
                    && (a.offset, a.stride, a.bottom_up) == (b.offset, b.stride, b.bottom_up)
            }
            (Taken::Nothing, Taken::Nothing) | (Taken::Retry, Taken::Retry) => true,
            _ => false,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Stats {
    pub frames: u64,
    /// Lines handed out, copied or imported.
    pub lines: u64,
    /// Lines named for the GPU to read in place, a part of `lines`.
    pub imported_lines: u64,
    /// Pixel bytes this reader copied out of a mapping: the only place it
    /// moves pixels, so zero on a run that imports.
    pub copied_bytes: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Key {
    width: u32,
    height: u32,
    plane: Plane,
}

#[derive(Default)]
struct HeadSlot {
    /// The geometry of the last picture handed out.
    key: Option<Key>,
    /// `content_gen` at the last take.
    last_content: u64,
    /// The next take must carry the whole picture.
    full: bool,
}

struct Region {
    /// Bytes of the region the planes may use.
    len: u64,
    /// The mapping, for a region that is shared memory. A region QEMU
    /// exported as GPU memory has none: it cannot be mapped here.
    mem: Option<Arc<dyn Memory>>,
    /// The region as the GPU holds it, when it imported or wrapped it.
    gpu: Option<Arc<dyn GpuMemory>>,
}

#[derive(Default)]
struct State {
    control: Option<Control>,
    regions: HashMap<u32, Region>,
    heads: Vec<HeadSlot>,
    /// Added to every reported sequence number. A control block's own
    /// numbers start again when QEMU restarts; the engine's never go backwards.
    seq_base: u64,
    seq_seen: u64,
    stats: Stats,
    /// How the last region reached the GPU, in words, for the debug output.
    backend: Option<String>,
    /// What imports regions into the GPU, if anything does.
    importer: Option<Arc<dyn Importer>>,
    /// How the connection to QEMU stands, for saying why there is no picture.
    link: Link,
}

#[derive(Default)]
struct Link {
    connected: bool,
    connect_error: Option<String>,
    protocol_error: Option<String>,
    connects: u64,
    connect_failures: u64,
}

pub struct Display {
    state: Mutex<State>,
    visible: AtomicBool,
    /// Messages read from QEMU, over every connection.
    messages: AtomicU64,
}

impl Default for Display {
    fn default() -> Self {
        Display {
            state: Mutex::default(),
            visible: AtomicBool::new(true),
            messages: AtomicU64::new(0),
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Runs `[y0, y1)` covering `lines` (sorted), joining those at most `gap`
/// lines apart.
fn runs_of(lines: &[u32], gap: u32) -> Vec<(u32, u32)> {
    let mut runs: Vec<(u32, u32)> = Vec::new();
    for &y in lines {
        match runs.last_mut() {
            Some(r) if y < r.1 + gap => r.1 = y + 1,
            _ => runs.push((y, y + 1)),
        }
    }
    runs
}

impl State {
    fn layout_usable(&self, l: &Layout) -> bool {
        if !l.active || l.family != FAMILY_XRGB {
            return false;
        }
        let p = &l.plane0;
        if l.width == 0
            || l.height == 0
            || l.width > MAX_DIM
            || l.height > MAX_DIM
            || p.format != FMT_X8R8G8B8
            || p.width != l.width
            || p.height != l.height
            || p.stride % 4 != 0
            || (p.stride as u64) < u64::from(l.width) * 4
        {
            return false;
        }
        let Some(r) = self.regions.get(&p.region) else {
            return false;
        };
        let end = p
            .offset
            .checked_add(u64::from(p.stride) * u64::from(l.height - 1))
            .and_then(|v| v.checked_add(u64::from(l.width) * 4));
        end.is_some_and(|e| e <= r.len)
    }
}

impl Display {
    /// A new control block (a new connection): forget the old heads and
    /// regions. QEMU resends every live region.
    pub fn attach(&self, control: Control) {
        let mut s = lock(&self.state);
        s.seq_base = s.seq_seen;
        s.regions.clear();
        s.heads = (0..MAX_HEADS).map(|_| HeadSlot::default()).collect();
        s.control = Some(control);
        s.link.protocol_error = None;
    }

    /// The connection is gone. Heads stop being active; the UI keeps the last
    /// picture it has until the display comes back.
    pub fn detach(&self) {
        let mut s = lock(&self.state);
        s.control = None;
        s.regions.clear();
        s.link.connected = false;
    }

    /// A connection to the display socket is up.
    pub fn link_up(&self) {
        let mut s = lock(&self.state);
        s.link.connected = true;
        s.link.connects += 1;
        s.link.connect_error = None;
    }

    /// The display socket could not be reached.
    pub fn link_connect_failed(&self, why: String) {
        let mut s = lock(&self.state);
        s.link.connected = false;
        s.link.connect_failures += 1;
        s.link.connect_error = Some(why);
    }

    /// QEMU's messages could not be used, and the connection was dropped.
    pub fn link_failed(&self, why: String) {
        lock(&self.state).link.protocol_error = Some(why);
    }

    pub fn count_message(&self) {
        self.messages.fetch_add(1, Ordering::Relaxed);
    }

    /// Where the connection stands.
    pub fn link_facts(&self) -> LinkFacts {
        let active_heads = self.heads().iter().filter(|h| h.active).count() as u32;
        let s = lock(&self.state);
        LinkFacts {
            connected: s.link.connected,
            welcomed: s.control.is_some(),
            regions: s.regions.len() as u32,
            active_heads,
            connect_error: s.link.connect_error.clone(),
            protocol_error: s.link.protocol_error.clone(),
            connects: s.link.connects,
            connect_failures: s.link.connect_failures,
            messages: self.messages.load(Ordering::Relaxed),
        }
    }

    #[cfg(test)]
    pub fn is_attached(&self) -> bool {
        lock(&self.state).control.is_some()
    }

    pub fn add_region(&self, id: u32, mem: Arc<dyn Memory>) {
        lock(&self.state).regions.insert(
            id,
            Region {
                len: mem.len() as u64,
                mem: Some(mem),
                gpu: None,
            },
        );
    }

    /// A region the GPU reads in place: exported GPU memory that only it can
    /// reach (`mem` None), or a mapping it wraps without copying.
    pub fn add_gpu_region(
        &self,
        id: u32,
        len: u64,
        mem: Option<Arc<dyn Memory>>,
        gpu: Arc<dyn GpuMemory>,
    ) {
        lock(&self.state).regions.insert(
            id,
            Region {
                len,
                mem,
                gpu: Some(gpu),
            },
        );
    }

    /// Let `importer` take regions into the GPU instead of mapping them.
    pub fn set_importer(&self, importer: Arc<dyn Importer>) {
        lock(&self.state).importer = Some(importer);
    }

    pub fn importer(&self) -> Option<Arc<dyn Importer>> {
        lock(&self.state).importer.clone()
    }

    /// Say how regions reach the GPU (see [`Display::backend`]).
    pub fn note_backend(&self, what: String) {
        lock(&self.state).backend = Some(what);
    }

    pub fn backend(&self) -> Option<String> {
        lock(&self.state).backend.clone()
    }

    pub fn remove_region(&self, id: u32) {
        lock(&self.state).regions.remove(&id);
    }

    pub fn set_visible(&self, visible: bool) {
        self.visible.store(visible, Ordering::Release);
    }

    pub fn visible(&self) -> bool {
        self.visible.load(Ordering::Acquire)
    }

    pub fn heads(&self) -> Vec<HeadInfo> {
        let mut guard = lock(&self.state);
        let s = &mut *guard;
        let Some(c) = s.control.as_ref() else {
            return Vec::new();
        };
        let mut seen = s.seq_seen;
        let infos = (0..c.nheads())
            .map(|h| {
                let seq = s.seq_base + c.content_gen(h);
                seen = seen.max(seq);
                match c.layout(h) {
                    Ok(l) if s.layout_usable(&l) => HeadInfo {
                        width: l.width,
                        height: l.height,
                        seq,
                        active: true,
                    },
                    Ok(l) => HeadInfo {
                        width: l.width,
                        height: l.height,
                        seq,
                        active: false,
                    },
                    Err(_) => HeadInfo {
                        width: 0,
                        height: 0,
                        seq,
                        active: false,
                    },
                }
            })
            .collect();
        // What was reported counts as seen, so a restart's new block cannot
        // report lower numbers than the old one did.
        s.seq_seen = seen;
        infos
    }

    /// Ask for a `WAKE` for `head`, then check whether it changed since the
    /// last take (the arm-then-recheck the protocol asks for). True when it
    /// did, so the caller need not wait for a wake.
    pub fn arm(&self, head: usize) -> bool {
        let s = lock(&self.state);
        let Some(c) = s.control.as_ref() else {
            return false;
        };
        if head >= c.nheads() {
            return false;
        }
        c.arm(head);
        fence(Ordering::SeqCst);
        s.heads
            .get(head)
            .is_some_and(|slot| c.content_gen(head) != slot.last_content)
    }

    /// Arm every head; true when any has already changed.
    pub fn arm_all(&self) -> bool {
        let n = lock(&self.state).control.as_ref().map_or(0, |c| c.nheads());
        (0..n).fold(false, |any, h| self.arm(h) | any)
    }

    pub fn stats(&self) -> Stats {
        lock(&self.state).stats
    }

    /// Copy the lines of `head` that changed since the last take into
    /// `pixels`, run after run.
    pub fn take(&self, head: usize, pixels: &mut Vec<u8>) -> Result<Taken, ControlError> {
        let mut guard = lock(&self.state);
        let s = &mut *guard;
        let Some(c) = s.control.as_ref() else {
            return Ok(Taken::Nothing);
        };
        if head >= c.nheads() {
            return Ok(Taken::Nothing);
        }
        let layout = match c.layout(head) {
            Ok(l) => l,
            Err(ControlError::Torn) => return Ok(Taken::Retry),
            Err(e) => return Err(e),
        };
        if !s.layout_usable(&layout) {
            // Not a picture: the bits are not ours to keep either.
            c.take_dirty(head, layout.height);
            return Ok(Taken::Nothing);
        }
        let plane = layout.plane0;
        let key = Key {
            width: layout.width,
            height: layout.height,
            plane,
        };
        let cg = c.content_gen(head);
        let exact = layout.flags & HEAD_DIRTY_EXACT != 0 && layout.height <= MAX_LINES;
        // Exchange first: a line QEMU marks while we copy is for the next take.
        let dirty = c.take_dirty(head, layout.height);
        let slot = &mut s.heads[head];
        let resized = slot
            .key
            .as_ref()
            .is_none_or(|k| (k.width, k.height) != (key.width, key.height));
        let full = slot.full || slot.key != Some(key) || (!exact && cg != slot.last_content);
        let runs = if full {
            vec![(0, layout.height)]
        } else {
            runs_of(&dirty, RUN_GAP)
        };
        if runs.is_empty() {
            slot.last_content = cg;
            return Ok(Taken::Nothing);
        }
        let region = &s.regions[&plane.region];
        let gpu = region.gpu.clone();
        let mem = region.mem.clone();
        let width_bytes = layout.width as usize * 4;
        if gpu.is_none() {
            let mem = mem.expect("a region with no GPU memory is mapped");
            pixels.clear();
            pixels.reserve(runs.iter().map(|(a, b)| (b - a) as usize).sum::<usize>() * width_bytes);
            for &(y0, y1) in &runs {
                for y in y0..y1 {
                    let row = if plane.flags & PLANE_BOTTOM_UP != 0 {
                        layout.height - 1 - y
                    } else {
                        y
                    };
                    let at = plane.offset as usize + row as usize * plane.stride as usize;
                    let start = pixels.len();
                    pixels.resize(start + width_bytes, 0);
                    // SAFETY: `layout_usable` bounded the plane inside the
                    // region; QEMU may be writing it, which a copy tolerates.
                    unsafe {
                        std::ptr::copy_nonoverlapping(
                            mem.base().add(at),
                            pixels.as_mut_ptr().add(start),
                            width_bytes,
                        );
                    }
                }
            }
        } else {
            pixels.clear();
        }
        fence(Ordering::Acquire);
        if c.layout_gen(head) != layout.layout_gen {
            // The mode changed under the copy (or the read).
            slot.full = true;
            return Ok(Taken::Retry);
        }
        slot.key = Some(key);
        slot.full = false;
        slot.last_content = cg;
        let lines: u32 = runs.iter().map(|(a, b)| b - a).sum();
        s.stats.frames += 1;
        s.stats.lines += u64::from(lines);
        s.seq_seen = s.seq_seen.max(s.seq_base + cg);
        let update = FrameUpdate {
            seq: s.seq_base + cg,
            width: layout.width,
            height: layout.height,
            runs,
            resized,
        };
        Ok(match gpu {
            Some(memory) => {
                s.stats.imported_lines += u64::from(lines);
                Taken::Imported(ImportedUpdate {
                    update,
                    memory,
                    offset: plane.offset,
                    stride: plane.stride,
                    bottom_up: plane.flags & PLANE_BOTTOM_UP != 0,
                })
            }
            None => {
                s.stats.copied_bytes += pixels.len() as u64;
                Taken::Frame(update)
            }
        })
    }

    /// The guest's cursor for `head`. The image comes only when `cursor_gen`
    /// differs from `known_gen`.
    pub fn cursor(&self, head: usize, known_gen: u32) -> Option<CursorInfo> {
        let s = lock(&self.state);
        let c = s.control.as_ref()?;
        if head >= c.nheads() {
            return None;
        }
        let st = c.cursor(head);
        let mut info = CursorInfo {
            on: st.on && st.width > 0 && st.height > 0,
            x: st.x,
            y: st.y,
            hot_x: st.hot_x,
            hot_y: st.hot_y,
            width: st.width,
            height: st.height,
            gen: known_gen,
            image: None,
        };
        if st.gen == known_gen || st.gen & 1 == 1 || st.width == 0 || st.height == 0 {
            return Some(info);
        }
        let layout = c.layout(head).ok()?;
        let mem = s.regions.get(&layout.cursor_region)?.mem.as_ref()?;
        let (w, h) = (st.width as usize, st.height as usize);
        let end = layout
            .cursor_offset
            .checked_add((CURSOR_MAX as u64) * (CURSOR_MAX as u64) * 4)?;
        if end > mem.len() as u64 {
            return Some(info);
        }
        let mut image = vec![0u8; w * h * 4];
        for y in 0..h {
            let at = layout.cursor_offset as usize + y * CURSOR_MAX as usize * 4;
            // SAFETY: inside the region (checked above).
            unsafe {
                std::ptr::copy_nonoverlapping(
                    mem.base().add(at),
                    image.as_mut_ptr().add(y * w * 4),
                    w * 4,
                )
            };
        }
        fence(Ordering::Acquire);
        let again = c.cursor(head);
        if again.gen != st.gen {
            return Some(info); // changed under the copy: ask again
        }
        info.gen = st.gen;
        info.image = Some(image);
        Some(info)
    }
}

#[cfg(test)]
mod tests {
    use super::super::region::testing::{fill_lines, Block, Heap};
    use super::*;

    const W: u32 = 16;
    const STRIDE: u32 = 80; // wider than the picture, as a model may pad
    const OFF: u64 = 256;

    struct Rig {
        block: Block,
        plane: Arc<Heap>,
        d: Display,
    }

    fn rig(w: u32, h: u32) -> Rig {
        let block = Block::new(1);
        let plane = Heap::new(1 << 20);
        let d = Display::default();
        d.attach(Control::new(block.mem.clone()).unwrap());
        d.add_region(1, plane.clone());
        block.set_layout(0, w, h, 1, OFF, STRIDE, HEAD_DIRTY_EXACT);
        fill_lines(
            &plane,
            OFF as usize,
            STRIDE as usize,
            w as usize,
            0..h,
            &|y| 0x100 + y,
        );
        Rig { block, plane, d }
    }

    fn word(px: &[u8], i: usize) -> u32 {
        u32::from_le_bytes(px[i * 4..i * 4 + 4].try_into().unwrap())
    }

    fn frame(d: &Display, px: &mut Vec<u8>) -> FrameUpdate {
        match d.take(0, px).unwrap() {
            Taken::Frame(f) => f,
            other => panic!("expected a frame, got {other:?}"),
        }
    }

    #[test]
    fn the_first_take_is_the_whole_picture_and_later_ones_only_dirty_lines() {
        let r = rig(W, 40);
        let mut px = Vec::new();
        let seq0 = r.d.heads()[0].seq;
        let f = frame(&r.d, &mut px);
        assert_eq!(
            (f.width, f.height, f.runs.clone(), f.resized),
            (W, 40, vec![(0, 40)], true)
        );
        assert_eq!(
            px.len(),
            (W * 40 * 4) as usize,
            "rows are tight, the stride padding is not copied"
        );
        assert_eq!(word(&px, 0), 0x100);
        assert_eq!(word(&px, 39 * W as usize), 0x100 + 39);
        assert_eq!(f.seq, seq0);
        assert_eq!(r.d.take(0, &mut px).unwrap(), Taken::Nothing, "taken once");

        // Lines 5 and 6 change, and 30.
        fill_lines(
            &r.plane,
            OFF as usize,
            STRIDE as usize,
            W as usize,
            5..7,
            &|_| 0xAA,
        );
        fill_lines(
            &r.plane,
            OFF as usize,
            STRIDE as usize,
            W as usize,
            30..31,
            &|_| 0xBB,
        );
        r.block.mark(0, 5, 7);
        r.block.mark(0, 30, 31);
        r.block.bump(0);
        let f = frame(&r.d, &mut px);
        assert_eq!(f.runs, vec![(5, 7), (30, 31)]);
        assert!(!f.resized);
        assert_eq!(px.len(), (3 * W * 4) as usize);
        assert_eq!(word(&px, 0), 0xAA);
        assert_eq!(word(&px, 2 * W as usize), 0xBB);
        assert!(f.seq > seq0);
        assert_eq!(r.d.stats().lines, 43);
    }

    #[test]
    fn nearby_dirty_lines_are_one_run() {
        let r = rig(W, 40);
        let mut px = Vec::new();
        frame(&r.d, &mut px);
        r.block.mark(0, 2, 3);
        r.block.mark(0, 6, 7);
        r.block.mark(0, 20, 21);
        r.block.bump(0);
        let f = frame(&r.d, &mut px);
        assert_eq!(f.runs, vec![(2, 7), (20, 21)]);
        assert_eq!(f.lines(), 6);
    }

    #[test]
    fn a_change_with_no_dirty_bits_gives_nothing_when_dirty_bits_are_exact() {
        let r = rig(W, 8);
        let mut px = Vec::new();
        frame(&r.d, &mut px);
        r.block.bump(0);
        assert_eq!(r.d.take(0, &mut px).unwrap(), Taken::Nothing);
    }

    #[test]
    fn without_exact_dirty_bits_any_content_change_is_a_full_frame() {
        let r = rig(W, 8);
        r.block.put(0, R_FLAGS, 0);
        let mut px = Vec::new();
        frame(&r.d, &mut px);
        assert_eq!(r.d.take(0, &mut px).unwrap(), Taken::Nothing);
        r.block.bump(0);
        let f = frame(&r.d, &mut px);
        assert_eq!(f.runs, vec![(0, 8)]);
    }

    #[test]
    fn a_resize_is_a_whole_new_picture() {
        let r = rig(W, 40);
        let mut px = Vec::new();
        frame(&r.d, &mut px);
        // A new, smaller mode in the same region.
        r.block
            .set_layout(0, 8, 6, 1, OFF, STRIDE, HEAD_DIRTY_EXACT);
        fill_lines(&r.plane, OFF as usize, STRIDE as usize, 8, 0..6, &|y| {
            0x900 + y
        });
        assert_eq!(r.d.heads()[0].width, 8);
        let f = frame(&r.d, &mut px);
        assert_eq!(
            (f.width, f.height, f.runs, f.resized),
            (8, 6, vec![(0, 6)], true)
        );
        assert_eq!(px.len(), 8 * 6 * 4);
        assert_eq!(word(&px, 8), 0x901);
    }

    #[test]
    fn a_new_region_for_the_plane_is_a_whole_picture_once_it_is_mapped() {
        let r = rig(W, 8);
        let mut px = Vec::new();
        frame(&r.d, &mut px);
        // The mode change names region 2, which the socket has not delivered.
        r.block.set_layout(0, W, 8, 2, 0, STRIDE, HEAD_DIRTY_EXACT);
        assert!(
            !r.d.heads()[0].active,
            "composes nothing from a region it has not mapped"
        );
        assert_eq!(r.d.take(0, &mut px).unwrap(), Taken::Nothing);
        let second = Heap::new(1 << 16);
        fill_lines(&second, 0, STRIDE as usize, W as usize, 0..8, &|_| 0x77);
        r.d.add_region(2, second);
        assert!(r.d.heads()[0].active);
        let f = frame(&r.d, &mut px);
        assert_eq!(f.runs, vec![(0, 8)]);
        assert_eq!(word(&px, 0), 0x77);
        // Region 1 going away does not matter to a head that left it.
        r.d.remove_region(1);
        assert!(r.d.heads()[0].active);
        // The plane's region going away does.
        r.d.remove_region(2);
        assert!(!r.d.heads()[0].active);
    }

    #[test]
    fn a_layout_that_does_not_fit_its_region_is_not_a_picture() {
        let r = rig(W, 8);
        let mut px = Vec::new();
        for (w, h, off, stride) in [
            (W, 8, 1u64 << 20, STRIDE), // starts past the region
            (W, 8, OFF, 10),            // stride below the width
            (W, 8, OFF, 62),            // stride not a multiple of 4
            (9000, 8, OFF, 9000 * 4),   // wider than the reader takes
            (W, 100_000, OFF, STRIDE),  // taller than the region
            (0, 8, OFF, STRIDE),
        ] {
            r.block
                .set_layout(0, w, h, 1, off, stride, HEAD_DIRTY_EXACT);
            assert!(!r.d.heads()[0].active, "{w}x{h} at {off} stride {stride}");
            assert_eq!(r.d.take(0, &mut px).unwrap(), Taken::Nothing);
        }
        // A format that is not x8r8g8b8 is not this family's picture.
        r.block
            .set_layout(0, W, 8, 1, OFF, STRIDE, HEAD_DIRTY_EXACT);
        r.block.put(0, R_PLANE + P_FORMAT, 2);
        assert!(!r.d.heads()[0].active);
    }

    #[test]
    fn other_families_and_inactive_heads_are_not_shown() {
        let r = rig(W, 8);
        let mut px = Vec::new();
        r.block.put(0, R_FAMILY, 2);
        assert!(!r.d.heads()[0].active);
        assert_eq!(r.d.take(0, &mut px).unwrap(), Taken::Nothing);
        r.block.put(0, R_FAMILY, FAMILY_XRGB);
        assert!(r.d.heads()[0].active);
        r.block.deactivate(0);
        assert!(!r.d.heads()[0].active);
        assert_eq!(r.d.take(0, &mut px).unwrap(), Taken::Nothing);
        assert_eq!(
            r.d.take(5, &mut px).unwrap(),
            Taken::Nothing,
            "no such head"
        );
    }

    #[test]
    fn a_layout_rewritten_under_the_copy_is_repainted_whole() {
        let r = rig(W, 8);
        let mut px = Vec::new();
        frame(&r.d, &mut px);
        r.block.mark(0, 1, 2);
        r.block.bump(0);
        // The writer is inside the layout and stays there.
        let g = r.block.get(0, R_LAYOUT_GEN);
        r.block.put(0, R_LAYOUT_GEN, g + 1);
        assert_eq!(r.d.take(0, &mut px).unwrap(), Taken::Retry);
        r.block.put(0, R_LAYOUT_GEN, g + 2);
        // Its dirty bits were already taken, so the repaint must be whole.
        r.block.mark(0, 0, 8);
        let f = frame(&r.d, &mut px);
        assert_eq!(f.runs, vec![(0, 8)]);
    }

    /// Memory the GPU holds, as far as the engine can tell.
    struct FakeGpu;

    impl GpuMemory for FakeGpu {
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }

    #[test]
    fn a_region_the_gpu_holds_is_named_and_not_copied() {
        // QEMU's exported memory: no mapping here at all.
        let block = Block::new(1);
        let d = Display::default();
        d.attach(Control::new(block.mem.clone()).unwrap());
        let gpu: Arc<dyn GpuMemory> = Arc::new(FakeGpu);
        d.add_gpu_region(1, 1 << 16, None, gpu.clone());
        block.set_layout(0, W, 40, 1, OFF, STRIDE, HEAD_DIRTY_EXACT);
        assert!(d.heads()[0].active, "the plane fits the region's size");
        let mut px = vec![1, 2, 3];
        let Taken::Imported(f) = d.take(0, &mut px).unwrap() else {
            panic!("expected the rows to be named");
        };
        assert!(px.is_empty(), "no pixel buffer is filled");
        assert_eq!(f.update.runs, vec![(0, 40)]);
        assert_eq!((f.offset, f.stride, f.bottom_up), (OFF, STRIDE, false));
        assert!(Arc::ptr_eq(&f.memory, &gpu));
        assert_eq!(d.take(0, &mut px).unwrap(), Taken::Nothing, "taken once");

        // Two lines change: only they are named, still nothing copied.
        block.mark(0, 5, 7);
        block.bump(0);
        let Taken::Imported(f) = d.take(0, &mut px).unwrap() else {
            panic!("expected the rows to be named");
        };
        assert_eq!(
            (f.update.runs.clone(), f.update.resized),
            (vec![(5, 7)], false)
        );
        let st = d.stats();
        assert_eq!((st.lines, st.imported_lines, st.copied_bytes), (42, 42, 0));

        // A plane that does not fit the region is not a picture.
        block.set_layout(0, W, 4096, 1, OFF, STRIDE, HEAD_DIRTY_EXACT);
        assert!(!d.heads()[0].active);
    }

    #[test]
    fn a_mapping_the_gpu_wraps_is_named_too_and_its_bottom_up_flag_travels() {
        let r = rig(W, 4);
        let gpu: Arc<dyn GpuMemory> = Arc::new(FakeGpu);
        r.d.add_gpu_region(1, 1 << 20, Some(r.plane.clone()), gpu);
        r.block.put(0, R_PLANE + P_FLAGS, PLANE_BOTTOM_UP);
        r.block.mark(0, 0, 4);
        let mut px = Vec::new();
        let Taken::Imported(f) = r.d.take(0, &mut px).unwrap() else {
            panic!("expected the rows to be named");
        };
        assert!(f.bottom_up && px.is_empty());
        assert_eq!(r.d.stats().copied_bytes, 0);
    }

    #[test]
    fn bottom_up_planes_are_flipped() {
        let r = rig(W, 4);
        r.block.put(0, R_PLANE + P_FLAGS, PLANE_BOTTOM_UP);
        let mut px = Vec::new();
        r.block.mark(0, 0, 4);
        let f = frame(&r.d, &mut px);
        assert_eq!(f.runs, vec![(0, 4)]);
        // Display line 0 is stored last.
        assert_eq!(word(&px, 0), 0x100 + 3);
        assert_eq!(word(&px, 3 * W as usize), 0x100);
    }

    #[test]
    fn arming_reports_a_change_it_would_otherwise_wait_for() {
        let r = rig(W, 8);
        let mut px = Vec::new();
        assert!(r.d.arm(0), "never taken: the head is new");
        frame(&r.d, &mut px);
        assert!(!r.d.arm(0));
        assert_eq!(r.block.get(0, R_ARMED), 1);
        r.block.bump(0);
        assert!(r.d.arm_all(), "changed between the take and the arm");
        assert!(!r.d.arm(7));
    }

    #[test]
    fn a_new_control_block_after_a_restart_keeps_sequence_numbers_climbing() {
        let r = rig(W, 8);
        let mut px = Vec::new();
        for _ in 0..5 {
            r.block.bump(0);
        }
        let f = frame(&r.d, &mut px);
        let before = f.seq;
        r.d.detach();
        assert!(r.d.heads().is_empty(), "no heads while disconnected");
        assert_eq!(r.d.take(0, &mut px).unwrap(), Taken::Nothing);
        assert!(!r.d.is_attached());

        // QEMU restarts: a new block counting from 1 again, another mode.
        let block = Block::new(1);
        let plane = Heap::new(1 << 16);
        r.d.attach(Control::new(block.mem.clone()).unwrap());
        assert!(
            r.d.heads().iter().all(|h| !h.active),
            "regions come after the welcome"
        );
        r.d.add_region(1, plane.clone());
        block.set_layout(0, 12, 6, 1, 0, 48, HEAD_DIRTY_EXACT);
        fill_lines(&plane, 0, 48, 12, 0..6, &|_| 5);
        let h = r.d.heads()[0];
        assert!(h.active && h.seq > before, "{} past {before}", h.seq);
        let f = frame(&r.d, &mut px);
        assert_eq!((f.width, f.height, f.resized), (12, 6, true));
        assert!(f.seq > before);
    }

    #[test]
    fn the_cursor_image_comes_once_per_generation() {
        let r = rig(W, 8);
        let img = Heap::new(1 << 16);
        r.d.add_region(3, img.clone());
        let o = r.block.head(0);
        r.block.mem.put32(o + R_CURSOR_IMAGE, 3);
        r.block
            .mem
            .u64_at(o + R_CURSOR_IMAGE + 8)
            .store(512, std::sync::atomic::Ordering::SeqCst);
        // 3 x 2 cursor, rows 256 bytes apart.
        for y in 0..2u32 {
            let row: Vec<u8> = (0..3u32)
                .flat_map(|x| (0xFF00_0000 | (y * 16 + x)).to_le_bytes())
                .collect();
            img.write(512 + y as usize * 256, &row);
        }
        r.block.put(0, R_CURSOR_ON, 1);
        r.block.put(0, R_CURSOR_X, 10);
        r.block.put(0, R_CURSOR_Y, 20);
        r.block.put(0, R_CURSOR_HOT_X, 1);
        r.block.put(0, R_CURSOR_W, 3);
        r.block.put(0, R_CURSOR_H, 2);
        r.block.put(0, R_CURSOR_GEN, 4);
        let c = r.d.cursor(0, 0).unwrap();
        assert_eq!(
            (c.on, c.x, c.y, c.hot_x, c.width, c.height, c.gen),
            (true, 10, 20, 1, 3, 2, 4)
        );
        let image = c.image.unwrap();
        assert_eq!(image.len(), 3 * 2 * 4, "tight rows, not the 256-byte ones");
        assert_eq!(word(&image, 3), 0xFF00_0010);
        // Same generation: position only.
        let c = r.d.cursor(0, 4).unwrap();
        assert!(c.image.is_none());
        assert_eq!(c.gen, 4);
        // Moves are not a new generation.
        r.block.put(0, R_CURSOR_X, 11);
        assert_eq!(r.d.cursor(0, 4).unwrap().x, 11);
        // Odd means QEMU is writing the image: no image, the old generation.
        r.block.put(0, R_CURSOR_GEN, 5);
        let c = r.d.cursor(0, 4).unwrap();
        assert!(c.image.is_none() && c.gen == 4);
        // A guest cursor of size 0 is not shown.
        r.block.put(0, R_CURSOR_GEN, 6);
        r.block.put(0, R_CURSOR_W, 0);
        assert!(!r.d.cursor(0, 6).unwrap().on);
        assert!(r.d.cursor(3, 0).is_none());
    }
}
