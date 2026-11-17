//! What the terminal asks of a VT emulator, so the one behind it can change.

use std::num::NonZeroU16;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Colour {
    Default,
    Idx(u8),
    Rgb(u8, u8, u8),
}

/// One visible cell. `text` is empty for a blank cell and for the second
/// half of a wide character (`continuation`).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct CellInfo {
    pub text: String,
    pub fg: Colour,
    pub bg: Colour,
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: bool,
    pub inverse: bool,
    pub wide: bool,
    pub continuation: bool,
}

/// Which emulator a `Terminal` runs on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum EngineKind {
    #[default]
    Vt100,
}

pub trait Engine: Send {
    fn feed(&mut self, bytes: &[u8]);
    /// (rows, columns)
    fn size(&self) -> (u16, u16);
    fn resize(&mut self, rows: u16, cols: u16);
    /// A cell of the view, which shows history when scrolled back.
    fn cell(&self, row: u16, col: u16) -> Option<CellInfo>;
    /// (row, column) on the live screen.
    fn cursor(&self) -> (u16, u16);
    fn cursor_visible(&self) -> bool;
    /// Lines of history above the live screen.
    fn history(&self) -> usize;
    /// How far back the view is; 0 at the live screen.
    fn scrolled(&self) -> usize;
    fn set_scrolled(&mut self, lines: usize);
    fn app_cursor(&self) -> bool;
    fn bracketed_paste(&self) -> bool;
    /// Text from (r0, c0) up to but not including (r1, c1_end).
    fn text_between(&self, r0: u16, c0: u16, r1: u16, c1_end: u16) -> String;
    /// The OSC 8 link on a cell of the view.
    fn hyperlink(&self, _row: u16, _col: u16) -> Option<String> {
        None
    }
    /// Bytes the emulator wants sent back to the guest (DA, DSR replies).
    fn take_replies(&mut self) -> Vec<u8> {
        Vec::new()
    }
}

pub(crate) fn nz(n: u16) -> NonZeroU16 {
    NonZeroU16::new(n.max(1)).unwrap_or(NonZeroU16::MIN)
}
