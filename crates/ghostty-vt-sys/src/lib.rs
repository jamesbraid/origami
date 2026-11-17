//! The part of libghostty-vt 1.0's C ABI the terminal engine uses, declared
//! by hand from `ghostty/vt/*.h` (release 1.0.1). Layouts are checked below.

#![allow(non_camel_case_types)]

use core::ffi::c_void;

pub type GhosttyTerminal = *mut c_void;
pub type GhosttyResult = i32;
pub const GHOSTTY_SUCCESS: GhosttyResult = 0;
pub const GHOSTTY_OUT_OF_SPACE: GhosttyResult = -3;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct GhosttyGridRef {
    pub node: *mut c_void,
    pub x: u16,
    pub y: u16,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct GhosttyPoint {
    pub tag: i32,
    _pad: u32,
    pub x: u16,
    pub y: u32,
    _tail: u64,
}

impl GhosttyPoint {
    pub fn new(tag: i32, x: u16, y: u32) -> Self {
        GhosttyPoint {
            tag,
            _pad: 0,
            x,
            y,
            _tail: 0,
        }
    }
}

pub const POINT_VIEWPORT: i32 = 1;
pub const POINT_SCREEN: i32 = 2;

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct GhosttyStyleColor {
    pub tag: u8,
    pub value: [u8; 3],
}

pub const STYLE_COLOR_NONE: u8 = 0;
pub const STYLE_COLOR_PALETTE: u8 = 1;
pub const STYLE_COLOR_RGB: u8 = 2;

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct GhosttyStyle {
    pub fg_color: GhosttyStyleColor,
    pub bg_color: GhosttyStyleColor,
    pub underline_color: GhosttyStyleColor,
    pub flags: u16,
    pub reserved: u16,
}

pub const STYLE_BOLD: u16 = 1 << 0;
pub const STYLE_ITALIC: u16 = 1 << 1;
pub const STYLE_FAINT: u16 = 1 << 2;
pub const STYLE_INVERSE: u16 = 1 << 4;
pub const STYLE_UNDERLINE_SHIFT: u16 = 8;
pub const STYLE_UNDERLINE_MASK: u16 = 0x7 << STYLE_UNDERLINE_SHIFT;

pub type GhosttyCell = u64;
pub const CELL_DATA_CONTENT_TAG: i32 = 2;
pub const CELL_DATA_WIDE: i32 = 3;
pub const CELL_DATA_HAS_HYPERLINK: i32 = 7;
pub const CELL_DATA_COLOR_PALETTE: i32 = 10;
pub const CELL_DATA_COLOR_RGB: i32 = 11;
pub const CELL_CONTENT_BG_COLOR_PALETTE: i32 = 2;
pub const CELL_CONTENT_BG_COLOR_RGB: i32 = 3;
pub const CELL_WIDE_WIDE: i32 = 1;
pub const CELL_WIDE_SPACER_TAIL: i32 = 2;
pub const CELL_WIDE_SPACER_HEAD: i32 = 3;

#[repr(C)]
pub struct GhosttyTerminalScrollbar {
    pub total: u64,
    pub offset: u64,
    pub len: u64,
}

#[repr(C)]
pub struct GhosttyTerminalModeConfig {
    pub mode: u16,
    pub value: bool,
}

#[repr(C)]
pub struct GhosttyTerminalScrollViewport {
    pub tag: i32,
    pub value: [i64; 2],
}

pub const SCROLL_VIEWPORT_BOTTOM: i32 = 1;
pub const SCROLL_VIEWPORT_DELTA: i32 = 2;
pub const SCROLL_VIEWPORT_ROW: i32 = 3;

pub const OPT_USERDATA: i32 = 1;
pub const OPT_WRITE_PTY: i32 = 2;
pub const OPT_SCROLLBACK_MAX_BYTES: i32 = 28;
pub const OPT_SCROLLBACK_MAX_LINES: i32 = 29;

pub const DATA_CURSOR_X: i32 = 3;
pub const DATA_CURSOR_Y: i32 = 4;
pub const DATA_ACTIVE_SCREEN: i32 = 6;
pub const DATA_CURSOR_VISIBLE: i32 = 7;
pub const DATA_SCROLLBAR: i32 = 9;
pub const DATA_TOTAL_ROWS: i32 = 14;
pub const DATA_SCROLLBACK_ROWS: i32 = 15;
pub const DATA_MODE: i32 = 37;

/// `ghostty_mode_new(value, false)`: DEC private modes.
pub const MODE_CURSOR_KEYS: u16 = 1;
pub const MODE_BRACKETED_PASTE: u16 = 2004;

pub type WritePtyFn = unsafe extern "C" fn(GhosttyTerminal, *mut c_void, *const u8, usize);

extern "C" {
    pub fn ghostty_terminal_new(
        allocator: *const c_void,
        out: *mut GhosttyTerminal,
        cols: u16,
        rows: u16,
    ) -> GhosttyResult;
    pub fn ghostty_terminal_free(t: GhosttyTerminal);
    pub fn ghostty_terminal_resize(
        t: GhosttyTerminal,
        cols: u16,
        rows: u16,
        cell_width_px: u32,
        cell_height_px: u32,
    ) -> GhosttyResult;
    pub fn ghostty_terminal_set(
        t: GhosttyTerminal,
        option: i32,
        value: *const c_void,
    ) -> GhosttyResult;
    pub fn ghostty_terminal_get(t: GhosttyTerminal, data: i32, out: *mut c_void) -> GhosttyResult;
    pub fn ghostty_terminal_vt_write(t: GhosttyTerminal, data: *const u8, len: usize);
    pub fn ghostty_terminal_scroll_viewport(
        t: GhosttyTerminal,
        behavior: GhosttyTerminalScrollViewport,
    ) -> GhosttyResult;
    pub fn ghostty_terminal_grid_ref(
        t: GhosttyTerminal,
        point: GhosttyPoint,
        out: *mut GhosttyGridRef,
    ) -> GhosttyResult;
    pub fn ghostty_grid_ref_cell(r: *const GhosttyGridRef, out: *mut GhosttyCell) -> GhosttyResult;
    pub fn ghostty_grid_ref_style(
        r: *const GhosttyGridRef,
        out: *mut GhosttyStyle,
    ) -> GhosttyResult;
    pub fn ghostty_grid_ref_graphemes(
        r: *const GhosttyGridRef,
        buf: *mut u32,
        buf_len: usize,
        out_written: *mut usize,
    ) -> GhosttyResult;
    pub fn ghostty_grid_ref_hyperlink_uri(
        r: *const GhosttyGridRef,
        buf: *mut u8,
        buf_len: usize,
        out_written: *mut usize,
    ) -> GhosttyResult;
    pub fn ghostty_cell_get(cell: GhosttyCell, data: i32, out: *mut c_void) -> GhosttyResult;
}

#[cfg(test)]
mod layout {
    use super::*;
    use core::mem::{align_of, size_of};

    #[test]
    fn structs_match_the_c_header() {
        assert_eq!(size_of::<GhosttyGridRef>(), 16);
        assert_eq!(size_of::<GhosttyStyle>(), 16);
        assert_eq!(size_of::<GhosttyStyleColor>(), 4);
        assert_eq!(size_of::<GhosttyPoint>(), 24);
        assert_eq!(align_of::<GhosttyPoint>(), 8);
        assert_eq!(size_of::<GhosttyTerminalScrollViewport>(), 24);
        assert_eq!(size_of::<GhosttyTerminalScrollbar>(), 24);
        assert_eq!(size_of::<GhosttyTerminalModeConfig>(), 4);
    }
}
