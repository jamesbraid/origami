//! A version 4 viewer as an engine's frame source: heads, takes, the cursor,
//! wake-ups and input. One thread runs [`Viewer::run`]; any thread may take
//! frames and send input. A GPU frame carries its lease as `ImportedUpdate::hold`.

use super::regions::{Mapping, StateView};
use super::session::{
    Event, FrameLease, HelloParams, Previous, Session, SessionError, SessionHandle,
};
use super::sys::UnixStream;
use super::wire;
use crate::input::{coalesce, evdev};
use crate::{Importer, Memory, Stats, Taken};
use origami_engine::status::LinkFacts;
use origami_engine::{
    CursorInfo, FrameUpdate, GpuMemory, HeadInfo, ImportedUpdate, InputEvent, InputSink,
    PointerButton, Result,
};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// A slot's mapping as the importers take it. Windows opens the section instead.
#[cfg_attr(windows, allow(dead_code))]
struct SlotMemory(Arc<Mapping>);

// SAFETY: the mapping is page aligned, valid while the Arc lives, and only
// read through a lease.
unsafe impl Memory for SlotMemory {
    fn base(&self) -> *mut u8 {
        self.0.as_ptr()
    }
    fn len(&self) -> usize {
        self.0.len()
    }
}

struct Slot {
    gpu: Option<Arc<dyn GpuMemory>>,
}

#[derive(Default)]
struct CursorView {
    image: Option<wire::CursorImage>,
    /// The `cursor_seq` the producer was last asked to differ from.
    asked: Option<u32>,
}

#[derive(Default)]
struct HeadView {
    info: Option<wire::Head>,
    /// The newest frame serial received.
    serial: u64,
    pending: Option<FrameLease>,
    /// The size of the last picture handed out, and the mode it was in.
    handed: Option<(u32, u32, u32)>,
    /// The next frame must carry every line: one was dropped unseen.
    need_full: bool,
    embedded: bool,
    cursor: CursorView,
}

#[derive(Default)]
struct State {
    heads: Vec<HeadView>,
    slots: HashMap<u32, Slot>,
    stats: Stats,
    backend: Option<String>,
    /// Added to every serial, so the count never goes back when the producer restarts.
    seq_base: u64,
    seq_seen: u64,
    link: LinkFacts,
    /// The extensions the producer accepted, with their message indexes.
    accepted: Vec<(String, u8)>,
    /// The id the producer gave this viewer.
    viewer_id: u32,
}

pub struct Viewer {
    state: Mutex<State>,
    visible: AtomicBool,
    importer: Mutex<Option<Arc<dyn Importer>>>,
    session: Mutex<Option<SessionHandle>>,
    state_view: Mutex<Option<Arc<StateView>>>,
    hello: HelloParams,
    /// How the last connection ended, for the next `HELLO` on Windows.
    prev: Mutex<Option<Previous>>,
}

impl Default for Viewer {
    fn default() -> Self {
        Viewer::new()
    }
}

impl Viewer {
    pub fn new() -> Viewer {
        let hello = HelloParams {
            name: "origami".into(),
            features: wire::feat::INPUT | wire::feat::CURSOR_OVERLAY,
            credit: 1,
            offers: vec![wire::Offer {
                name: STEERING.into(),
                version_min: 1,
                version_max: 1,
                payload: vec![],
            }],
            ..Default::default()
        };
        Viewer {
            state: Mutex::new(State::default()),
            visible: AtomicBool::new(true),
            importer: Mutex::new(None),
            session: Mutex::new(None),
            state_view: Mutex::new(None),
            hello,
            prev: Mutex::new(None),
        }
    }

    /// Add a feature bit and an extension offer to what `HELLO` says.
    pub fn offer(mut self, features: u32, offer: wire::Offer) -> Viewer {
        self.hello.features |= features;
        self.hello.offers.push(offer);
        self
    }

    /// Take slots into the GPU with `importer` where it can.
    pub fn set_importer(&self, importer: Arc<dyn Importer>) {
        *lock(&self.importer) = Some(importer);
    }

    pub fn visible(&self) -> bool {
        self.visible.load(Ordering::Acquire)
    }

    /// Show or hide; hidden, nothing new is asked for. True when showing
    /// finds a frame waiting, so the caller should wake its UI.
    pub fn set_visible(&self, visible: bool) -> bool {
        if self.visible.swap(visible, Ordering::AcqRel) == visible || !visible {
            return false;
        }
        let Some(handle) = lock(&self.session).clone() else {
            return false;
        };
        let mut st = lock(&self.state);
        let mut waiting = false;
        for (i, h) in st.heads.iter_mut().enumerate() {
            waiting |= h.pending.is_some();
            if h.info.is_some() {
                let _ = handle.request(i as u32, false);
                if !h.embedded {
                    let _ = handle.request_cursor(i as u32, h.cursor.asked.unwrap_or(0));
                }
            }
        }
        waiting
    }

    pub fn heads(&self) -> Vec<HeadInfo> {
        let st = lock(&self.state);
        st.heads
            .iter()
            .map(|h| {
                let (width, height, shown) = h.info.as_ref().map_or((0, 0, false), |i| {
                    (i.width, i.height, i.flags & wire::HEAD_PRESENT != 0)
                });
                HeadInfo {
                    width,
                    height,
                    seq: if h.serial == 0 {
                        0
                    } else {
                        st.seq_base + h.serial
                    },
                    active: shown && width > 0 && height > 0 && h.serial > 0,
                }
            })
            .collect()
    }

    pub fn stats(&self) -> Stats {
        lock(&self.state).stats
    }

    pub fn backend(&self) -> Option<String> {
        lock(&self.state).backend.clone()
    }

    pub fn link_facts(&self) -> LinkFacts {
        let st = lock(&self.state);
        let mut l = st.link.clone();
        l.regions = st.slots.len() as u32;
        l.active_heads = st
            .heads
            .iter()
            .filter(|h| h.info.as_ref().is_some_and(|i| i.width > 0) && h.serial > 0)
            .count() as u32;
        l
    }

    /// Note a failed attempt to reach the socket.
    pub fn connect_failed(&self, why: String) {
        let mut st = lock(&self.state);
        st.link.connect_failures += 1;
        st.link.connect_error = Some(why);
    }

    /// The head keys and buttons go to: the first one with a picture.
    fn first_head(&self) -> u32 {
        let st = lock(&self.state);
        st.heads
            .iter()
            .position(|h| {
                h.info
                    .as_ref()
                    .is_some_and(|i| i.flags & wire::HEAD_PRESENT != 0)
            })
            .unwrap_or(0) as u32
    }

    /// The pointer mode of `head`, if it has been described.
    fn pointer_mode(&self, head: u32) -> Option<u32> {
        let st = lock(&self.state);
        Some(st.heads.get(head as usize)?.info.as_ref()?.pointer_mode)
    }

    /// Connect the session on `stream`, serve it until it ends, and say why.
    /// `wake` is called, from this thread, when a head has something new.
    pub fn run(
        &self,
        stream: UnixStream,
        wake: &dyn Fn(),
    ) -> std::result::Result<(), SessionError> {
        let mut hello = self.hello.clone();
        if lock(&self.importer)
            .as_ref()
            .is_some_and(|i| i.shm_needs_write())
        {
            hello.features |= wire::feat::RW_PIXELS;
        }
        if cfg!(windows) {
            hello.prev = *lock(&self.prev);
        }
        let mut session = Session::connect(stream, &hello)?;
        let handle = session.handle();
        *lock(&self.state_view) = Some(session.state.clone());
        *lock(&self.session) = Some(handle.clone());
        {
            let mut st = lock(&self.state);
            st.link.connected = true;
            st.link.welcomed = true;
            st.link.connects += 1;
            st.link.protocol_error = None;
            st.viewer_id = session.welcome.viewer_id;
            st.accepted = session
                .welcome
                .accepts
                .iter()
                .map(|a| (a.name.clone(), a.index))
                .collect();
            for n in 0..handle.head_count() {
                if let Some(info) = handle.head(n as u32) {
                    // No frame can be pending before the first head is described.
                    let _ = self.apply_head(&mut st, info, &handle);
                }
            }
        }
        let result = loop {
            match session.recv(None) {
                Ok(Some(event)) => {
                    lock(&self.state).link.messages += 1;
                    self.event(event, &handle, wake);
                }
                Ok(None) => {}
                Err(SessionError::Closed) => break Ok(()),
                Err(e) => break Err(e),
            }
        };
        *lock(&self.prev) = Some(Previous {
            viewer_id: session.welcome.viewer_id,
            session_id: session.welcome.session_id,
            handles_taken: session.handles_taken(),
        });
        *lock(&self.session) = None;
        *lock(&self.state_view) = None;
        // Leases end with a write to the socket, so they end outside the lock.
        let dead: Vec<_> = {
            let mut st = lock(&self.state);
            st.link.connected = false;
            st.link.welcomed = false;
            if let Err(e) = &result {
                st.link.protocol_error = Some(e.to_string());
            }
            st.seq_base += st.seq_seen;
            st.seq_seen = 0;
            st.slots.clear();
            st.heads
                .iter_mut()
                .filter_map(|h| std::mem::take(h).pending)
                .collect()
        };
        drop(dead);
        wake();
        result
    }

    /// Record a head's description. Returns a frame of the old mode, to be
    /// dropped once the lock is released.
    fn apply_head(
        &self,
        st: &mut State,
        info: wire::Head,
        handle: &SessionHandle,
    ) -> Option<FrameLease> {
        let n = info.head as usize;
        if st.heads.len() <= n {
            st.heads.resize_with(n + 1, HeadView::default);
        }
        let h = &mut st.heads[n];
        let changed = h
            .info
            .as_ref()
            .is_none_or(|old| old.mode_serial != info.mode_serial);
        let mut old = None;
        if changed {
            // A frame in the old mode is the wrong size now.
            old = h.pending.take();
            h.need_full = true;
            h.embedded = false;
        }
        let present = info.flags & wire::HEAD_PRESENT != 0;
        let head = info.head;
        h.info = Some(info);
        if present && self.visible() {
            let _ = handle.request(head, false);
        }
        old
    }

    fn event(&self, event: Event, handle: &SessionHandle, wake: &dyn Fn()) {
        match event {
            Event::Frame(lease) => self.on_frame(lease, handle, wake),
            Event::Head(info) => {
                let old = self.apply_head(&mut lock(&self.state), info, handle);
                drop(old);
            }
            Event::CursorImage(img) => {
                let mut st = lock(&self.state);
                let shown = st.heads.get_mut(img.head as usize).is_some_and(|h| {
                    h.cursor.image = Some(img);
                    !h.embedded
                });
                drop(st);
                if shown && self.visible() {
                    wake();
                }
            }
            Event::CursorNotify { head, .. } => {
                let mut st = lock(&self.state);
                if let Some(h) = st.heads.get_mut(head as usize) {
                    // The notice is spent; the next read asks again.
                    h.cursor.asked = None;
                }
                drop(st);
                if self.visible() {
                    wake();
                }
            }
            Event::RegionGone(id) => {
                lock(&self.state).slots.remove(&id);
            }
            _ => {}
        }
    }

    /// A frame arrived: hold the newest, ask for the next, wake the UI.
    fn on_frame(&self, lease: FrameLease, handle: &SessionHandle, wake: &dyn Fn()) {
        let head = lease.frame.head;
        // Leases end with a write to the socket, so they end after the lock does.
        let mut dead = Vec::new();
        let mut st = lock(&self.state);
        if !st.slots.contains_key(&lease.frame.region) {
            let slot = self.import(&mut st, &lease);
            st.slots.insert(lease.frame.region, slot);
        }
        st.seq_seen = st.seq_seen.max(lease.frame.serial);
        let Some(h) = st.heads.get_mut(head as usize) else {
            return;
        };
        if lease.stale_mode || !matches!(lease.fourcc, wire::FOURCC_XR24 | wire::FOURCC_XB24) {
            // Its damage is lost with it.
            h.need_full = true;
            dead.push(lease);
            if self.visible() {
                let _ = handle.request(head, false);
            }
            return;
        }
        h.serial = h.serial.max(lease.frame.serial);
        let embedded = lease.frame.frame_flags & wire::FRAME_CURSOR_EMBEDDED != 0;
        let first = h.pending.is_none();
        if let Some(old) = h.pending.replace(lease) {
            h.need_full = true;
            dead.push(old);
        }
        let visible = self.visible();
        if visible {
            let _ = handle.request(head, false);
            if !embedded && h.cursor.asked.is_none() {
                h.cursor.asked = Some(0);
                let _ = handle.request_cursor(head, 0);
            }
        }
        h.embedded = embedded;
        drop(st);
        if visible && first {
            wake();
        }
    }

    fn import(&self, st: &mut State, lease: &FrameLease) -> Slot {
        let Some(importer) = lock(&self.importer).clone() else {
            st.backend = Some("copied: the viewer has no GPU importer".into());
            return Slot { gpu: None };
        };
        #[cfg(windows)]
        let gpu = {
            use std::os::windows::io::AsRawHandle;
            let size = lease.slot.len() as u64;
            let region = crate::proto::Region {
                id: lease.frame.region,
                kind: 0,
                backend: 0,
                memory_type: 0,
                size,
                alloc_size: size,
                flags: 0,
                handle: 0,
            };
            let section = lease.slot.section().as_raw_handle() as usize as u64;
            importer.open_section(&region, &crate::Handle::Value(section))
        };
        #[cfg(not(windows))]
        let gpu = {
            let memory: Arc<dyn Memory> = Arc::new(SlotMemory(lease.slot.clone()));
            importer.wrap_shm(lease.slot.len() as u64, &memory)
        };
        let backend = match &gpu {
            Ok(_) => format!("imported, zero-copy ({})", importer.name()),
            Err(why) => format!("copied: {why}"),
        };
        if st.backend.as_ref() != Some(&backend) {
            eprintln!("origami: display: {backend}");
            st.backend = Some(backend);
        }
        Slot { gpu: gpu.ok() }
    }

    /// The rows of `head` that changed since the last take, copied into
    /// `pixels`, or named in GPU memory when the slot is imported.
    pub fn take(&self, head: usize, pixels: &mut Vec<u8>) -> Taken {
        // A lease ends with a write to the socket, so it ends after the lock does.
        let (taken, spent) = self.take_locked(head, pixels);
        drop(spent);
        taken
    }

    fn take_locked(&self, head: usize, pixels: &mut Vec<u8>) -> (Taken, Option<FrameLease>) {
        let mut st = lock(&self.state);
        let st = &mut *st;
        let Some(h) = st.heads.get_mut(head) else {
            return (Taken::Nothing, None);
        };
        let Some(lease) = h.pending.take() else {
            return (Taken::Nothing, None);
        };
        let (w, ht) = (lease.width, lease.height);
        let key = (w, ht, lease.frame.mode_serial);
        let full = h.need_full || h.handed != Some(key);
        let runs = if full {
            vec![(0, ht)]
        } else {
            runs_of(&lease.frame.damage, ht)
        };
        h.need_full = false;
        if runs.is_empty() {
            return (Taken::Nothing, Some(lease));
        }
        let resized = h.handed.is_none_or(|(hw, hh, _)| (hw, hh) != (w, ht));
        h.handed = Some(key);
        let lines: u32 = runs.iter().map(|(a, b)| b - a).sum();
        let update = FrameUpdate {
            seq: st.seq_base + lease.frame.serial,
            width: w,
            height: ht,
            runs,
            resized,
        };
        st.stats.frames += 1;
        st.stats.lines += u64::from(lines);
        if let Some(memory) = st
            .slots
            .get(&lease.frame.region)
            .and_then(|s| s.gpu.clone())
        {
            st.stats.imported_lines += u64::from(lines);
            pixels.clear();
            let stride = lease.stride;
            return (
                Taken::Imported(ImportedUpdate {
                    update,
                    memory,
                    offset: 0,
                    stride,
                    bottom_up: false,
                    swap_rb: lease.fourcc == wire::FOURCC_XB24,
                    hold: Some(Arc::new(lease)),
                }),
                None,
            );
        }
        pixels.clear();
        let stride = lease.stride as usize;
        let row = w as usize * 4;
        let src = lease.pixels();
        for &(y0, y1) in &update.runs {
            for y in y0 as usize..y1 as usize {
                let at = pixels.len();
                pixels.extend_from_slice(&src[y * stride..y * stride + row]);
                if lease.fourcc == wire::FOURCC_XB24 {
                    // Rows go out as blue, green, red.
                    pixels[at..]
                        .as_chunks_mut::<4>()
                        .0
                        .iter_mut()
                        .for_each(|p| p.swap(0, 2));
                }
            }
        }
        st.stats.copied_bytes += pixels.len() as u64;
        (Taken::Frame(update), Some(lease))
    }

    /// The guest's cursor when the picture does not carry it.
    pub fn cursor(&self, head: usize, known_gen: u32) -> Option<CursorInfo> {
        let view = lock(&self.state_view).clone()?;
        let handle = lock(&self.session).clone();
        let mut st = lock(&self.state);
        let h = st.heads.get_mut(head)?;
        if h.embedded || h.info.is_none() {
            return None;
        }
        let rec = view.cursor(head as u32)?;
        if h.cursor.asked != Some(rec.seq) && self.visible() {
            h.cursor.asked = Some(rec.seq);
            if let Some(handle) = handle {
                let _ = handle.request_cursor(head as u32, rec.seq);
            }
        }
        // The last image stays up until the one for `rec.image_serial` arrives.
        let image = h
            .cursor
            .image
            .as_ref()
            .filter(|i| i.fourcc == wire::FOURCC_AR24);
        let mut info = CursorInfo {
            on: rec.flags & 1 != 0 && image.is_some(),
            x: rec.x,
            y: rec.y,
            hot_x: 0,
            hot_y: 0,
            width: 0,
            height: 0,
            gen: image.map_or(rec.image_serial, |i| i.image_serial),
            image: None,
        };
        if let Some(img) = image {
            info.hot_x = img.hot_x;
            info.hot_y = img.hot_y;
            info.width = img.width;
            info.height = img.height;
            if img.image_serial != known_gen {
                let (stride, row) = (img.stride as usize, img.width as usize * 4);
                let mut tight = Vec::with_capacity(row * img.height as usize);
                for y in 0..img.height as usize {
                    tight.extend_from_slice(img.pixels.get(y * stride..y * stride + row)?);
                }
                info.image = Some(tight);
            }
        }
        Some(info)
    }

    /// The message index the producer gave extension `name`, if it accepted it.
    pub fn extension(&self, name: &str) -> Option<u8> {
        let st = lock(&self.state);
        st.accepted.iter().find(|a| a.0 == name).map(|a| a.1)
    }

    /// The id the producer gave this viewer, for a test that addresses it.
    pub fn viewer_id(&self) -> u32 {
        lock(&self.state).viewer_id
    }

    /// Send an encoded message (an extension's) on the session.
    pub fn send_raw(&self, message: &[u8]) -> Result<()> {
        let handle = lock(&self.session)
            .clone()
            .ok_or("the machine's display is not connected")?;
        handle.send(message)?;
        Ok(())
    }

    /// End the session from another thread; `run` then returns.
    pub fn close(&self) {
        let handle = lock(&self.session).clone();
        if let Some(h) = handle {
            h.close();
        }
    }

    /// Send typed input as one report.
    pub fn send_input(&self, events: &[InputEvent]) -> Result<()> {
        let mut out = Vec::with_capacity(events.len() * 24 + 16);
        let head = self.first_head();
        for e in coalesce(events) {
            match e {
                InputEvent::Key { key, down } => {
                    out.extend(wire::key(head, evdev(key), u32::from(down)));
                }
                InputEvent::PointerMove { dx, dy } => out.extend(wire::rel(head, dx, dy)),
                InputEvent::PointerAt { head, x, y } => {
                    // A relative pointer without steering has no place to go to.
                    if matches!(
                        self.pointer_mode(head),
                        Some(wire::POINTER_ABSOLUTE | wire::POINTER_STEERED)
                    ) {
                        out.extend(wire::abs(head, x, y));
                    }
                }
                InputEvent::Button { button, down } => {
                    let n = match button {
                        PointerButton::Left => 0,
                        PointerButton::Middle => 1,
                        PointerButton::Right => 2,
                    };
                    out.extend(wire::button(head, n, u32::from(down)));
                }
                InputEvent::Wheel { notches } => {
                    let n = if notches > 0 { 3 } else { 4 };
                    // The producer takes at most 64 notches in 10 ms.
                    for _ in 0..notches.unsigned_abs().min(64) {
                        out.extend(wire::button(head, n, 1));
                        out.extend(wire::button(head, n, 0));
                    }
                }
            }
        }
        if out.is_empty() {
            return Ok(());
        }
        out.extend(wire::sync(0, 0));
        let handle = lock(&self.session)
            .clone()
            .ok_or("the machine's display is not connected")?;
        handle.send(&out)?;
        Ok(())
    }
}

/// Sends a viewer's input.
pub struct ViewerInput(pub Arc<Viewer>);

impl InputSink for ViewerInput {
    fn send(&mut self, events: &[InputEvent]) -> Result<()> {
        self.0.send_input(events)
    }
}

const STEERING: &str = "qemu.pointer-steering";

/// The changed lines as runs `y0..y1`. Only damaged lines are valid in a slot, so touching runs join and nothing else does.
fn runs_of(damage: &wire::Damage, height: u32) -> Vec<(u32, u32)> {
    let mut runs: Vec<(u32, u32)> = Vec::new();
    let mut add = |y0: u32, y1: u32| {
        let (y0, y1) = (y0.min(height), y1.min(height));
        if y0 >= y1 {
            return;
        }
        match runs.last_mut() {
            Some(last) if y0 <= last.1 => last.1 = last.1.max(y1),
            _ => runs.push((y0, y1)),
        }
    };
    match damage {
        wire::Damage::Whole => add(0, height),
        wire::Damage::Rects(rects) => {
            let mut sorted: Vec<_> = rects
                .iter()
                .map(|r| (r[1], r[1].saturating_add(r[3])))
                .collect();
            sorted.sort_unstable();
            for (a, b) in sorted {
                add(a, b);
            }
        }
        wire::Damage::Lines(words) => {
            let mut y = 0u32;
            while y < height {
                let word = words.get(y as usize / 64).copied().unwrap_or(0) >> (y % 64);
                if word == 0 {
                    y = (y / 64 + 1) * 64;
                    continue;
                }
                y += word.trailing_zeros();
                let start = y;
                while y < height
                    && words
                        .get(y as usize / 64)
                        .is_some_and(|w| w >> (y % 64) & 1 != 0)
                {
                    y += 1;
                }
                add(start, y);
            }
        }
    }
    runs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_touching_runs_join_and_the_last_clips_to_the_height() {
        let rects = wire::Damage::Rects(vec![
            [0, 40, 8, 2],
            [0, 4, 8, 3],
            [0, 7, 8, 1],
            [0, 9, 8, 1],
            [0, 90, 8, 40],
        ]);
        assert_eq!(
            runs_of(&rects, 100),
            vec![(4, 8), (9, 10), (40, 42), (90, 100)]
        );
        assert_eq!(runs_of(&wire::Damage::Whole, 7), vec![(0, 7)]);
        let mut words = vec![0u64; 2];
        words[0] = 1 << 3 | 1 << 4 | 1 << 63;
        words[1] = 1 << 5;
        assert_eq!(
            runs_of(&wire::Damage::Lines(words), 200),
            vec![(3, 5), (63, 64), (69, 70)]
        );
    }
}
