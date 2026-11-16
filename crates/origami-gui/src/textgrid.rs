//! Draws a grid of text cells into RGBA pixels, for the serial console and
//! the log views. The window shows the result as one image, so the same
//! picture appears under any renderer and a test can read it back from a
//! screenshot.

use std::collections::HashMap;

pub type Rgb = [u8; 3];

/// DejaVu Sans Mono, built in so the views look the same on every host and
/// need no font lookup. Bold is the same glyphs struck twice.
static FONT: &[u8] = include_bytes!("../fonts/DejaVuSansMono.ttf");

pub const DEFAULT_FG: Rgb = [0xd0, 0xd0, 0xd0];
pub const DEFAULT_BG: Rgb = [0x10, 0x10, 0x10];

/// Logical pixel height of the text.
const FONT_PX: f32 = 15.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cell {
    pub ch: char,
    pub fg: Rgb,
    pub bg: Rgb,
    pub bold: bool,
    pub underline: bool,
}

impl Cell {
    pub fn plain(ch: char) -> Self {
        Cell {
            ch,
            fg: DEFAULT_FG,
            bg: DEFAULT_BG,
            bold: false,
            underline: false,
        }
    }
}

struct Glyph {
    width: usize,
    height: usize,
    /// Pixels from the cell's left edge to the bitmap's, and from the
    /// baseline up to the bitmap's bottom.
    xmin: i32,
    ymin: i32,
    coverage: Vec<u8>,
}

pub struct Painter {
    font: fontdue::Font,
    px: f32,
    cell_w: u32,
    cell_h: u32,
    ascent: i32,
    glyphs: HashMap<char, Glyph>,
}

impl Painter {
    /// A painter for a window whose scale factor is `scale`.
    pub fn new(scale: f32) -> Self {
        let font = fontdue::Font::from_bytes(FONT, fontdue::FontSettings::default())
            .expect("the built-in font parses");
        let px = (FONT_PX * scale.max(0.5)).round();
        let lines = font
            .horizontal_line_metrics(px)
            .expect("the built-in font has line metrics");
        let cell_w = font.metrics('M', px).advance_width.round().max(1.0) as u32;
        let cell_h = (lines.ascent - lines.descent + lines.line_gap)
            .ceil()
            .max(1.0) as u32;
        Painter {
            font,
            px,
            cell_w,
            cell_h,
            ascent: lines.ascent.round() as i32,
            glyphs: HashMap::new(),
        }
    }

    /// Width and height of one cell in pixels.
    pub fn cell_size(&self) -> (u32, u32) {
        (self.cell_w, self.cell_h)
    }

    /// Columns and rows that fit `width` x `height` pixels.
    pub fn grid_for(&self, width: u32, height: u32) -> (u16, u16) {
        let cols = (width / self.cell_w).clamp(1, 1000) as u16;
        let rows = (height / self.cell_h).clamp(1, 1000) as u16;
        (rows, cols)
    }

    fn glyph(&mut self, ch: char) -> &Glyph {
        if !self.glyphs.contains_key(&ch) {
            let ch_drawn = if self.font.lookup_glyph_index(ch) == 0 {
                '?'
            } else {
                ch
            };
            let (m, coverage) = self.font.rasterize(ch_drawn, self.px);
            self.glyphs.insert(
                ch,
                Glyph {
                    width: m.width,
                    height: m.height,
                    xmin: m.xmin,
                    ymin: m.ymin,
                    coverage,
                },
            );
        }
        &self.glyphs[&ch]
    }

    /// Paint `rows` x `cols` cells, as `cell` gives them, into a picture of
    /// `width` x `height` pixels (RGBA, 4 bytes a pixel). Cells that do not
    /// fill the picture leave a margin in the default background. `cursor`
    /// is drawn as a block.
    pub fn paint(
        &mut self,
        width: u32,
        height: u32,
        rows: u16,
        cols: u16,
        mut cell: impl FnMut(u16, u16) -> Cell,
        cursor: Option<(u16, u16)>,
    ) -> Vec<u8> {
        let (w, h) = (width as usize, height as usize);
        let mut px = vec![0u8; w * h * 4];
        for p in px.as_chunks_mut::<4>().0 {
            p.copy_from_slice(&[DEFAULT_BG[0], DEFAULT_BG[1], DEFAULT_BG[2], 255]);
        }
        let (cw, ch) = (self.cell_w as usize, self.cell_h as usize);
        for r in 0..rows {
            for c in 0..cols {
                let (x0, y0) = (c as usize * cw, r as usize * ch);
                if x0 + cw > w || y0 + ch > h {
                    continue;
                }
                let mut cell = cell(r, c);
                if cursor == Some((r, c)) {
                    std::mem::swap(&mut cell.fg, &mut cell.bg);
                }
                if cell.bg != DEFAULT_BG {
                    for y in y0..y0 + ch {
                        let row = &mut px[(y * w + x0) * 4..(y * w + x0 + cw) * 4];
                        for p in row.as_chunks_mut::<4>().0 {
                            p[..3].copy_from_slice(&cell.bg);
                        }
                    }
                }
                if cell.ch != ' ' && cell.ch != '\0' {
                    let baseline = y0 as i32 + self.ascent;
                    let (fg, bg, bold) = (cell.fg, cell.bg, cell.bold);
                    let g = self.glyph(cell.ch);
                    let (gx, gy) = (x0 as i32 + g.xmin, baseline - g.ymin - g.height as i32);
                    for pass in 0..=(bold as i32) {
                        for gy_off in 0..g.height {
                            let y = gy + gy_off as i32;
                            if y < y0 as i32 || y >= (y0 + ch) as i32 {
                                continue;
                            }
                            for gx_off in 0..g.width {
                                let x = gx + gx_off as i32 + pass;
                                if x < x0 as i32 || x >= (x0 + cw) as i32 {
                                    continue;
                                }
                                let cov = u32::from(g.coverage[gy_off * g.width + gx_off]);
                                if cov == 0 {
                                    continue;
                                }
                                let o = (y as usize * w + x as usize) * 4;
                                for k in 0..3 {
                                    let under = if pass == 0 {
                                        u32::from(bg[k])
                                    } else {
                                        u32::from(px[o + k])
                                    };
                                    let v = (under * (255 - cov) + u32::from(fg[k]) * cov) / 255;
                                    px[o + k] = v as u8;
                                }
                            }
                        }
                    }
                }
                if cell.underline {
                    let y = (y0 + (self.ascent as usize + 2).min(ch - 1)).min(h - 1);
                    for x in x0..x0 + cw {
                        px[(y * w + x) * 4..(y * w + x) * 4 + 3].copy_from_slice(&cell.fg);
                    }
                }
            }
        }
        px
    }
}

/// The colour of palette entry `i`: the sixteen standard colours, the 6x6x6
/// cube and the grey ramp, as xterm has them.
pub fn indexed(i: u8) -> Rgb {
    const BASE: [Rgb; 16] = [
        [0x00, 0x00, 0x00],
        [0xcd, 0x31, 0x31],
        [0x0d, 0xbc, 0x79],
        [0xe5, 0xe5, 0x10],
        [0x24, 0x72, 0xc8],
        [0xbc, 0x3f, 0xbc],
        [0x11, 0xa8, 0xcd],
        [0xe5, 0xe5, 0xe5],
        [0x66, 0x66, 0x66],
        [0xf1, 0x4c, 0x4c],
        [0x23, 0xd1, 0x8b],
        [0xf5, 0xf5, 0x43],
        [0x3b, 0x8e, 0xea],
        [0xd6, 0x70, 0xd6],
        [0x29, 0xb8, 0xdb],
        [0xff, 0xff, 0xff],
    ];
    match i {
        0..=15 => BASE[i as usize],
        16..=231 => {
            let n = i - 16;
            let level = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
            [level(n / 36), level((n / 6) % 6), level(n % 6)]
        }
        _ => {
            let v = 8 + (i - 232) * 10;
            [v, v, v]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cell_has_a_usable_size_and_the_grid_fits_a_view() {
        let p = Painter::new(1.0);
        let (w, h) = p.cell_size();
        assert!((7..=12).contains(&w), "cell width {w}");
        assert!((14..=24).contains(&h), "cell height {h}");
        let (rows, cols) = p.grid_for(900, 540);
        assert_eq!(cols as u32, 900 / w);
        assert_eq!(rows as u32, 540 / h);
        // A bigger scale makes bigger cells.
        assert!(Painter::new(2.0).cell_size().0 > w);
    }

    #[test]
    fn a_letter_marks_pixels_in_its_own_cell_only() {
        let mut p = Painter::new(1.0);
        let (cw, ch) = p.cell_size();
        let (w, h) = (cw * 3, ch * 2);
        let px = p.paint(
            w,
            h,
            2,
            3,
            |r, c| Cell::plain(if (r, c) == (1, 1) { 'W' } else { ' ' }),
            None,
        );
        let lit = |x0: u32, y0: u32| {
            (y0..y0 + ch)
                .flat_map(|y| (x0..x0 + cw).map(move |x| (x, y)))
                .filter(|&(x, y)| px[((y * w + x) * 4) as usize] > 0x40)
                .count()
        };
        assert!(lit(cw, ch) > 10, "the W drew {} pixels", lit(cw, ch));
        assert_eq!(lit(0, 0), 0);
        assert_eq!(lit(2 * cw, ch), 0);
    }

    #[test]
    fn the_cursor_and_a_coloured_background_fill_their_cell() {
        let mut p = Painter::new(1.0);
        let (cw, ch) = p.cell_size();
        let px = p.paint(
            cw * 2,
            ch,
            1,
            2,
            |_, c| Cell {
                bg: if c == 1 { [200, 0, 0] } else { DEFAULT_BG },
                ..Cell::plain(' ')
            },
            Some((0, 0)),
        );
        // The cursor cell is the default foreground, the other is red.
        assert_eq!(&px[0..3], &DEFAULT_FG);
        let o = (cw * 4) as usize;
        assert_eq!(&px[o..o + 3], &[200, 0, 0]);
    }

    #[test]
    fn indexed_colours_follow_xterm() {
        assert_eq!(indexed(1), [0xcd, 0x31, 0x31]);
        assert_eq!(indexed(16), [0, 0, 0]);
        assert_eq!(indexed(231), [255, 255, 255]);
        assert_eq!(indexed(232), [8, 8, 8]);
        assert_eq!(indexed(255), [238, 238, 238]);
    }
}
