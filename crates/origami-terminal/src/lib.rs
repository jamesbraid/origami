//! A serial console as a terminal: the VT emulator's screen and scrollback, a
//! selection over it, and the bytes a key sends. Nothing here draws or
//! touches a window.

use std::num::NonZeroU16;

/// Lines of history kept above the screen.
pub const SCROLLBACK: usize = 5000;

fn nz(n: u16) -> NonZeroU16 {
    NonZeroU16::new(n.max(1)).unwrap_or(NonZeroU16::MIN)
}

/// A cell of the visible screen, row first.
pub type Cell = (u16, u16);

pub struct Terminal {
    parser: vt100::Parser,
    /// Where a drag started and where it is now, in visible cells.
    selection: Option<(Cell, Cell)>,
    /// Bytes the guest has sent.
    pub received: u64,
}

impl Terminal {
    pub fn new(rows: u16, cols: u16) -> Self {
        Terminal {
            parser: vt100::Parser::new(nz(rows), nz(cols), SCROLLBACK),
            selection: None,
            received: 0,
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
        self.parser.process(bytes);
    }

    pub fn resize(&mut self, rows: u16, cols: u16) {
        let (r, c) = self.size();
        if (r, c) != (rows.max(1), cols.max(1)) {
            self.selection = None;
            self.parser.set_size(nz(rows), nz(cols));
        }
    }

    /// (rows, columns)
    pub fn size(&self) -> (u16, u16) {
        let (r, c) = self.parser.screen().size();
        (r.get(), c.get())
    }

    pub fn screen(&self) -> &vt100::Screen {
        self.parser.screen()
    }

    /// Lines of history above what the screen shows now (0 when at the end).
    pub fn scrolled(&self) -> usize {
        self.parser.screen().scrollback()
    }

    pub fn history(&self) -> usize {
        self.parser.screen().scrollback_len()
    }

    /// Scroll `rows` lines back into history (negative: forward).
    pub fn scroll(&mut self, rows: isize) {
        let now = self.scrolled() as isize;
        let to = (now + rows).clamp(0, self.history() as isize) as usize;
        if to != self.scrolled() {
            self.selection = None;
            self.parser.screen_mut().set_scrollback(to);
        }
    }

    pub fn scroll_to_end(&mut self) {
        self.scroll(-(self.scrolled() as isize));
    }

    pub fn app_cursor(&self) -> bool {
        self.parser.screen().application_cursor()
    }

    pub fn bracketed_paste(&self) -> bool {
        self.parser.screen().bracketed_paste()
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
        let text = self
            .parser
            .screen()
            .contents_between(r0, c0, r1, (c1 + 1).min(cols));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_lands_on_the_screen_and_old_lines_go_to_scrollback() {
        let mut t = Terminal::new(5, 20);
        for i in 0..30 {
            t.feed(format!("line {i}\r\n").as_bytes());
        }
        assert!(t.history() >= 25);
        assert_eq!(t.scrolled(), 0);
        t.scroll(10);
        assert_eq!(t.scrolled(), 10);
        let top = t.screen().rows(0, 20).next().unwrap();
        assert!(top.starts_with("line "), "{top}");
        t.scroll_to_end();
        assert_eq!(t.scrolled(), 0);
        // Never further than there is history.
        t.scroll(1_000_000);
        assert_eq!(t.scrolled(), t.history());
    }

    #[test]
    fn a_selection_yields_its_text_and_clears_on_new_output() {
        let mut t = Terminal::new(5, 20);
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
    }

    #[test]
    fn a_click_without_a_drag_selects_nothing() {
        let mut t = Terminal::new(5, 20);
        t.feed(b"abc");
        t.select_from((0, 1));
        assert_eq!(t.selection(), None);
        assert_eq!(t.selected_text(), None);
    }

    #[test]
    fn a_paste_ends_lines_with_carriage_returns_and_brackets_when_asked() {
        let mut t = Terminal::new(5, 20);
        assert_eq!(t.paste_bytes("ls\nls -l\r\n"), b"ls\rls -l\r");
        t.feed(b"\x1b[?2004h");
        assert_eq!(t.paste_bytes("a"), b"\x1b[200~a\x1b[201~");
        assert_eq!(t.paste_bytes("a\x1b[201~b"), b"\x1b[200~a[201~b\x1b[201~");
    }

    #[test]
    fn resizing_keeps_the_text() {
        let mut t = Terminal::new(5, 20);
        t.feed(b"abc");
        t.resize(10, 40);
        assert_eq!(t.size(), (10, 40));
        assert!(t.screen().contents().starts_with("abc"));
    }
}
