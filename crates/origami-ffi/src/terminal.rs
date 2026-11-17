//! A machine's console as a terminal, over `origami-terminal`'s VT model, so
//! the apps need no emulator of their own. A pump thread feeds it the guest's
//! output; the app asks what changed and redraws those rows.

use crate::control::{CoreError, OrigamiMachine};
use origami_core::facade as f;
use origami_terminal::{key_bytes as term_key_bytes, Colour, Key, KeyModes, Terminal};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

type Res<T> = Result<T, CoreError>;

/// Largest grid, so a resize cannot ask for unbounded memory. With the
/// terminal's 5000 history lines this also bounds the scrollback.
const MAX_ROWS: u16 = 256;
const MAX_COLS: u16 = 512;
const POLL: Duration = Duration::from_millis(15);
const CHUNK: usize = 1 << 16;
/// Sends waiting for the console's writer before a caller is told it is busy.
const QUEUE: usize = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, uniffi::Enum)]
pub enum OrigamiTerminalColour {
    Default,
    Indexed { index: u8 },
    Rgb { r: u8, g: u8, b: u8 },
}

impl From<Colour> for OrigamiTerminalColour {
    fn from(c: Colour) -> Self {
        match c {
            Colour::Default => Self::Default,
            Colour::Idx(index) => Self::Indexed { index },
            Colour::Rgb(r, g, b) => Self::Rgb { r, g, b },
        }
    }
}

/// One cell. `text` is empty for a blank cell and for the second half of a
/// wide character (`continuation`).
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct OrigamiTerminalCell {
    pub text: String,
    pub fg: OrigamiTerminalColour,
    pub bg: OrigamiTerminalColour,
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: bool,
    pub inverse: bool,
    pub wide: bool,
    pub continuation: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Record)]
pub struct OrigamiTerminalSize {
    pub rows: u16,
    pub cols: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Record)]
pub struct OrigamiTerminalCursor {
    pub row: u16,
    pub col: u16,
    pub visible: bool,
}

/// What to redraw since a sequence number. `full` means everything (the
/// first call, a resize or a scroll); otherwise only `rows`. Pass
/// `sequence` back next time.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct OrigamiTerminalDamage {
    pub sequence: u64,
    pub full: bool,
    pub rows: Vec<u16>,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum OrigamiTerminalKey {
    /// Typed text. With `ctrl` or `alt` only a single character counts.
    Text {
        text: String,
    },
    Enter,
    Backspace,
    Tab,
    BackTab,
    Escape,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
    Delete,
    /// F1 to F12.
    Function {
        number: u8,
    },
}

struct State {
    term: Terminal,
    /// The console offset the next pump read starts at.
    next: u64,
    generation: Option<u64>,
    seq: u64,
    /// The sequence at which everything last had to be redrawn.
    full_seq: u64,
    hashes: Vec<u64>,
    row_seq: Vec<u64>,
}

impl State {
    fn new(rows: u16, cols: u16) -> State {
        let mut s = State {
            term: Terminal::new(rows.clamp(1, MAX_ROWS), cols.clamp(1, MAX_COLS)),
            next: 0,
            generation: None,
            seq: 0,
            full_seq: 0,
            hashes: Vec::new(),
            row_seq: Vec::new(),
        };
        s.refresh(true);
        s
    }

    /// Blank screen and history; the sequence keeps counting.
    fn reset(&mut self) {
        let (rows, cols) = self.term.size();
        self.term = Terminal::new(rows, cols);
        self.refresh(true);
    }

    #[cfg(test)]
    fn feed(&mut self, bytes: &[u8]) {
        self.term.feed(bytes);
        self.refresh(false);
    }

    /// Feed output at console offset `offset`; the bytes to answer it with.
    fn feed_at(&mut self, offset: u64, bytes: &[u8]) -> Vec<u8> {
        let replies = self.term.feed_at(offset, bytes);
        self.refresh(false);
        replies
    }

    fn resize(&mut self, rows: u16, cols: u16) {
        self.term
            .resize(rows.clamp(1, MAX_ROWS), cols.clamp(1, MAX_COLS));
        self.refresh(true);
    }

    fn scroll(&mut self, lines: i32) {
        self.term.scroll(lines as isize);
        self.refresh(true);
    }

    /// Stamp the rows whose content (or cursor) differs from last time.
    fn refresh(&mut self, full: bool) {
        let (rows, cols) = self.term.size();
        let full = full || self.hashes.len() != usize::from(rows);
        let stamp = self.seq + 1;
        let mut dirty = full;
        if full {
            self.hashes = vec![0; usize::from(rows)];
            self.row_seq = vec![0; usize::from(rows)];
            self.full_seq = stamp;
        }
        let cursor = self.term.cursor_visible().then(|| self.term.cursor());
        for r in 0..rows {
            let mut h = DefaultHasher::new();
            for c in 0..cols {
                if let Some(cell) = self.term.cell(r, c) {
                    cell.hash(&mut h);
                }
            }
            cursor.filter(|p| p.0 == r).hash(&mut h);
            let (h, i) = (h.finish(), usize::from(r));
            if self.hashes[i] != h || full {
                self.hashes[i] = h;
                self.row_seq[i] = stamp;
                dirty = true;
            }
        }
        if dirty {
            self.seq = stamp;
        }
    }

    fn row(&self, row: u16) -> Vec<OrigamiTerminalCell> {
        (0..self.term.size().1)
            .map(|c| match self.term.cell(row, c) {
                Some(cell) => OrigamiTerminalCell {
                    text: cell.text,
                    fg: cell.fg.into(),
                    bg: cell.bg.into(),
                    bold: cell.bold,
                    dim: cell.dim,
                    italic: cell.italic,
                    underline: cell.underline,
                    inverse: cell.inverse,
                    wide: cell.wide,
                    continuation: cell.continuation,
                },
                None => OrigamiTerminalCell {
                    text: String::new(),
                    fg: OrigamiTerminalColour::Default,
                    bg: OrigamiTerminalColour::Default,
                    bold: false,
                    dim: false,
                    italic: false,
                    underline: false,
                    inverse: false,
                    wide: false,
                    continuation: false,
                },
            })
            .collect()
    }

    /// History row `index`, 0 being the oldest: scroll it to the top of the
    /// view, read it, put the view back.
    fn history_row(&mut self, index: u32) -> Option<Vec<OrigamiTerminalCell>> {
        let history = self.term.history();
        let index = usize::try_from(index).ok().filter(|i| *i < history)?;
        let was = self.term.scrolled();
        self.term.scroll((history - index) as isize - was as isize);
        let row = self.row(0);
        self.term
            .scroll(was as isize - self.term.scrolled() as isize);
        Some(row)
    }
}

fn key_bytes(
    key: &OrigamiTerminalKey,
    shift: bool,
    ctrl: bool,
    alt: bool,
    modes: KeyModes,
) -> Option<Vec<u8>> {
    use OrigamiTerminalKey as K;
    let key = match key {
        K::Text { text } => {
            let mut chars = text.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) => Key::Char(c),
                // Composed or multi-character input goes through as typed.
                (Some(_), Some(_)) if !ctrl && !alt => return Some(text.clone().into_bytes()),
                _ => return None,
            }
        }
        K::Enter => Key::Enter,
        K::Backspace => Key::Backspace,
        K::Tab => Key::Tab,
        K::BackTab => Key::BackTab,
        K::Escape => Key::Escape,
        K::Up => Key::Up,
        K::Down => Key::Down,
        K::Left => Key::Left,
        K::Right => Key::Right,
        K::Home => Key::Home,
        K::End => Key::End,
        K::PageUp => Key::PageUp,
        K::PageDown => Key::PageDown,
        K::Insert => Key::Insert,
        K::Delete => Key::Delete,
        K::Function { number } => Key::F(*number),
    };
    term_key_bytes(key, shift, ctrl, alt, modes)
}

struct Shared {
    machine: Arc<OrigamiMachine>,
    /// Input for the console's writer.
    tx: SyncSender<Vec<u8>>,
    state: Mutex<State>,
    closed: AtomicBool,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn pump(&self) {
        while !self.closed.load(Ordering::Relaxed) {
            while !self.closed.load(Ordering::Relaxed) {
                let from = self.lock().next;
                let chunk = self.machine.console_read(from, CHUNK as u32);
                let mut s = self.lock();
                // A restart rewinds the offsets, and lost bytes leave the
                // screen wrong: start over in both cases.
                if s.generation.is_some_and(|g| g != chunk.generation) {
                    s.reset();
                    s.next = 0;
                    s.generation = Some(chunk.generation);
                    continue;
                }
                s.generation = Some(chunk.generation);
                if chunk.dropped > 0 {
                    s.reset();
                }
                s.next = chunk.next;
                // Answers to the guest's queries (device attributes and the like).
                let start = chunk.next - chunk.bytes.len() as u64;
                let replies = s.feed_at(start, &chunk.bytes);
                if !replies.is_empty() {
                    let _ = self.tx.try_send(replies);
                }
                if chunk.bytes.len() < CHUNK {
                    break;
                }
                drop(s);
                std::thread::yield_now();
            }
            std::thread::sleep(POLL);
        }
    }
}

/// Writes queued input to the console, so a stalled guest blocks this
/// thread and no caller. Ends when the terminal is dropped.
fn send_loop(machine: Arc<OrigamiMachine>, queue: Receiver<Vec<u8>>) {
    for bytes in queue {
        let _ = machine.console_send(bytes);
    }
}

/// A terminal on a machine's console. The pump runs on its own thread, so
/// every call is quick and safe from a UI thread.
#[derive(uniffi::Object)]
pub struct OrigamiTerminal(Arc<Shared>, SyncSender<Vec<u8>>);

#[uniffi::export]
impl OrigamiTerminal {
    /// `rows` and `cols` are clamped to 1..=256 and 1..=512. Output the
    /// console already holds is replayed into it.
    #[uniffi::constructor]
    pub fn new(machine: Arc<OrigamiMachine>, rows: u16, cols: u16) -> Res<Arc<OrigamiTerminal>> {
        let (tx, rx) = sync_channel(QUEUE);
        let mut state = State::new(rows, cols);
        // What the console holds now is history: its queries get no answer.
        state
            .term
            .set_history_end(machine.console_read(u64::MAX, 0).next);
        let shared = Arc::new(Shared {
            machine,
            tx: tx.clone(),
            state: Mutex::new(state),
            closed: AtomicBool::new(false),
        });
        let (pump, machine) = (shared.clone(), shared.machine.clone());
        let spawn = |name: &str, run: Box<dyn FnOnce() + Send>| {
            std::thread::Builder::new()
                .name(name.into())
                .spawn(run)
                .map_err(|e| f::CoreError::new(f::ErrorKind::Internal, e.to_string()))
        };
        // The pump ends by `closed`, which Drop sets before the sender goes.
        let term = Arc::new(OrigamiTerminal(shared, tx));
        spawn("origami-terminal", Box::new(move || pump.pump()))?;
        spawn(
            "origami-terminal-tx",
            Box::new(move || send_loop(machine, rx)),
        )?;
        Ok(term)
    }

    pub fn size(&self) -> OrigamiTerminalSize {
        let (rows, cols) = self.0.lock().term.size();
        OrigamiTerminalSize { rows, cols }
    }

    /// Clamped like the constructor's.
    pub fn resize(&self, rows: u16, cols: u16) {
        self.0.lock().resize(rows, cols);
    }

    /// Rows of the view, `0..size().rows`; scrolled back, they show history.
    pub fn row(&self, index: u16) -> Res<Vec<OrigamiTerminalCell>> {
        let s = self.0.lock();
        if index >= s.term.size().0 {
            return Err(f::CoreError::new(f::ErrorKind::Invalid, "no such row").into());
        }
        Ok(s.row(index))
    }

    pub fn cursor(&self) -> OrigamiTerminalCursor {
        let s = self.0.lock();
        let (row, col) = s.term.cursor();
        OrigamiTerminalCursor {
            row,
            col,
            visible: s.term.cursor_visible(),
        }
    }

    /// Lines of history above the screen.
    pub fn scrollback_count(&self) -> u32 {
        self.0.lock().term.history() as u32
    }

    /// A history row, 0 being the oldest; None past the end.
    pub fn scrollback_row(&self, index: u32) -> Option<Vec<OrigamiTerminalCell>> {
        self.0.lock().history_row(index)
    }

    /// Show `lines` further back in history (negative: forward), clamped.
    pub fn scroll(&self, lines: i32) {
        self.0.lock().scroll(lines);
    }

    /// How far back the view is; 0 follows the output.
    pub fn scroll_offset(&self) -> u32 {
        self.0.lock().term.scrolled() as u32
    }

    /// What changed since `since` (0 for everything).
    pub fn changed_since(&self, since: u64) -> OrigamiTerminalDamage {
        let s = self.0.lock();
        let full = since < s.full_seq;
        let rows = if full {
            Vec::new()
        } else {
            (0..s.row_seq.len())
                .filter(|r| s.row_seq[*r] > since)
                .map(|r| r as u16)
                .collect()
        };
        OrigamiTerminalDamage {
            sequence: s.seq,
            full,
            rows,
        }
    }

    /// Send a key to the guest. `ctrl` and `alt` apply to `Text`. Never
    /// blocks: a full queue is `Busy`. Same as `send_key_with` without Shift.
    pub fn send_key(&self, key: OrigamiTerminalKey, ctrl: bool, alt: bool) -> Res<()> {
        self.send_key_with(key, false, ctrl, alt)
    }

    /// `send_key` with Shift, which matters for cursor, editing and function
    /// keys (xterm's modified forms).
    pub fn send_key_with(
        &self,
        key: OrigamiTerminalKey,
        shift: bool,
        ctrl: bool,
        alt: bool,
    ) -> Res<()> {
        let modes = self.0.lock().term.key_modes();
        match key_bytes(&key, shift, ctrl, alt, modes) {
            Some(bytes) => self.queue(bytes),
            None => Ok(()),
        }
    }

    /// Whether Backspace sends ^H (the default, which SGI consoles erase on)
    /// or DEL.
    pub fn backspace_sends_bs(&self) -> bool {
        self.0.lock().term.backspace_sends_bs
    }

    pub fn set_backspace_sends_bs(&self, on: bool) {
        self.0.lock().term.backspace_sends_bs = on;
    }

    /// Paste text: line ends become `\r`, and the guest's bracketed-paste
    /// mode is honoured. Never blocks, like `send_key`.
    pub fn paste(&self, text: String) -> Res<()> {
        let bytes = self.0.lock().term.paste_bytes(&text);
        self.queue(bytes)
    }
}

impl OrigamiTerminal {
    fn queue(&self, bytes: Vec<u8>) -> Res<()> {
        let console = self.0.machine.view().console;
        if !console.connected {
            let why = console.down_reason.unwrap_or_default();
            return Err(f::CoreError::new(f::ErrorKind::NotRunning, why).into());
        }
        self.1.try_send(bytes).map_err(|e| {
            let (kind, why) = match e {
                TrySendError::Full(_) => (f::ErrorKind::Busy, "the console is not taking input"),
                TrySendError::Disconnected(_) => (f::ErrorKind::Internal, "the sender stopped"),
            };
            f::CoreError::new(kind, why).into()
        })
    }
}

impl Drop for OrigamiTerminal {
    fn drop(&mut self) {
        self.0.closed.store(true, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn modes(app_cursor: bool) -> KeyModes {
        KeyModes {
            app_cursor,
            ..KeyModes::default()
        }
    }

    fn text(row: &[OrigamiTerminalCell]) -> String {
        row.iter().map(|c| c.text.as_str()).collect::<String>()
    }

    #[test]
    fn output_lands_with_its_colours_and_moves_the_cursor() {
        let mut s = State::new(5, 20);
        s.feed(b"\x1b[31mhi\x1b[0m");
        let row = s.row(0);
        assert_eq!(text(&row), "hi");
        assert_eq!(row[0].fg, OrigamiTerminalColour::Indexed { index: 1 });
        assert_eq!(s.term.cursor(), (0, 2));
    }

    #[test]
    fn only_the_rows_that_changed_are_reported() {
        let mut s = State::new(5, 20);
        s.feed(b"a\r\nb");
        let seen = s.seq;
        s.feed(b"c");
        // Row 1 got a character and the cursor moved within it.
        let rows: Vec<u16> = (0..5).filter(|r| s.row_seq[*r as usize] > seen).collect();
        assert_eq!(rows, [1]);
        s.feed(b"");
        assert_eq!(s.seq, seen + 1);
        s.resize(6, 20);
        assert!(s.full_seq > seen);
    }

    #[test]
    fn resize_clamps_to_the_grid_limits() {
        let mut s = State::new(0, 0);
        assert_eq!(s.term.size(), (1, 1));
        s.resize(u16::MAX, u16::MAX);
        assert_eq!(s.term.size(), (MAX_ROWS, MAX_COLS));
    }

    #[test]
    fn scrollback_is_capped_and_history_rows_read_oldest_first() {
        let mut s = State::new(5, 20);
        for i in 0..30 {
            s.feed(format!("line {i}\r\n").as_bytes());
        }
        assert_eq!(text(&s.history_row(0).unwrap()), "line 0");
        assert_eq!(text(&s.history_row(1).unwrap()), "line 1");
        assert_eq!(s.term.scrolled(), 0);
        assert!(s.history_row(s.term.history() as u32).is_none());
        for i in 0..20_000 {
            s.feed(format!("{i}\r\n").as_bytes());
        }
        // Ghostty trims history a page at a time, so it keeps a little more.
        let kept = s.term.history();
        let cap = origami_terminal::SCROLLBACK;
        assert!((cap..=cap + 2048).contains(&kept), "kept {kept}");
    }

    #[test]
    fn hostile_parameters_neither_trap_nor_hang() {
        let mut s = State::new(MAX_ROWS, MAX_COLS);
        for seq in [
            "A", "B", "C", "D", "@", "L", "M", "P", "S", "T", "X", "b", "H",
        ] {
            s.feed(format!("x\x1b[4294967295{seq}\x1b[99999999999;99999999999{seq}").as_bytes());
        }
        s.feed(b"\x1b[65535;65535H\x1b[999999r\x1b[1;999999;999999;999999$z");
        let (r, c) = s.term.cursor();
        assert!(r < MAX_ROWS && c < MAX_COLS);
    }

    #[test]
    fn paste_ends_lines_with_carriage_returns() {
        let s = State::new(5, 20);
        assert_eq!(s.term.paste_bytes("a\r\nb\nc"), b"a\rb\rc");
    }

    #[test]
    fn text_keys_map_to_the_shared_key_table() {
        let t = |s: &str| OrigamiTerminalKey::Text { text: s.into() };
        assert_eq!(
            key_bytes(&t("a"), false, true, false, modes(false)),
            Some(vec![1])
        );
        assert_eq!(
            key_bytes(&t("ab"), false, false, false, modes(false)),
            Some(b"ab".to_vec())
        );
        assert_eq!(key_bytes(&t("ab"), false, true, false, modes(false)), None);
        let up = OrigamiTerminalKey::Up;
        assert_eq!(
            key_bytes(&up, false, false, false, modes(true)),
            Some(b"\x1bOA".to_vec())
        );
        assert_eq!(
            key_bytes(&up, true, false, false, modes(true)),
            Some(b"\x1b[1;2A".to_vec())
        );
    }

    #[test]
    fn backspace_follows_the_terminal_option() {
        let mut s = State::new(5, 20);
        let bs = |s: &State| {
            key_bytes(
                &OrigamiTerminalKey::Backspace,
                false,
                false,
                false,
                s.term.key_modes(),
            )
        };
        assert_eq!(bs(&s), Some(vec![0x08]));
        s.term.backspace_sends_bs = false;
        assert_eq!(bs(&s), Some(vec![0x7f]));
    }
}
