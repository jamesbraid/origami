//! libghostty-vt behind `Engine`, through the grid-reference API (one cell
//! at a time, which suits an 80x24 serial console).

use crate::engine::{CellInfo, Colour, Engine};
use crate::SCROLLBACK;
use ghostty_vt_sys as g;
use std::ffi::c_void;
use std::ptr;

#[derive(Default)]
struct Replies(Vec<u8>);

pub struct Ghostty {
    term: g::GhosttyTerminal,
    // Boxed so the callback's userdata pointer survives moves of `Ghostty`.
    replies: Box<Replies>,
}

// The library is not thread safe, but every call comes through the one lock
// that guards the `Terminal`, so a hand-off between threads is sequential.
unsafe impl Send for Ghostty {}

const MAX_REPLIES: usize = 4096;

unsafe extern "C" fn write_pty(
    _t: g::GhosttyTerminal,
    ud: *mut c_void,
    data: *const u8,
    len: usize,
) {
    let out = &mut *(ud as *mut Replies);
    // Replies are drained after every feed; this bounds a flood of queries.
    if out.0.len() + len <= MAX_REPLIES {
        out.0
            .extend_from_slice(std::slice::from_raw_parts(data, len));
    }
}

impl Ghostty {
    /// Lines asked for beyond SCROLLBACK: ghostty drops whole pages, up to
    /// about 1500 lines at 20 columns.
    const SCROLLBACK_SLACK: usize = 2048;

    pub fn new(rows: u16, cols: u16) -> Self {
        let (rows, cols) = (rows.max(1), cols.max(1));
        let mut term = ptr::null_mut();
        let r = unsafe { g::ghostty_terminal_new(ptr::null(), &mut term, cols, rows) };
        assert_eq!(r, g::GHOSTTY_SUCCESS, "ghostty_terminal_new");
        let mut replies = Box::<Replies>::default();
        let ud = &mut *replies as *mut Replies as *mut c_void;
        // The byte limit defaults to one page: lift it and cap by lines.
        let (lines, bytes): (usize, usize) = (SCROLLBACK + Self::SCROLLBACK_SLACK, usize::MAX);
        unsafe {
            g::ghostty_terminal_set(term, g::OPT_USERDATA, ud);
            g::ghostty_terminal_set(
                term,
                g::OPT_WRITE_PTY,
                write_pty as g::WritePtyFn as *const c_void,
            );
            g::ghostty_terminal_set(
                term,
                g::OPT_SCROLLBACK_MAX_BYTES,
                &bytes as *const usize as *const c_void,
            );
            g::ghostty_terminal_set(
                term,
                g::OPT_SCROLLBACK_MAX_LINES,
                &lines as *const usize as *const c_void,
            );
        }
        Ghostty { term, replies }
    }

    fn get<T: Default>(&self, key: i32) -> T {
        let mut v = T::default();
        unsafe { g::ghostty_terminal_get(self.term, key, &mut v as *mut T as *mut c_void) };
        v
    }

    fn scrollbar(&self) -> (u64, u64, u64) {
        let mut s = g::GhosttyTerminalScrollbar {
            total: 0,
            offset: 0,
            len: 0,
        };
        unsafe {
            g::ghostty_terminal_get(
                self.term,
                g::DATA_SCROLLBAR,
                &mut s as *mut _ as *mut c_void,
            )
        };
        (s.total, s.offset, s.len)
    }

    fn mode(&self, mode: u16) -> bool {
        let mut m = g::GhosttyTerminalModeConfig { mode, value: false };
        unsafe {
            g::ghostty_terminal_get(self.term, g::DATA_MODE, &mut m as *mut _ as *mut c_void)
        };
        m.value
    }

    fn grid_ref(&self, row: u16, col: u16) -> Option<g::GhosttyGridRef> {
        let (rows, cols) = self.size();
        if row >= rows || col >= cols {
            return None;
        }
        let mut r = g::GhosttyGridRef {
            node: ptr::null_mut(),
            x: 0,
            y: 0,
        };
        let p = g::GhosttyPoint::new(g::POINT_VIEWPORT, col, u32::from(row));
        let ok = unsafe { g::ghostty_terminal_grid_ref(self.term, p, &mut r) };
        (ok == g::GHOSTTY_SUCCESS).then_some(r)
    }
}

impl Drop for Ghostty {
    fn drop(&mut self) {
        unsafe { g::ghostty_terminal_free(self.term) };
    }
}

fn colour(c: g::GhosttyStyleColor) -> Colour {
    match c.tag {
        g::STYLE_COLOR_PALETTE => Colour::Idx(c.value[0]),
        g::STYLE_COLOR_RGB => Colour::Rgb(c.value[0], c.value[1], c.value[2]),
        _ => Colour::Default,
    }
}

fn cell_get<T: Default>(cell: g::GhosttyCell, key: i32) -> T {
    let mut v = T::default();
    unsafe { g::ghostty_cell_get(cell, key, &mut v as *mut T as *mut c_void) };
    v
}

impl Engine for Ghostty {
    fn feed(&mut self, bytes: &[u8]) {
        unsafe { g::ghostty_terminal_vt_write(self.term, bytes.as_ptr(), bytes.len()) };
    }

    fn size(&self) -> (u16, u16) {
        let cols: u16 = self.get(1);
        let rows: u16 = self.get(2);
        (rows, cols)
    }

    fn resize(&mut self, rows: u16, cols: u16) {
        unsafe { g::ghostty_terminal_resize(self.term, cols.max(1), rows.max(1), 0, 0) };
    }

    fn cell(&self, row: u16, col: u16) -> Option<CellInfo> {
        let r = self.grid_ref(row, col)?;
        let mut cell: g::GhosttyCell = 0;
        unsafe { g::ghostty_grid_ref_cell(&r, &mut cell) };
        let mut style = g::GhosttyStyle::default();
        unsafe { g::ghostty_grid_ref_style(&r, &mut style) };
        let wide: i32 = cell_get(cell, g::CELL_DATA_WIDE);
        let mut cps = [0u32; 16];
        let mut n = 0usize;
        let mut text = String::new();
        let ok = unsafe { g::ghostty_grid_ref_graphemes(&r, cps.as_mut_ptr(), cps.len(), &mut n) };
        if ok == g::GHOSTTY_SUCCESS {
            text.extend(cps[..n].iter().filter_map(|c| char::from_u32(*c)));
        }
        let mut bg = colour(style.bg_color);
        // A cell that only sets a background keeps it in the cell, not the style.
        match cell_get::<i32>(cell, g::CELL_DATA_CONTENT_TAG) {
            g::CELL_CONTENT_BG_COLOR_PALETTE => {
                bg = Colour::Idx(cell_get::<u8>(cell, g::CELL_DATA_COLOR_PALETTE));
            }
            g::CELL_CONTENT_BG_COLOR_RGB => {
                let rgb: [u8; 3] = cell_get(cell, g::CELL_DATA_COLOR_RGB);
                bg = Colour::Rgb(rgb[0], rgb[1], rgb[2]);
            }
            _ => {}
        }
        let f = style.flags;
        Some(CellInfo {
            text,
            fg: colour(style.fg_color),
            bg,
            bold: f & g::STYLE_BOLD != 0,
            dim: f & g::STYLE_FAINT != 0,
            italic: f & g::STYLE_ITALIC != 0,
            underline: f & g::STYLE_UNDERLINE_MASK != 0,
            inverse: f & g::STYLE_INVERSE != 0,
            wide: wide == g::CELL_WIDE_WIDE,
            continuation: wide == g::CELL_WIDE_SPACER_TAIL,
        })
    }

    fn cursor(&self) -> (u16, u16) {
        (self.get(g::DATA_CURSOR_Y), self.get(g::DATA_CURSOR_X))
    }

    fn cursor_visible(&self) -> bool {
        self.get(g::DATA_CURSOR_VISIBLE)
    }

    fn history(&self) -> usize {
        let (total, _, len) = self.scrollbar();
        total.saturating_sub(len) as usize
    }

    fn scrolled(&self) -> usize {
        let (total, offset, len) = self.scrollbar();
        total.saturating_sub(len).saturating_sub(offset) as usize
    }

    fn set_scrolled(&mut self, lines: usize) {
        let (total, _, len) = self.scrollbar();
        let row = total.saturating_sub(len).saturating_sub(lines as u64);
        let b = g::GhosttyTerminalScrollViewport {
            tag: g::SCROLL_VIEWPORT_ROW,
            value: [row as i64, 0],
        };
        unsafe { g::ghostty_terminal_scroll_viewport(self.term, b) };
    }

    fn app_cursor(&self) -> bool {
        self.mode(g::MODE_CURSOR_KEYS)
    }

    fn bracketed_paste(&self) -> bool {
        self.mode(g::MODE_BRACKETED_PASTE)
    }

    fn text_between(&self, r0: u16, c0: u16, r1: u16, c1_end: u16) -> String {
        let (_, cols) = self.size();
        let mut out = String::new();
        for row in r0..=r1 {
            let (from, to) = (
                if row == r0 { c0 } else { 0 },
                if row == r1 { c1_end.min(cols) } else { cols },
            );
            if row > r0 {
                out.push('\n');
            }
            for col in from..to {
                match self.cell(row, col) {
                    Some(c) if c.text.is_empty() && !c.continuation => out.push(' '),
                    Some(c) => out.push_str(&c.text),
                    None => {}
                }
            }
        }
        out
    }

    fn hyperlink(&self, row: u16, col: u16) -> Option<String> {
        let r = self.grid_ref(row, col)?;
        let mut cell: g::GhosttyCell = 0;
        unsafe { g::ghostty_grid_ref_cell(&r, &mut cell) };
        if !cell_get::<bool>(cell, g::CELL_DATA_HAS_HYPERLINK) {
            return None;
        }
        let mut buf = [0u8; 2048];
        let mut n = 0usize;
        let ok =
            unsafe { g::ghostty_grid_ref_hyperlink_uri(&r, buf.as_mut_ptr(), buf.len(), &mut n) };
        (ok == g::GHOSTTY_SUCCESS && n > 0).then(|| String::from_utf8_lossy(&buf[..n]).into_owned())
    }

    fn take_replies(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.replies.0)
    }
}
