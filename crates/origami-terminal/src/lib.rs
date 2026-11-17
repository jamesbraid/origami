//! A serial console as a terminal: the VT emulator's screen and scrollback, a
//! selection over it, and the bytes a key sends. Nothing here draws or
//! touches a window.

mod engine;
#[cfg(feature = "ghostty")]
mod ghostty_engine;
mod vt100_engine;

pub use engine::{CellInfo, Colour, Engine, EngineKind};
pub use vt100_engine::SCROLLBACK;

#[cfg(feature = "ghostty")]
pub use ghostty_engine::Ghostty;
pub use vt100_engine::Vt100;

/// A cell of the visible screen, row first.
pub type Cell = (u16, u16);

pub struct Terminal {
    engine: Box<dyn Engine>,
    /// Where a drag started and where it is now, in visible cells.
    selection: Option<(Cell, Cell)>,
    /// Whether Backspace sends ^H (SGI consoles erase on BS) instead of DEL.
    pub backspace_sends_bs: bool,
    /// Bytes the guest has sent.
    pub received: u64,
    /// Console offset below which output was already there when this
    /// terminal opened; None until a reader says (see `feed_at`).
    history_end: Option<u64>,
}

impl Terminal {
    pub fn new(rows: u16, cols: u16) -> Self {
        Self::with_engine(EngineKind::default(), rows, cols)
    }

    pub fn with_engine(kind: EngineKind, rows: u16, cols: u16) -> Self {
        let engine: Box<dyn Engine> = match kind {
            EngineKind::Vt100 => Box::new(Vt100::new(rows, cols)),
            #[cfg(feature = "ghostty")]
            EngineKind::Ghostty => Box::new(Ghostty::new(rows, cols)),
        };
        Terminal {
            engine,
            selection: None,
            backspace_sends_bs: true,
            received: 0,
            history_end: None,
        }
    }

    /// What the guest's modes and this terminal's options change in a key.
    pub fn key_modes(&self) -> KeyModes {
        KeyModes {
            app_cursor: self.app_cursor(),
            backspace_bs: self.backspace_sends_bs,
        }
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        self.received += bytes.len() as u64;
        // New output moves the text under a selection, and a viewer that has
        // scrolled back stays where it is looking (vt100 keeps the offset).
        self.selection = None;
        self.engine.feed(bytes);
    }

    /// Output before console offset `end` is history: `feed_at` answers none
    /// of its queries.
    pub fn set_history_end(&mut self, end: u64) {
        self.history_end = Some(end);
    }

    pub fn history_end(&self) -> Option<u64> {
        self.history_end
    }

    /// Feed output that starts at console offset `offset`; returns the bytes
    /// to send the guest in reply to the part after the history, so a
    /// replayed query does not type a reply into a guest that has moved on.
    pub fn feed_at(&mut self, offset: u64, bytes: &[u8]) -> Vec<u8> {
        let end = self.history_end.unwrap_or(0);
        let split = end.saturating_sub(offset).min(bytes.len() as u64) as usize;
        self.feed(&bytes[..split]);
        drop(self.engine.take_replies());
        self.feed(&bytes[split..]);
        self.engine.take_replies()
    }

    pub fn resize(&mut self, rows: u16, cols: u16) {
        let (r, c) = self.size();
        if (r, c) != (rows.max(1), cols.max(1)) {
            self.selection = None;
            self.engine.resize(rows.max(1), cols.max(1));
        }
    }

    /// (rows, columns)
    pub fn size(&self) -> (u16, u16) {
        self.engine.size()
    }

    pub fn cell(&self, row: u16, col: u16) -> Option<CellInfo> {
        self.engine.cell(row, col)
    }

    /// (row, column) on the live screen.
    pub fn cursor(&self) -> (u16, u16) {
        self.engine.cursor()
    }

    pub fn cursor_visible(&self) -> bool {
        self.engine.cursor_visible()
    }

    pub fn hyperlink(&self, row: u16, col: u16) -> Option<String> {
        self.engine.hyperlink(row, col)
    }

    /// Bytes the emulator wants sent to the guest, such as a device
    /// attributes reply.
    pub fn take_replies(&mut self) -> Vec<u8> {
        self.engine.take_replies()
    }

    /// The view as text: rows joined by newlines, trailing blank rows cut.
    pub fn screen_text(&self) -> String {
        let rows = (0..self.size().0).map(|r| self.row_text(r));
        let text = rows.collect::<Vec<_>>().join("\n");
        text.trim_end_matches('\n').to_string()
    }

    /// A row of the view as text, blanks as spaces, trailing blanks cut.
    pub fn row_text(&self, row: u16) -> String {
        let (_, cols) = self.size();
        let mut s = String::new();
        for c in 0..cols {
            match self.cell(row, c) {
                Some(cell) if cell.text.is_empty() && !cell.continuation => s.push(' '),
                Some(cell) => s.push_str(&cell.text),
                None => {}
            }
        }
        s.truncate(s.trim_end().len());
        s
    }

    /// Lines of history above what the screen shows now (0 when at the end).
    pub fn scrolled(&self) -> usize {
        self.engine.scrolled()
    }

    pub fn history(&self) -> usize {
        self.engine.history()
    }

    /// Scroll `rows` lines back into history (negative: forward).
    pub fn scroll(&mut self, rows: isize) {
        let now = self.scrolled() as isize;
        let to = (now + rows).clamp(0, self.history() as isize) as usize;
        if to != self.scrolled() {
            self.selection = None;
            self.engine.set_scrolled(to);
        }
    }

    pub fn scroll_to_end(&mut self) {
        self.scroll(-(self.scrolled() as isize));
    }

    pub fn app_cursor(&self) -> bool {
        self.engine.app_cursor()
    }

    pub fn bracketed_paste(&self) -> bool {
        self.engine.bracketed_paste()
    }

    pub fn select_from(&mut self, cell: Cell) {
        self.selection = Some((cell, cell));
    }

    pub fn select_to(&mut self, cell: Cell) {
        if let Some((from, _)) = self.selection {
            self.selection = Some((from, cell));
        }
    }

    pub fn clear_selection(&mut self) {
        self.selection = None;
    }

    /// The selection, first cell before last, both included. None for a
    /// click that selected nothing.
    pub fn selection(&self) -> Option<(Cell, Cell)> {
        let (a, b) = self.selection?;
        if a == b {
            return None;
        }
        Some(if a <= b { (a, b) } else { (b, a) })
    }

    pub fn is_selected(&self, row: u16, col: u16) -> bool {
        self.selection()
            .is_some_and(|(a, b)| (row, col) >= a && (row, col) <= b)
    }

    /// The selected text, line ends where the screen's lines end.
    pub fn selected_text(&self) -> Option<String> {
        let ((r0, c0), (r1, c1)) = self.selection()?;
        let (_, cols) = self.size();
        let text = self.engine.text_between(r0, c0, r1, (c1 + 1).min(cols));
        let text = text
            .split('\n')
            .map(str::trim_end)
            .collect::<Vec<_>>()
            .join("\n");
        (!text.is_empty()).then_some(text)
    }

    /// What pasting `text` sends: lines end with a carriage return, as an
    /// Enter key does, and the whole is bracketed when the guest asked.
    pub fn paste_bytes(&self, text: &str) -> Vec<u8> {
        let body = text.replace("\r\n", "\r").replace('\n', "\r");
        if self.bracketed_paste() {
            // An ESC in the text could close the bracket early and let the
            // rest run as typed input.
            let body = body.replace('\u{1b}', "");
            let mut out = b"\x1b[200~".to_vec();
            out.extend_from_slice(body.as_bytes());
            out.extend_from_slice(b"\x1b[201~");
            out
        } else {
            body.into_bytes()
        }
    }
}

/// A key a serial terminal has bytes for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Char(char),
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
    F(u8),
}

/// What changes the bytes a key sends: the guest's cursor-key mode, and
/// whether Backspace is ^H rather than DEL.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct KeyModes {
    pub app_cursor: bool,
    pub backspace_bs: bool,
}

/// The bytes `key` sends, or None when it sends nothing. `alt` prefixes an
/// ESC except on keys that carry it as an xterm modifier.
pub fn key_bytes(key: Key, shift: bool, ctrl: bool, alt: bool, modes: KeyModes) -> Option<Vec<u8>> {
    let app_cursor = modes.app_cursor;
    let md = 1 + u8::from(shift) + 2 * u8::from(alt) + 4 * u8::from(ctrl);
    let csi = |s: &str| format!("\x1b[{s}").into_bytes();
    let ss3 = |s: &str| format!("\x1bO{s}").into_bytes();
    let letter = |f: &str, ss3_plain: bool| match (md, ss3_plain) {
        (1, true) => ss3(f),
        (1, false) => csi(f),
        _ => csi(&format!("1;{md}{f}")),
    };
    let tilde = |n: u8| match md {
        1 => csi(&format!("{n}~")),
        _ => csi(&format!("{n};{md}~")),
    };
    let cursor = |c: &str| letter(c, app_cursor);
    let modified = md > 1
        && matches!(
            key,
            Key::Up
                | Key::Down
                | Key::Left
                | Key::Right
                | Key::Home
                | Key::End
                | Key::PageUp
                | Key::PageDown
                | Key::Insert
                | Key::Delete
                | Key::F(1..=12)
        );
    let mut out = match key {
        Key::Up => cursor("A"),
        Key::Down => cursor("B"),
        Key::Right => cursor("C"),
        Key::Left => cursor("D"),
        Key::Home => cursor("H"),
        Key::End => cursor("F"),
        Key::PageUp => tilde(5),
        Key::PageDown => tilde(6),
        Key::Insert => tilde(2),
        Key::Delete => tilde(3),
        Key::BackTab => csi("Z"),
        Key::Enter => b"\r".to_vec(),
        // ^H for SGI consoles; DEL, as xterm and the Linux console send, otherwise.
        Key::Backspace if modes.backspace_bs => vec![0x08],
        Key::Backspace => vec![0x7f],
        Key::Tab => b"\t".to_vec(),
        Key::Escape => vec![0x1b],
        Key::F(n) => match n {
            1..=4 => letter(["P", "Q", "R", "S"][usize::from(n) - 1], true),
            5..=12 => tilde([15, 17, 18, 19, 20, 21, 23, 24][usize::from(n) - 5]),
            _ => return None,
        },
        Key::Char(c) if (c as u32) < 0x20 => vec![c as u8],
        Key::Char(c) if ctrl => vec![match c {
            'a'..='z' | 'A'..='Z' => (c.to_ascii_lowercase() as u8) & 0x1f,
            '@' | ' ' | '2' => 0,
            '[' | '3' => 0x1b,
            '\\' | '4' => 0x1c,
            ']' | '5' => 0x1d,
            '^' | '6' => 0x1e,
            '_' | '7' | '/' => 0x1f,
            '8' => 0x7f,
            _ => return None,
        }],
        Key::Char(c) => c.to_string().into_bytes(),
    };
    if alt && !modified {
        out.insert(0, 0x1b);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app_cursor(app_cursor: bool) -> KeyModes {
        KeyModes {
            app_cursor,
            ..KeyModes::default()
        }
    }

    fn each(f: impl Fn(EngineKind)) {
        f(EngineKind::Vt100);
        #[cfg(feature = "ghostty")]
        f(EngineKind::Ghostty);
    }

    #[test]
    fn output_lands_on_the_screen_and_old_lines_go_to_scrollback() {
        each(|k| {
            let mut t = Terminal::with_engine(k, 5, 20);
            for i in 0..30 {
                t.feed(format!("line {i}\r\n").as_bytes());
            }
            assert!(t.history() >= 25);
            assert_eq!(t.scrolled(), 0);
            t.scroll(10);
            assert_eq!(t.scrolled(), 10);
            let top = t.row_text(0);
            assert!(top.starts_with("line "), "{top}");
            t.scroll_to_end();
            assert_eq!(t.scrolled(), 0);
            // Never further than there is history.
            t.scroll(1_000_000);
            assert_eq!(t.scrolled(), t.history());
        });
    }

    #[test]
    fn a_selection_yields_its_text_and_clears_on_new_output() {
        each(|k| {
            let mut t = Terminal::with_engine(k, 5, 20);
            t.feed(b"hello world\r\nsecond line\r\n");
            t.select_from((0, 6));
            t.select_to((0, 10));
            assert_eq!(t.selected_text().as_deref(), Some("world"));
            assert!(t.is_selected(0, 8));
            assert!(!t.is_selected(0, 5));
            // Dragging up and left selects the same cells.
            t.select_from((1, 5));
            t.select_to((0, 6));
            assert_eq!(t.selected_text().as_deref(), Some("world\nsecond"));
            t.feed(b"x");
            assert_eq!(t.selected_text(), None);
        });
    }

    #[test]
    fn a_click_without_a_drag_selects_nothing() {
        each(|k| {
            let mut t = Terminal::with_engine(k, 5, 20);
            t.feed(b"abc");
            t.select_from((0, 1));
            assert_eq!(t.selection(), None);
            assert_eq!(t.selected_text(), None);
        });
    }

    #[test]
    fn a_paste_ends_lines_with_carriage_returns_and_brackets_when_asked() {
        each(|k| {
            let mut t = Terminal::with_engine(k, 5, 20);
            assert_eq!(t.paste_bytes("ls\nls -l\r\n"), b"ls\rls -l\r");
            t.feed(b"\x1b[?2004h");
            assert_eq!(t.paste_bytes("a"), b"\x1b[200~a\x1b[201~");
            assert_eq!(t.paste_bytes("a\x1b[201~b"), b"\x1b[200~a[201~b\x1b[201~");
        });
    }

    #[test]
    fn resizing_keeps_the_text() {
        each(|k| {
            let mut t = Terminal::with_engine(k, 5, 20);
            t.feed(b"abc");
            t.resize(10, 40);
            assert_eq!(t.size(), (10, 40));
            assert!(t.row_text(0).starts_with("abc"));
        });
    }

    #[test]
    fn keys_send_what_a_serial_terminal_expects() {
        let k = |key, ctrl, alt, app| key_bytes(key, false, ctrl, alt, app_cursor(app));
        assert_eq!(k(Key::Char('a'), true, false, false), Some(vec![1]));
        assert_eq!(
            k(Key::Char('x'), false, true, false),
            Some(b"\x1bx".to_vec())
        );
        assert_eq!(k(Key::Char('\u{e9}'), true, false, false), None);
        assert_eq!(k(Key::Up, false, false, true), Some(b"\x1bOA".to_vec()));
        assert_eq!(k(Key::Up, false, false, false), Some(b"\x1b[A".to_vec()));
        assert_eq!(
            k(Key::F(5), false, false, false),
            Some(b"\x1b[15~".to_vec())
        );
        assert_eq!(k(Key::F(13), false, false, false), None);
    }

    #[test]
    fn modified_navigation_and_function_keys_use_the_xterm_forms() {
        let k =
            |key, shift, ctrl, alt, app| key_bytes(key, shift, ctrl, alt, app_cursor(app)).unwrap();
        // mod = 1 + shift + 2 alt + 4 ctrl
        assert_eq!(k(Key::Up, true, false, false, false), b"\x1b[1;2A");
        assert_eq!(k(Key::Left, false, false, true, false), b"\x1b[1;3D");
        assert_eq!(k(Key::Right, false, true, false, false), b"\x1b[1;5C");
        assert_eq!(k(Key::Home, true, true, true, false), b"\x1b[1;8H");
        // Application cursor mode only changes the unmodified form.
        assert_eq!(k(Key::Up, false, true, false, true), b"\x1b[1;5A");
        assert_eq!(k(Key::Up, false, false, false, true), b"\x1bOA");
        assert_eq!(k(Key::PageUp, false, true, false, false), b"\x1b[5;5~");
        assert_eq!(k(Key::Delete, true, false, false, false), b"\x1b[3;2~");
        assert_eq!(k(Key::F(1), true, false, false, false), b"\x1b[1;2P");
        assert_eq!(k(Key::F(5), false, true, false, false), b"\x1b[15;5~");
        assert_eq!(k(Key::F(12), false, false, true, false), b"\x1b[24;3~");
        // No modifier, and keys that are not modified this way, are as before.
        assert_eq!(k(Key::F(2), false, false, false, false), b"\x1bOQ");
        assert_eq!(k(Key::PageDown, false, false, false, false), b"\x1b[6~");
        assert_eq!(k(Key::Char('x'), true, false, true, false), b"\x1bx");
    }

    #[test]
    fn backspace_sends_bs_unless_the_option_is_off() {
        let mut t = Terminal::new(5, 20);
        let bs = |t: &Terminal, alt| key_bytes(Key::Backspace, false, false, alt, t.key_modes());
        assert!(t.backspace_sends_bs);
        assert_eq!(bs(&t, false), Some(vec![0x08]));
        assert_eq!(bs(&t, true), Some(vec![0x1b, 0x08]));
        t.backspace_sends_bs = false;
        assert_eq!(bs(&t, false), Some(vec![0x7f]));
        // The cursor-key mode travels in the same value.
        t.feed(b"\x1b[?1h");
        assert!(t.key_modes().app_cursor);
    }

    #[cfg(feature = "ghostty")]
    #[test]
    fn ghostty_answers_queries_and_reports_links_and_wide_cells() {
        let mut t = Terminal::with_engine(EngineKind::Ghostty, 5, 20);
        t.feed(b"\x1b[c\x1b[6n");
        let reply = String::from_utf8(t.take_replies()).unwrap();
        assert!(
            reply.starts_with("\x1b[?") && reply.ends_with("\x1b[1;1R"),
            "{reply:?}"
        );
        assert!(t.take_replies().is_empty());
        t.feed(b"\x1b[2J\x1b[H\x1b]8;;http://x.test/\x1b\\ab\x1b]8;;\x1b\\c\xe6\xbc\xa2");
        assert_eq!(t.hyperlink(0, 1).as_deref(), Some("http://x.test/"));
        assert_eq!(t.hyperlink(0, 2), None);
        let (wide, tail) = (t.cell(0, 3).unwrap(), t.cell(0, 4).unwrap());
        assert!(wide.wide && wide.text == "\u{6f22}");
        assert!(tail.continuation && tail.text.is_empty());
        // A background-only cell keeps its colour outside the style.
        t.feed(b"\x1b[2J\x1b[H\x1b[44m\x1b[K");
        assert_eq!(t.cell(0, 3).unwrap().bg, Colour::Idx(4));
    }

    #[cfg(feature = "ghostty")]
    #[test]
    fn a_ghostty_terminal_can_move_between_threads() {
        let mut t = Terminal::with_engine(EngineKind::Ghostty, 5, 20);
        t.feed(b"hi");
        let t = std::thread::spawn(move || {
            t.feed(b"!");
            t
        })
        .join()
        .unwrap();
        assert_eq!(t.row_text(0), "hi!");
    }

    #[cfg(feature = "ghostty")]
    #[test]
    fn replies_to_history_are_dropped() {
        let mut t = Terminal::with_engine(EngineKind::Ghostty, 5, 20);
        t.set_history_end(3);
        // The first query (device attributes) is history, the second is live.
        assert_eq!(t.feed_at(0, b"\x1b[c\x1b[6n"), b"\x1b[1;1R");
        assert!(t.feed_at(0, b"\x1b[c").is_empty());
        assert!(t.feed_at(3, b"\x1b[c").starts_with(b"\x1b[?"));
    }
}
