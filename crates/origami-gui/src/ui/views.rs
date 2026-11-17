//! A machine's serial console, logs, status and controls: everything in the
//! window besides the picture. Part of [`super`], whose state it works on.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::Ordering;

use slint::ComponentHandle;

use super::Ui;
use crate::core::logs::{self, is_error_line, Follower, LogSelection};
use crate::core::status::{self, Facts, Reason};
use crate::core::terminal::{Colour, Terminal};
use crate::core::{ConsoleKind, ConsoleLink, Key, Power};
use crate::pointer::{AbsThrottle, PointerMode, RelAccum};
use crate::termkeys::{self, KeyAction, Mods};
use crate::textgrid::{self, Cell, Painter};
use origami_core::facade::{CoreError, OperationState, StopKind};
use origami_engine::Control;

/// Which of a machine's views a window shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    Display,
    Serial,
    Logs,
}

impl View {
    pub fn from_index(i: i32) -> View {
        match i {
            1 => View::Serial,
            2 => View::Logs,
            _ => View::Display,
        }
    }

    pub fn index(self) -> i32 {
        match self {
            View::Display => 0,
            View::Serial => 1,
            View::Logs => 2,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            View::Display => "display",
            View::Serial => "serial",
            View::Logs => "logs",
        }
    }
}

/// Entries of a log the viewer keeps.
const LOG_KEEP: usize = 5000;
/// Bytes of a log read on one tick, so a huge file cannot stall the window.
const LOG_BUDGET: usize = 4 << 20;
/// Smallest terminal: what a guest's console assumes.
const MIN_ROWS: u16 = 24;
const MIN_COLS: u16 = 80;

/// Where a log pane's entries sit in the text view's grid.
#[derive(Clone, Copy)]
struct PaneLayout {
    /// The header's row, with the entries' rows below it, `count` rows in all.
    first: u16,
    count: u16,
    /// The entries the rows show: `start..end`, the last possibly the line
    /// still being written.
    start: usize,
    end: usize,
}

/// What a machine has besides its picture.
pub struct Extra {
    pub view: View,
    pub term: Terminal,
    /// The terminal or a log changed since the picture of it was drawn.
    pub text_dirty: bool,
    /// Physical size of the area the text views are drawn in.
    pub text_area: (u32, u32),
    pub text_img: Option<slint::Image>,
    pub console: ConsoleLink,
    pub runner: Follower,
    pub serial_log: Follower,
    /// Lines scrolled back from the end, for the runner log and the serial log.
    pub log_scroll: [usize; 2],
    pub pointer: (f32, f32),
    pub wheel_fraction: f32,
    pub selecting: bool,
    /// The drag over a log, kept as entries and characters so it stays on
    /// its text while the view scrolls.
    pub log_sel: Option<LogSelection>,
    /// Copy a merged line with its repeat count.
    pub copy_counts: bool,
    /// How the pointer reaches the guest, and whether the host pointer is
    /// captured now (and how: "locked" or "confined").
    pub mode: PointerMode,
    pub captured: bool,
    pub grab: &'static str,
    /// Buttons whose release is the end of the click that captured.
    pub swallow: [bool; 3],
    /// Buttons the guest believes are down.
    pub held_buttons: [bool; 3],
    /// How many times each log has been emptied and read again, so a
    /// selection of the old lines can be dropped.
    pub log_clears: [u64; 2],
    /// Keys the guest believes are down.
    pub held: HashSet<Key>,
    pub rel: RelAccum,
    pub abs: AbsThrottle,
    pub abs_timer: slint::Timer,
    pub busy: String,
    pub busy_actions: u32,
    pub notice: String,
    pub console_kind: Option<ConsoleKind>,
    pub facts: Facts,
    pub status: String,
    pub debug_text: String,
    pub was_running: bool,
    /// What the windows were last given, so a tick that changed nothing sets
    /// no property: a property set can cost a redraw, and an idle machine
    /// draws nothing.
    pub pushed: String,
    /// Where the next console read starts in the facade's offset ring.
    pub console_from: u64,
}

impl Extra {
    pub fn new(dir: Option<PathBuf>) -> Extra {
        let dir = dir.unwrap_or_default();
        Extra {
            view: View::Display,
            term: Terminal::new(MIN_ROWS, MIN_COLS),
            text_dirty: true,
            text_area: (0, 0),
            text_img: None,
            console: ConsoleLink::Down("not connected yet".into()),
            runner: Follower::new(dir.join("logs/runner.log"), LOG_KEEP, false),
            serial_log: Follower::new(dir.join("logs/serial.log"), LOG_KEEP, true),
            log_scroll: [0, 0],
            pointer: (0.0, 0.0),
            wheel_fraction: 0.0,
            selecting: false,
            log_sel: None,
            copy_counts: false,
            mode: PointerMode::Capture,
            captured: false,
            grab: "",
            swallow: [false; 3],
            held_buttons: [false; 3],
            log_clears: [0; 2],
            held: HashSet::new(),
            rel: RelAccum::default(),
            abs: AbsThrottle::default(),
            abs_timer: slint::Timer::default(),
            busy: String::new(),
            busy_actions: 0,
            notice: String::new(),
            console_kind: None,
            facts: Facts::default(),
            status: String::new(),
            debug_text: String::new(),
            was_running: false,
            pushed: String::new(),
            console_from: 0,
        }
    }
}

fn paint_colour(c: Colour, default: textgrid::Rgb) -> textgrid::Rgb {
    match c {
        Colour::Default => default,
        Colour::Idx(i) => textgrid::indexed(i),
        Colour::Rgb(r, g, b) => [r, g, b],
    }
}

/// The most console output one poll takes from the facade's ring.
const CONSOLE_READ: usize = 1 << 20;

/// Carry out a power button on a machine, waiting for it to finish.
fn run_power(m: &origami_core::facade::Machine, action: Power) -> Result<(), CoreError> {
    let wait = |op: origami_core::facade::Operation| match op.wait(None) {
        OperationState::Failed(e) => Err(e),
        _ => Ok(()),
    };
    match action {
        Power::Start => wait(m.start()?),
        Power::Stop => wait(m.stop(StopKind::Clean)?),
        Power::ForceStop => wait(m.stop(StopKind::Force)?),
        Power::Reset => m.control(Control::Reset),
    }
}

impl Ui {
    /// The picture of the text views needs drawing again.
    pub fn mark_text(&mut self, mi: usize) {
        self.machines[mi].x.text_dirty = true;
    }

    fn ensure_painter(&mut self, scale: f32) {
        if (self.painter_scale - scale).abs() > 0.01 {
            self.painter = Painter::new(scale);
            self.painter_scale = scale;
            for m in &mut self.machines {
                m.x.text_dirty = true;
            }
        }
    }

    /// A window reported the size of its text view, in physical pixels.
    pub fn text_size(&mut self, win: usize, w: u32, h: u32) {
        self.text_px.insert(win, (w, h));
        let scale = self
            .shell_of_window(win)
            .map_or(1.0, |s| s.window().scale_factor());
        self.ensure_painter(scale);
        if let Some(mi) = self.machine_in(win) {
            self.adopt_text_area(mi);
        }
    }

    pub fn text_origin(&mut self, win: usize, x: f32, y: f32) {
        self.text_at.insert(win, (x, y));
    }

    fn shell_of_window(&self, win: usize) -> Option<&crate::Shell> {
        if win == 0 {
            Some(&self.main)
        } else {
            self.pops.get(&(win - 1))
        }
    }

    /// Give machine `mi` the text view size of the window that shows it.
    pub fn adopt_text_area(&mut self, mi: usize) {
        let Some(win) = self.win_for(mi) else { return };
        let Some(&(w, h)) = self.text_px.get(&win) else {
            return;
        };
        if w == 0 || h == 0 {
            return;
        }
        let (rows, cols) = self.painter.grid_for(w, h);
        let x = &mut self.machines[mi].x;
        x.text_area = (w, h);
        x.term.resize(rows.max(MIN_ROWS), cols.max(MIN_COLS));
        x.text_dirty = true;
        self.render_text(mi);
    }

    pub fn set_view(&mut self, mi: usize, v: View) {
        if self.machines[mi].x.view == v {
            return;
        }
        self.release_all(mi);
        self.machines[mi].x.view = v;
        self.machines[mi].x.text_dirty = true;
        if v == View::Logs {
            // Read what the files hold now, not at the next tick.
            self.poll_logs(mi, true);
        }
        // A window's text view reports its size when it appears; a window
        // that already knows one adopts it for this machine now.
        self.adopt_text_area(mi);
        self.update_visibility();
        self.push(mi);
        self.redraw(mi);
    }

    /// Draw the serial or log picture again, when something changed and a
    /// window shows it.
    pub fn render_text(&mut self, mi: usize) {
        let m = &self.machines[mi];
        if !m.x.text_dirty || m.x.view == View::Display || self.win_for(mi).is_none() {
            return;
        }
        let (w, h) = m.x.text_area;
        if w == 0 || h == 0 {
            return;
        }
        let px = match m.x.view {
            View::Serial => self.paint_serial(mi, w, h),
            _ => self.paint_logs(mi, w, h),
        };
        let mut buf = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(w, h);
        buf.make_mut_bytes().copy_from_slice(&px);
        let m = &mut self.machines[mi];
        m.x.text_img = Some(slint::Image::from_rgba8(buf));
        m.x.text_dirty = false;
        self.counters.text_renders.fetch_add(1, Ordering::Relaxed);
        self.push(mi);
        self.redraw(mi);
    }

    fn paint_serial(&mut self, mi: usize, w: u32, h: u32) -> Vec<u8> {
        let term = &self.machines[mi].x.term;
        let (rows, cols) = self.painter.grid_for(w, h);
        let cursor = (term.scrolled() == 0 && term.cursor_visible()).then(|| term.cursor());
        self.painter.paint(
            w,
            h,
            rows,
            cols,
            |r, c| {
                let Some(cell) = term.cell(r, c) else {
                    return Cell::plain(' ');
                };
                let mut fg = paint_colour(cell.fg, textgrid::DEFAULT_FG);
                let mut bg = paint_colour(cell.bg, textgrid::DEFAULT_BG);
                if cell.inverse ^ term.is_selected(r, c) {
                    std::mem::swap(&mut fg, &mut bg);
                }
                if cell.dim {
                    fg = fg.map(|v| (u32::from(v) * 2 / 3) as u8);
                }
                Cell {
                    ch: cell.text.chars().next().unwrap_or(' '),
                    fg,
                    bg,
                    bold: cell.bold,
                    underline: cell.underline,
                }
            },
            cursor,
        )
    }

    /// Where each log pane's entries sit in a grid of `rows`: its header row,
    /// the rows below it, and the entries those rows show.
    fn log_panes(&self, mi: usize, rows: u16) -> [PaneLayout; 2] {
        let x = &self.machines[mi].x;
        let top = rows / 2;
        let pane = |f: &Follower, first: u16, count: u16, scroll: usize| {
            let body = count.saturating_sub(1) as usize;
            let total = f.lines.len() + usize::from(f.pending().is_some());
            let end = total.saturating_sub(scroll.min(total.saturating_sub(1)));
            PaneLayout {
                first,
                count,
                start: end.saturating_sub(body),
                end,
            }
        };
        [
            pane(&x.runner, 0, top, x.log_scroll[0]),
            pane(&x.serial_log, top, rows - top, x.log_scroll[1]),
        ]
    }

    /// The logs: runner.log above, serial.log below, each with a header and
    /// the lines at the end of it unless scrolled back.
    fn paint_logs(&mut self, mi: usize, w: u32, h: u32) -> Vec<u8> {
        let (rows, cols) = self.painter.grid_for(w, h);
        let layout = self.log_panes(mi, rows);
        let x = &self.machines[mi].x;
        let panes: [(&str, &Follower, usize); 2] = [
            ("runner.log", &x.runner, 0),
            ("serial.log", &x.serial_log, 1),
        ];
        // The text of each row, drawn after the grid is laid out.
        let mut grid: Vec<Vec<Cell>> = (0..rows)
            .map(|_| vec![Cell::plain(' '); cols as usize])
            .collect();
        for (name, f, idx) in panes {
            let PaneLayout {
                first,
                count,
                start,
                end,
            } = layout[idx];
            if count == 0 {
                continue;
            }
            let scroll = x.log_scroll[idx];
            let head = format!(
                " {name}: {} lines written, {} shown after merging repeats{}{}",
                f.lines.raw_lines(),
                f.lines.len(),
                if scroll > 0 {
                    format!(", scrolled back {scroll}")
                } else {
                    String::new()
                },
                if f.path().exists() {
                    ""
                } else {
                    " (no such file yet)"
                }
            );
            let header = &mut grid[first as usize];
            header.fill(Cell {
                bg: [0x2b, 0x45, 0x66],
                ..Cell::plain(' ')
            });
            put(
                header,
                0,
                &head,
                [0xff, 0xff, 0xff],
                [0x2b, 0x45, 0x66],
                true,
            );
            let pending = f.pending();
            for (j, e) in (start..end).enumerate() {
                let row = &mut grid[first as usize + 1 + j];
                let (text, n, error) = match f.lines.get(e) {
                    Some(l) => (l.text.clone(), l.count, is_error_line(&l.text)),
                    None => (pending.clone().unwrap_or_default(), 1, false),
                };
                let suffix = if n > 1 {
                    format!(" \u{d7}{n}")
                } else {
                    String::new()
                };
                let room = (cols as usize).saturating_sub(suffix.chars().count() + 1);
                let shown: String = if text.chars().count() > room {
                    text.chars()
                        .take(room.saturating_sub(1))
                        .chain(Some('\u{2026}'))
                        .collect()
                } else {
                    text
                };
                let fg = if error {
                    [0xf1, 0x7c, 0x7c]
                } else {
                    textgrid::DEFAULT_FG
                };
                put(row, 0, &shown, fg, textgrid::DEFAULT_BG, false);
                if let (Some(sel), true) = (x.log_sel, e < f.lines.len()) {
                    let number = f.lines.first_number() + e as u64;
                    for (c, cell) in row.iter_mut().enumerate().take(shown.chars().count()) {
                        if sel.covers(idx, number, c) {
                            std::mem::swap(&mut cell.fg, &mut cell.bg);
                        }
                    }
                }
                if !suffix.is_empty() {
                    let at = cols as usize - suffix.chars().count();
                    put(
                        row,
                        at,
                        &suffix,
                        [0xe5, 0xc0, 0x7b],
                        textgrid::DEFAULT_BG,
                        true,
                    );
                }
            }
        }
        self.painter
            .paint(w, h, rows, cols, |r, c| grid[r as usize][c as usize], None)
    }

    /// The place in a log under view position (`px`, `py`) of the text view.
    /// With `pane` named, a position outside that pane is the nearest one in
    /// it, which is what a drag that leaves it wants.
    fn log_pos(
        &self,
        mi: usize,
        px: f32,
        py: f32,
        pane: Option<usize>,
    ) -> Option<(usize, logs::LogPos)> {
        let x = &self.machines[mi].x;
        let (cw, ch) = self.painter.cell_size();
        let (rows, cols) = self.painter.grid_for(x.text_area.0, x.text_area.1);
        if rows == 0 || cols == 0 {
            return None;
        }
        let row = ((py.max(0.0) as u32) / ch).min(u32::from(rows) - 1) as u16;
        let col = ((px.max(0.0) as u32) / cw).min(u32::from(cols) - 1) as usize;
        let layout = self.log_panes(mi, rows);
        let idx = pane.unwrap_or(usize::from(row >= rows / 2));
        let lay = layout[idx];
        if lay.count < 2 || lay.end == lay.start {
            return None;
        }
        let f = if idx == 0 { &x.runner } else { &x.serial_log };
        let body_first = lay.first + 1;
        let row = row.clamp(body_first, lay.first + lay.count - 1);
        // The partial line being written is not an entry yet.
        let entry = (lay.start + usize::from(row - body_first))
            .min(lay.end - 1)
            .min(f.lines.len().checked_sub(1)?);
        let chars = f.lines.get(entry)?.text.chars().count();
        Some((
            idx,
            (
                f.lines.first_number() + entry as u64,
                col.min(chars.saturating_sub(1)),
            ),
        ))
    }

    /// Select every entry of the log the pointer is over.
    pub fn select_all_log(&mut self, mi: usize) {
        let pane = self.log_pane(mi);
        let f = if pane == 0 {
            &self.machines[mi].x.runner
        } else {
            &self.machines[mi].x.serial_log
        };
        self.machines[mi].x.log_sel = LogSelection::all(pane, &f.lines);
        self.mark_text(mi);
        self.render_text(mi);
    }

    pub fn toggle_copy_counts(&mut self, mi: usize) {
        let x = &mut self.machines[mi].x;
        x.copy_counts = !x.copy_counts;
        self.push(mi);
    }

    fn copy_text(&mut self, mi: usize, text: Option<String>) {
        let Some(text) = text else {
            self.machines[mi].x.notice = "nothing selected; drag over the text first".into();
            self.push(mi);
            return;
        };
        let result = self
            .clipboard()
            .and_then(|c| c.set_text(text.clone()).map_err(|e| e.to_string()));
        self.machines[mi].x.notice = match result {
            Ok(()) => format!("copied {} characters", text.chars().count()),
            Err(e) => format!("copy failed: {e}"),
        };
        self.copied = Some(text);
        self.push(mi);
    }

    /// A key pressed in a window's text view.
    pub fn text_key(&mut self, win: usize, text: &str, mods: Mods) {
        let Some(mi) = self.machine_in(win) else {
            return;
        };
        match self.machines[mi].x.view {
            View::Serial => {
                let modes = self.machines[mi].x.term.key_modes();
                match termkeys::key_action(text, mods, modes) {
                    KeyAction::Send(bytes) => {
                        self.machines[mi].x.term.scroll_to_end();
                        self.machines[mi].x.term.clear_selection();
                        self.mark_text(mi);
                        self.send_console(mi, &bytes);
                        self.render_text(mi);
                    }
                    KeyAction::Copy => self.copy_selection(mi),
                    KeyAction::Paste => self.paste(mi),
                    KeyAction::Page(n) => {
                        let rows = self.machines[mi].x.term.size().0 as isize;
                        self.machines[mi].x.term.scroll(n * (rows - 1).max(1));
                        self.mark_text(mi);
                        self.render_text(mi);
                    }
                    KeyAction::Ignore => {}
                }
            }
            View::Logs => {
                use slint::platform::Key as K;
                let pane = self.log_pane(mi);
                let page = self.log_page(mi) as isize;
                let c = text.chars().next().unwrap_or('\0');
                // A log is not typed into, so Ctrl+C is copy here, and Ctrl+A
                // (Command on a Mac) selects the log under the pointer.
                if mods.command || mods.ctrl {
                    match c.to_ascii_lowercase() {
                        'c' | '\u{3}' => return self.copy_selection(mi),
                        'a' | '\u{1}' => return self.select_all_log(mi),
                        _ => {}
                    }
                }
                let delta = if c == char::from(K::PageUp) {
                    page
                } else if c == char::from(K::PageDown) {
                    -page
                } else if c == char::from(K::UpArrow) {
                    1
                } else if c == char::from(K::DownArrow) {
                    -1
                } else if c == char::from(K::Home) {
                    isize::MAX / 2
                } else if c == char::from(K::End) {
                    isize::MIN / 2
                } else {
                    return;
                };
                self.scroll_log(mi, pane, delta);
            }
            View::Display => {}
        }
    }

    /// Take what the serial console has sent into the terminal.
    pub fn feed_console(&mut self, mi: usize) {
        let m = &mut self.machines[mi];
        if m.x.term.history_end().is_none() {
            // What the console holds now is history: its queries get no answer.
            m.x.term
                .set_history_end(m.fm.console_read(u64::MAX, 0).next);
        }
        let mut chunk = m.fm.console_read(m.x.console_from, CONSOLE_READ);
        if chunk.next < m.x.console_from {
            // The machine started again and its offsets began from zero.
            chunk = m.fm.console_read(0, CONSOLE_READ);
            m.x.term.set_history_end(0);
        }
        m.x.console_from = chunk.next;
        if !chunk.bytes.is_empty() {
            let start = chunk.next - chunk.bytes.len() as u64;
            let replies = m.x.term.feed_at(start, &chunk.bytes);
            if !replies.is_empty() {
                let _ = m.fm.console_send(&replies);
            }
            m.x.text_dirty = true;
            self.render_text(mi);
        }
    }

    /// The serial console's connection, as the facade last saw it.
    fn console_link(&mut self, mi: usize) {
        let console = self.machines[mi].fm.view().console;
        let link = if console.connected {
            ConsoleLink::Connected
        } else {
            ConsoleLink::Down(console.down_reason.unwrap_or_default())
        };
        let x = &mut self.machines[mi].x;
        if x.console != link {
            x.console = link;
            x.text_dirty = true;
        }
    }

    pub fn send_console(&mut self, mi: usize, bytes: &[u8]) {
        match self.machines[mi].fm.console_send(bytes) {
            Ok(()) => {
                self.counters
                    .console_bytes_sent
                    .fetch_add(bytes.len() as u64, Ordering::Relaxed);
                if !self.machines[mi].x.notice.is_empty() {
                    self.machines[mi].x.notice.clear();
                    self.push(mi);
                }
            }
            Err(e) => {
                self.machines[mi].x.notice = e.to_string();
                self.push(mi);
            }
        }
    }

    fn clipboard(&mut self) -> Result<&mut arboard::Clipboard, String> {
        if self.clipboard.is_none() {
            self.clipboard = Some(arboard::Clipboard::new().map_err(|e| e.to_string())?);
        }
        Ok(self.clipboard.as_mut().unwrap())
    }

    pub fn copy_selection(&mut self, mi: usize) {
        let x = &self.machines[mi].x;
        let text = match (x.view, x.log_sel) {
            (View::Logs, Some(sel)) => {
                let f = if sel.pane == 0 {
                    &x.runner
                } else {
                    &x.serial_log
                };
                logs::selected_text(&f.lines, &sel, x.copy_counts)
            }
            (View::Logs, None) => None,
            _ => x.term.selected_text(),
        };
        self.copy_text(mi, text);
    }

    pub fn paste(&mut self, mi: usize) {
        let text = self
            .clipboard()
            .and_then(|c| c.get_text().map_err(|e| e.to_string()));
        match text {
            Ok(text) if !text.is_empty() => {
                let bytes = self.machines[mi].x.term.paste_bytes(&text);
                self.machines[mi].x.term.scroll_to_end();
                self.mark_text(mi);
                self.send_console(mi, &bytes);
                self.render_text(mi);
            }
            Ok(_) => {
                self.machines[mi].x.notice = "the clipboard has no text".into();
                self.push(mi);
            }
            Err(e) => {
                self.machines[mi].x.notice = format!("paste failed: {e}");
                self.push(mi);
            }
        }
    }

    fn cell_at(&self, mi: usize, px: f32, py: f32) -> (u16, u16) {
        let (cw, ch) = self.painter.cell_size();
        let (rows, cols) = self.machines[mi].x.term.size();
        let col = ((px.max(0.0) as u32) / cw).min(u32::from(cols) - 1) as u16;
        let row = ((py.max(0.0) as u32) / ch).min(u32::from(rows) - 1) as u16;
        (row, col)
    }

    pub fn text_down(&mut self, win: usize, x: f32, y: f32) {
        let Some(mi) = self.machine_in(win) else {
            return;
        };
        self.machines[mi].x.pointer = (x, y);
        match self.machines[mi].x.view {
            View::Serial => {
                let cell = self.cell_at(mi, x, y);
                let t = &mut self.machines[mi].x;
                t.term.select_from(cell);
                t.selecting = true;
                self.mark_text(mi);
                self.render_text(mi);
            }
            View::Logs => {
                let sel = self
                    .log_pos(mi, x, y, None)
                    .map(|(pane, pos)| LogSelection {
                        pane,
                        anchor: pos,
                        head: pos,
                    });
                let t = &mut self.machines[mi].x;
                t.log_sel = sel;
                t.selecting = sel.is_some();
                self.mark_text(mi);
                self.render_text(mi);
            }
            View::Display => {}
        }
    }

    pub fn text_moved(&mut self, win: usize, x: f32, y: f32) {
        let Some(mi) = self.machine_in(win) else {
            return;
        };
        self.machines[mi].x.pointer = (x, y);
        if self.machines[mi].x.selecting {
            if self.machines[mi].x.view == View::Logs {
                if let Some(sel) = self.machines[mi].x.log_sel {
                    if let Some((_, pos)) = self.log_pos(mi, x, y, Some(sel.pane)) {
                        self.machines[mi].x.log_sel = Some(LogSelection { head: pos, ..sel });
                    }
                }
            } else {
                let cell = self.cell_at(mi, x, y);
                self.machines[mi].x.term.select_to(cell);
            }
            self.mark_text(mi);
            self.render_text(mi);
        }
    }

    pub fn text_up(&mut self, win: usize) {
        if let Some(mi) = self.machine_in(win) {
            self.machines[mi].x.selecting = false;
        }
    }

    pub fn text_wheel(&mut self, win: usize, dy: f32) {
        let Some(mi) = self.machine_in(win) else {
            return;
        };
        let x = &mut self.machines[mi].x;
        x.wheel_fraction += dy / 40.0;
        let lines = x.wheel_fraction.trunc();
        if lines == 0.0 {
            return;
        }
        x.wheel_fraction -= lines;
        match x.view {
            View::Serial => {
                x.term.scroll(lines as isize);
                self.mark_text(mi);
                self.render_text(mi);
            }
            View::Logs => {
                let pane = self.log_pane(mi);
                self.scroll_log(mi, pane, lines as isize);
            }
            View::Display => {}
        }
    }

    /// The log pane the pointer is over.
    fn log_pane(&self, mi: usize) -> usize {
        let (_, ch) = self.painter.cell_size();
        let (rows, _) = self.painter.grid_for(
            self.machines[mi].x.text_area.0,
            self.machines[mi].x.text_area.1,
        );
        usize::from(self.machines[mi].x.pointer.1 / ch as f32 >= f32::from(rows / 2))
    }

    fn log_page(&self, mi: usize) -> usize {
        let (_, h) = self.machines[mi].x.text_area;
        let (_, ch) = self.painter.cell_size();
        ((h / ch / 2) as usize).saturating_sub(2).max(1)
    }

    fn scroll_log(&mut self, mi: usize, pane: usize, delta: isize) {
        let x = &mut self.machines[mi].x;
        let total = if pane == 0 {
            x.runner.lines.len()
        } else {
            x.serial_log.lines.len()
        };
        let now = x.log_scroll[pane] as isize;
        x.log_scroll[pane] =
            (now.saturating_add(delta)).clamp(0, total.saturating_sub(1) as isize) as usize;
        self.mark_text(mi);
        self.render_text(mi);
    }

    // ---- logs and status ------------------------------------------------

    /// Read what the logs gained. The runner log is always followed, since
    /// its last error explains a blank display; the serial log only while
    /// someone looks at it (or `all`).
    pub fn poll_logs(&mut self, mi: usize, all: bool) {
        let x = &mut self.machines[mi].x;
        let mut changed = x.runner.poll(LOG_BUDGET);
        if all || x.view == View::Logs {
            changed |= x.serial_log.poll(LOG_BUDGET);
        }
        // A log read again from its start numbers its entries afresh.
        for (idx, f) in [&x.runner, &x.serial_log].into_iter().enumerate() {
            if f.lines.clears() != x.log_clears[idx] {
                x.log_clears[idx] = f.lines.clears();
                if x.log_sel.is_some_and(|s| s.pane == idx) {
                    x.log_sel = None;
                    x.text_dirty = true;
                }
            }
        }
        if changed && x.view == View::Logs {
            // Following the end: new lines move what a scrolled view shows
            // only when it is at the end.
            x.text_dirty = true;
        }
    }

    /// The state the status line, controls and debug panel describe, read
    /// afresh. Called about twice a second for every machine.
    pub fn tick_machine(&mut self, mi: usize) {
        self.poll_logs(mi, false);
        self.console_link(mi);
        let mut facts = self.engine.facts(self.machines[mi].dir());
        let x = &mut self.machines[mi].x;
        facts.runner_error = x
            .runner
            .lines
            .last_where(is_error_line)
            .map(|l| (l.text.clone(), l.count));
        let running = facts.running;
        let was = x.was_running;
        x.was_running = running;
        x.facts = facts;
        if !running && (x.captured || !x.held.is_empty()) {
            self.release_all(mi);
        }
        if was && !running {
            // A stopped machine takes the console setting again.
            self.read_console_kind(mi);
        }
        self.machines[mi].x.status = self.status_text(mi);
        if self.debug {
            let mut lines = self.engine.debug(self.machines[mi].dir());
            let console = self.machines[mi].fm.view().console;
            lines.push(format!(
                "serial: {} | {} bytes received",
                if console.connected {
                    "connected".to_string()
                } else {
                    format!("down ({})", console.down_reason.unwrap_or_default())
                },
                console.total
            ));
            let c = &self.counters;
            let g = |a: &std::sync::atomic::AtomicU64| a.load(Ordering::Relaxed);
            lines.push(format!(
                "viewer: {} wakes | {} frames taken ({} imported) | {} rows uploaded | {} text renders | {} console bytes sent",
                g(&c.wakes),
                g(&c.frames_taken),
                g(&c.frames_imported),
                g(&c.rows_uploaded),
                g(&c.text_renders),
                g(&c.console_bytes_sent)
            ));
            let x = &mut self.machines[mi].x;
            lines.push(format!(
                "view {} | terminal {}x{} | {} lines of history | reason: {:?}",
                x.view.name(),
                x.term.size().1,
                x.term.size().0,
                x.term.history(),
                status::stage(&x.facts)
            ));
            x.debug_text = lines.join("\n");
        }
        let x = &mut self.machines[mi].x;
        let signature = format!(
            "{}|{}|{}|{}|{}|{:?}|{}",
            x.status,
            x.busy,
            x.notice,
            x.facts.running,
            x.debug_text,
            x.console_kind,
            x.view.name()
        );
        if signature != x.pushed {
            x.pushed = signature;
            self.push(mi);
            self.push_tabs();
        }
        self.render_text(mi);
    }

    pub fn zero_copy_text(&self, mi: usize) -> String {
        match self.engine.display_backend(self.machines[mi].dir()) {
            Some(b) if b.contains("import") => format!("zero copy: yes, {b}"),
            Some(b) => format!("zero copy: no, {b}"),
            None => format!(
                "zero copy: not in use yet; this GPU offers {}",
                self.gpu.import.describe()
            ),
        }
    }

    /// The status line, from the cached facts.
    pub fn status_text(&self, mi: usize) -> String {
        let m = &self.machines[mi];
        let scale = self.guest_scale(mi);
        let picture = match status::stage(&m.x.facts) {
            // The display's own view knows its scale; another view has no
            // area for it to be drawn in.
            Reason::Showing if m.x.view == View::Display => {
                let (w, h) = m.x.facts.picture.unwrap_or_default();
                format!("{w}x{h}, shown at {scale:.2}x")
            }
            _ => status::reason(&m.x.facts),
        };
        let text = match m.x.view {
            View::Display => {
                let hint = if matches!(status::stage(&m.x.facts), Reason::Showing) {
                    format!("{} | ", self.pointer_hint(mi))
                } else {
                    String::new()
                };
                format!("{hint}{picture} | {}", self.zero_copy_text(mi))
            }
            View::Serial => {
                let link = match &m.x.console {
                    ConsoleLink::Connected => "connected".to_string(),
                    ConsoleLink::Down(why) if why.is_empty() => "not connected".to_string(),
                    ConsoleLink::Down(why) => format!("not connected ({why})"),
                };
                format!(
                    "serial console {link}; {}x{}, {} lines of scrollback{} | display: {picture}",
                    m.x.term.size().1,
                    m.x.term.size().0,
                    m.x.term.history(),
                    if m.x.term.scrolled() > 0 {
                        format!(", scrolled back {}", m.x.term.scrolled())
                    } else {
                        String::new()
                    }
                )
            }
            View::Logs => format!(
                "logs under {} | display: {picture}",
                m.dir().join("logs").display()
            ),
        };
        text
    }

    // ---- controls -------------------------------------------------------

    pub fn power(&mut self, mi: usize, action: Power) {
        let label = match action {
            Power::Start => "starting...",
            Power::Stop => "stopping (a CHD disk can take minutes to fold)...",
            Power::ForceStop => "force stopping...",
            Power::Reset => "resetting...",
        };
        let x = &mut self.machines[mi].x;
        x.busy = label.to_string();
        x.busy_actions += 1;
        x.notice.clear();
        let fm = self.machines[mi].fm.clone();
        self.push(mi);
        std::thread::spawn(move || {
            let result = run_power(&fm, action).map_err(|e| e.to_string());
            let _ = slint::invoke_from_event_loop(move || {
                super::with_ui(move |u| u.power_done(mi, action, result))
            });
        });
    }

    fn power_done(&mut self, mi: usize, action: Power, result: Result<(), String>) {
        let x = &mut self.machines[mi].x;
        x.busy_actions = x.busy_actions.saturating_sub(1);
        if x.busy_actions == 0 {
            x.busy.clear();
        }
        x.notice = match (&result, action) {
            (Err(e), _) => format!("{action:?} failed: {e}"),
            (Ok(()), Power::Reset) => "reset sent".into(),
            (Ok(()), Power::Start) => "started".into(),
            (Ok(()), _) => "stopped".into(),
        };
        if result.is_ok() && matches!(action, Power::Stop | Power::ForceStop) {
            self.read_console_kind(mi);
        }
        self.tick_machine(mi);
        // Wake the display reader's view of the world.
        self.engine
            .set_visible(self.machines[mi].dir(), self.machines[mi].visible);
    }

    /// Read the PROM's console setting off the UI thread: it runs the
    /// product's `nvram` command, which works only on a stopped machine.
    pub fn read_console_kind(&mut self, mi: usize) {
        let (engine, dir) = (self.engine.clone(), self.machines[mi].dir().to_path_buf());
        std::thread::spawn(move || {
            let result = engine.console_kind(&dir).map_err(|e| e.to_string());
            let _ = slint::invoke_from_event_loop(move || {
                super::with_ui(move |u| {
                    if mi < u.machines.len() {
                        u.machines[mi].x.console_kind = result.ok();
                        u.push(mi);
                    }
                })
            });
        });
    }

    pub fn set_console(&mut self, mi: usize, kind: ConsoleKind) {
        let (engine, dir) = (self.engine.clone(), self.machines[mi].dir().to_path_buf());
        let x = &mut self.machines[mi].x;
        x.busy = "setting the PROM console...".into();
        x.busy_actions += 1;
        x.notice.clear();
        self.push(mi);
        std::thread::spawn(move || {
            let result = engine
                .set_console_kind(&dir, kind)
                .and_then(|()| engine.console_kind(&dir))
                .map_err(|e| e.to_string());
            let _ = slint::invoke_from_event_loop(move || {
                super::with_ui(move |u| {
                    if mi >= u.machines.len() {
                        return;
                    }
                    let x = &mut u.machines[mi].x;
                    x.busy_actions = x.busy_actions.saturating_sub(1);
                    if x.busy_actions == 0 {
                        x.busy.clear();
                    }
                    match result {
                        Ok(k) => {
                            x.console_kind = Some(k);
                            x.notice = format!(
                                "the PROM's console is now {}; it takes effect when the machine starts",
                                if k == ConsoleKind::Serial { "serial" } else { "graphics" }
                            );
                        }
                        Err(e) => x.notice = format!("could not set the console: {e}"),
                    }
                    u.push(mi);
                })
            });
        });
    }

    pub fn toggle_debug(&mut self) {
        self.debug = !self.debug;
        for mi in 0..self.machines.len() {
            self.tick_machine(mi);
        }
    }

    /// The Shell properties this module owns, for the windows that show
    /// machine `mi`.
    pub fn push_views(&self, mi: usize, shells: &[&crate::Shell]) {
        let m = &self.machines[mi];
        for w in shells {
            w.set_view(m.x.view.index());
            w.set_text_img(m.x.text_img.clone().unwrap_or_default());
            w.set_running(m.x.facts.running);
            w.set_busy(m.x.busy.clone().into());
            w.set_notice(m.x.notice.clone().into());
            w.set_console_kind(match m.x.console_kind {
                None => -1,
                Some(ConsoleKind::Serial) => 0,
                Some(ConsoleKind::Graphics) => 1,
            });
            w.set_debug(self.debug);
            w.set_debug_text(m.x.debug_text.clone().into());
        }
    }

    /// Everything a test or a bug report wants to know about the machines'
    /// views, as JSON.
    pub fn views_json(&self) -> serde_json::Value {
        let lines = |f: &Follower| {
            let n = f.lines.len();
            (n.saturating_sub(40)..n)
                .filter_map(|i| f.lines.get(i))
                .map(|l| serde_json::json!({"text": l.text, "count": l.count}))
                .collect::<Vec<_>>()
        };
        serde_json::Value::Array(
            self.machines
                .iter()
                .enumerate()
                .map(|(mi, m)| {
                    let buttons: Vec<_> = {
                        use slint::Model;
                        self.shell_for(mi)
                            .map(|s| {
                                s.get_buttons()
                                    .iter()
                                    .map(|b| serde_json::json!({"name": b.0.to_string(), "x": b.1, "y": b.2}))
                                    .collect()
                            })
                            .unwrap_or_default()
                    };
                    serde_json::json!({
                        "name": m.name,
                        "view": m.x.view.name(),
                        "running": m.x.facts.running,
                        "status": m.x.status,
                        "busy": m.x.busy,
                        "notice": m.x.notice,
                        "console_kind": m.x.console_kind.map(|k| if k == ConsoleKind::Serial { "serial" } else { "graphics" }),
                        "serial_connected": m.x.console == ConsoleLink::Connected,
                        "serial_screen": m.x.term.screen_text(),
                        "serial_received": m.x.term.received,
                        "serial_scrolled": m.x.term.scrolled(),
                        "serial_history": m.x.term.history(),
                        "selection": m.x.term.selected_text(),
                        "log_selection": m.x.log_sel.and_then(|s| {
                            let f = if s.pane == 0 { &m.x.runner } else { &m.x.serial_log };
                            logs::selected_text(&f.lines, &s, false)
                        }),
                        "pointer_mode": if m.x.mode == PointerMode::Absolute { "absolute" } else { "capture" },
                        "captured": m.x.captured,
                        "grab": m.x.grab,
                        "copy_counts": m.x.copy_counts,
                        "copied": self.copied,
                        "runner_log": lines(&m.x.runner),
                        "serial_log": lines(&m.x.serial_log),
                        "debug": self.debug,
                        "debug_text": m.x.debug_text,
                        "buttons": buttons,
                        "text_view": self.win_for(mi).and_then(|w| self.text_at.get(&w)).map(|p| serde_json::json!({"x": p.0, "y": p.1})),
                        "cell": {"w": self.painter.cell_size().0, "h": self.painter.cell_size().1, "scale": self.painter_scale},
                        "term_rows": m.x.term.size().0,
                        "term_cols": m.x.term.size().1,
                    })
                })
                .collect(),
        )
    }
}

/// Write `text` into `row` from column `at`, in the given colours.
fn put(row: &mut [Cell], at: usize, text: &str, fg: textgrid::Rgb, bg: textgrid::Rgb, bold: bool) {
    for (i, ch) in text.chars().enumerate() {
        if let Some(cell) = row.get_mut(at + i) {
            *cell = Cell {
                ch,
                fg,
                bg,
                bold,
                underline: false,
            };
        }
    }
}
