use crate::engine::{nz, CellInfo, Colour, Engine};

/// Lines of history kept above the screen.
pub const SCROLLBACK: usize = 5000;

pub struct Vt100(vt100::Parser);

impl Vt100 {
    pub fn new(rows: u16, cols: u16) -> Self {
        Vt100(vt100::Parser::new(nz(rows), nz(cols), SCROLLBACK))
    }
}

fn colour(c: vt100::Color) -> Colour {
    match c {
        vt100::Color::Default => Colour::Default,
        vt100::Color::Idx(i) => Colour::Idx(i),
        vt100::Color::Rgb(r, g, b) => Colour::Rgb(r, g, b),
    }
}

impl Engine for Vt100 {
    fn feed(&mut self, bytes: &[u8]) {
        self.0.process(bytes);
    }

    fn size(&self) -> (u16, u16) {
        let (r, c) = self.0.screen().size();
        (r.get(), c.get())
    }

    fn resize(&mut self, rows: u16, cols: u16) {
        self.0.set_size(nz(rows), nz(cols));
    }

    fn cell(&self, row: u16, col: u16) -> Option<CellInfo> {
        let c = self.0.screen().cell(row, col)?;
        Some(CellInfo {
            text: c.contents().to_string(),
            fg: colour(c.fgcolor()),
            bg: colour(c.bgcolor()),
            bold: c.bold(),
            dim: c.dim(),
            italic: c.italic(),
            underline: c.underline(),
            inverse: c.inverse(),
            wide: c.is_wide(),
            continuation: c.is_wide_continuation(),
        })
    }

    fn cursor(&self) -> (u16, u16) {
        self.0.screen().cursor_position()
    }

    fn cursor_visible(&self) -> bool {
        !self.0.screen().hide_cursor()
    }

    fn history(&self) -> usize {
        self.0.screen().scrollback_len()
    }

    fn scrolled(&self) -> usize {
        self.0.screen().scrollback()
    }

    fn set_scrolled(&mut self, lines: usize) {
        self.0.screen_mut().set_scrollback(lines);
    }

    fn app_cursor(&self) -> bool {
        self.0.screen().application_cursor()
    }

    fn bracketed_paste(&self) -> bool {
        self.0.screen().bracketed_paste()
    }

    fn text_between(&self, r0: u16, c0: u16, r1: u16, c1_end: u16) -> String {
        self.0.screen().contents_between(r0, c0, r1, c1_end)
    }
}
