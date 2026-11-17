//! The Console window: a machine's serial console as a terminal, drawn with
//! cairo and Pango over the core's terminal model (vt100 screen, scrollback,
//! selection, paste). One window per machine, pulled when the core says the
//! console has news, so a closed or quiet console costs nothing.

use crate::app::App;
use crate::machine::MachineUi;
use adw::prelude::*;
use gtk::{gdk, glib, pango};
use origami_terminal::{key_bytes as term_key_bytes, Key, Terminal};
use std::cell::RefCell;
use std::rc::Rc;

const FONT: &str = "JetBrains Mono, DejaVu Sans Mono, monospace 11";

fn palette(i: u8) -> (f64, f64, f64) {
    const BASE: [[u8; 3]; 16] = [
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
    let c = match i {
        0..=15 => BASE[i as usize],
        16..=231 => {
            let n = i - 16;
            let l = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
            [l(n / 36), l((n / 6) % 6), l(n % 6)]
        }
        _ => {
            let v = 8 + (i - 232) * 10;
            [v, v, v]
        }
    };
    (
        f64::from(c[0]) / 255.0,
        f64::from(c[1]) / 255.0,
        f64::from(c[2]) / 255.0,
    )
}

fn colour(c: vt100::Color, default: (f64, f64, f64)) -> (f64, f64, f64) {
    match c {
        vt100::Color::Default => default,
        vt100::Color::Idx(i) => palette(i),
        vt100::Color::Rgb(r, g, b) => (
            f64::from(r) / 255.0,
            f64::from(g) / 255.0,
            f64::from(b) / 255.0,
        ),
    }
}

/// The bytes a key sends a serial terminal, or None when it sends nothing.
pub fn key_bytes(key: gdk::Key, state: gdk::ModifierType, app_cursor: bool) -> Option<Vec<u8>> {
    let named = match key {
        gdk::Key::Up => Key::Up,
        gdk::Key::Down => Key::Down,
        gdk::Key::Right => Key::Right,
        gdk::Key::Left => Key::Left,
        gdk::Key::Home => Key::Home,
        gdk::Key::End => Key::End,
        gdk::Key::Page_Up => Key::PageUp,
        gdk::Key::Page_Down => Key::PageDown,
        gdk::Key::Insert => Key::Insert,
        gdk::Key::Delete => Key::Delete,
        gdk::Key::F1 => Key::F(1),
        gdk::Key::F2 => Key::F(2),
        gdk::Key::F3 => Key::F(3),
        gdk::Key::F4 => Key::F(4),
        gdk::Key::F5 => Key::F(5),
        gdk::Key::F6 => Key::F(6),
        gdk::Key::F7 => Key::F(7),
        gdk::Key::F8 => Key::F(8),
        gdk::Key::F9 => Key::F(9),
        gdk::Key::F10 => Key::F(10),
        gdk::Key::F11 => Key::F(11),
        gdk::Key::F12 => Key::F(12),
        gdk::Key::ISO_Left_Tab => Key::BackTab,
        gdk::Key::Return | gdk::Key::KP_Enter => Key::Enter,
        gdk::Key::BackSpace => Key::Backspace,
        gdk::Key::Tab => Key::Tab,
        gdk::Key::Escape => Key::Escape,
        _ => {
            let ch = key.to_unicode().filter(|c| !c.is_control())?;
            Key::Char(ch)
        }
    };
    let ctrl = state.contains(gdk::ModifierType::CONTROL_MASK);
    let alt = state.contains(gdk::ModifierType::ALT_MASK);
    term_key_bytes(named, ctrl, alt, app_cursor)
}

struct View {
    term: RefCell<Terminal>,
    cell: std::cell::Cell<(f64, f64)>,
    /// Where to continue reading the guest's output.
    next: std::cell::Cell<u64>,
}

pub fn present(app: &Rc<App>, m: &Rc<MachineUi>) {
    if let Some(w) = m.console.borrow().as_ref() {
        w.present();
        return;
    }
    let area = gtk::DrawingArea::builder()
        .hexpand(true)
        .vexpand(true)
        .focusable(true)
        .can_focus(true)
        .build();
    area.update_property(&[gtk::accessible::Property::Label(&format!(
        "{} console",
        m.name
    ))]);
    let view = Rc::new(View {
        term: RefCell::new(Terminal::new(24, 80)),
        cell: std::cell::Cell::new((9.0, 18.0)),
        next: std::cell::Cell::new(0),
    });
    let font = pango::FontDescription::from_string(FONT);

    {
        let (view, font) = (view.clone(), font.clone());
        area.set_draw_func(move |area, cr, w, h| {
            let pl = area.create_pango_layout(None);
            pl.set_font_description(Some(&font));
            pl.set_text("M");
            let (pw, ph) = pl.pixel_size();
            let (cw, ch) = (f64::from(pw.max(1)), f64::from(ph.max(1)));
            view.cell.set((cw, ch));
            let (rows, cols) = (
                (f64::from(h) / ch).floor().max(1.0) as u16,
                (f64::from(w) / cw).floor().max(1.0) as u16,
            );
            if view.term.borrow().size() != (rows, cols) {
                view.term.borrow_mut().resize(rows, cols);
            }
            let term = view.term.borrow();
            let screen = term.screen();
            let (bg, fg) = ((0.07, 0.07, 0.08), (0.85, 0.85, 0.85));
            cr.set_source_rgb(bg.0, bg.1, bg.2);
            let _ = cr.paint();
            for r in 0..rows {
                for c in 0..cols {
                    let Some(cell) = screen.cell(r, c) else {
                        continue;
                    };
                    let (mut f, mut b) = (colour(cell.fgcolor(), fg), colour(cell.bgcolor(), bg));
                    if cell.inverse() ^ term.is_selected(r, c) {
                        std::mem::swap(&mut f, &mut b);
                    }
                    let (x, y) = (f64::from(c) * cw, f64::from(r) * ch);
                    if b != bg {
                        cr.set_source_rgb(b.0, b.1, b.2);
                        cr.rectangle(x, y, cw, ch);
                        let _ = cr.fill();
                    }
                    let text = cell.contents();
                    if !text.is_empty() && text != " " {
                        let l = area.create_pango_layout(Some(text));
                        let mut fd = font.clone();
                        if cell.bold() {
                            fd.set_weight(pango::Weight::Bold);
                        }
                        l.set_font_description(Some(&fd));
                        cr.set_source_rgb(f.0, f.1, f.2);
                        cr.move_to(x, y);
                        pangocairo_show(cr, &l);
                    }
                }
            }
            if !screen.hide_cursor() && term.scrolled() == 0 {
                let (cr_r, cr_c) = screen.cursor_position();
                cr.set_source_rgba(0.9, 0.9, 0.9, 0.6);
                cr.rectangle(f64::from(cr_c) * cw, f64::from(cr_r) * ch, cw, ch);
                let _ = cr.fill();
            }
        });
    }

    // Keys to the guest, paste and copy.
    let key = gtk::EventControllerKey::new();
    {
        let (m, view, area2) = (m.clone(), view.clone(), area.clone());
        key.connect_key_pressed(move |_, keyval, _, state| {
            let shift = state.contains(gdk::ModifierType::SHIFT_MASK);
            let ctrl = state.contains(gdk::ModifierType::CONTROL_MASK);
            if ctrl && shift && matches!(keyval, gdk::Key::C | gdk::Key::c) {
                if let Some(t) = view.term.borrow().selected_text() {
                    area2.clipboard().set_text(&t);
                }
                return glib::Propagation::Stop;
            }
            if (ctrl && shift && matches!(keyval, gdk::Key::V | gdk::Key::v))
                || (shift && keyval == gdk::Key::Insert)
            {
                let (m, view) = (m.clone(), view.clone());
                let cb = area2.clipboard();
                glib::spawn_future_local(async move {
                    if let Ok(Some(text)) = cb.read_text_future().await {
                        let bytes = view.term.borrow().paste_bytes(&text);
                        let _ = m.machine.console_send(&bytes);
                    }
                });
                return glib::Propagation::Stop;
            }
            if shift && matches!(keyval, gdk::Key::Page_Up | gdk::Key::Page_Down) {
                let rows = view.term.borrow().size().0 as isize - 1;
                view.term
                    .borrow_mut()
                    .scroll(if keyval == gdk::Key::Page_Up {
                        rows
                    } else {
                        -rows
                    });
                area2.queue_draw();
                return glib::Propagation::Stop;
            }
            let app_cursor = view.term.borrow().app_cursor();
            match key_bytes(keyval, state, app_cursor) {
                Some(b) => {
                    view.term.borrow_mut().scroll_to_end();
                    let _ = m.machine.console_send(&b);
                    glib::Propagation::Stop
                }
                None => glib::Propagation::Proceed,
            }
        });
    }
    area.add_controller(key);

    let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
    {
        let (view, a) = (view.clone(), area.clone());
        scroll.connect_scroll(move |_, _, dy| {
            view.term.borrow_mut().scroll(-(dy * 3.0) as isize);
            a.queue_draw();
            glib::Propagation::Stop
        });
    }
    area.add_controller(scroll);

    let drag = gtk::GestureDrag::new();
    let cell_at = {
        let view = view.clone();
        move |x: f64, y: f64| {
            let (cw, ch) = view.cell.get();
            ((y / ch).max(0.0) as u16, (x / cw).max(0.0) as u16)
        }
    };
    {
        let (view, a, f, area2) = (view.clone(), area.clone(), cell_at.clone(), area.clone());
        drag.connect_drag_begin(move |_, x, y| {
            area2.grab_focus();
            view.term.borrow_mut().select_from(f(x, y));
            a.queue_draw();
        });
    }
    {
        let (view, a, f) = (view.clone(), area.clone(), cell_at);
        drag.connect_drag_update(move |g, dx, dy| {
            if let Some((sx, sy)) = g.start_point() {
                view.term.borrow_mut().select_to(f(sx + dx, sy + dy));
                a.queue_draw();
            }
        });
    }
    area.add_controller(drag);

    let status = adw::WindowTitle::new(&format!("{} Console", m.name), "");
    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&status));
    let tv = adw::ToolbarView::new();
    tv.add_top_bar(&header);
    tv.set_content(Some(&area));
    let win = adw::Window::builder()
        .title(format!("{} Console", m.name))
        .default_width(900)
        .default_height(560)
        .content(&tv)
        .build();
    if let Some(p) = app.windows.borrow().first() {
        win.set_transient_for(Some(&p.win));
    }
    *m.console.borrow_mut() = Some(win.clone());
    {
        let m = m.clone();
        win.connect_close_request(move |_| {
            m.console.borrow_mut().take();
            m.console_notify.borrow_mut().take();
            glib::Propagation::Proceed
        });
    }
    // Pull what the guest printed whenever the core says there is more.
    let pull = {
        let (m, view, area, status) = (m.clone(), view, area.clone(), status);
        move || {
            let chunk = m.machine.console_read(view.next.get(), 1 << 16);
            view.next.set(chunk.next);
            if !chunk.bytes.is_empty() {
                view.term.borrow_mut().feed(&chunk.bytes);
                area.queue_draw();
            }
            let c = m.machine.view().console;
            let sub = if c.connected {
                String::new()
            } else {
                format!("Not connected: {}", c.down_reason.unwrap_or_default())
            };
            if status.subtitle() != sub {
                status.set_subtitle(&sub);
            }
        }
    };
    pull();
    *m.console_notify.borrow_mut() = Some(Box::new(pull));
    win.present();
    area.grab_focus();
}

fn pangocairo_show(cr: &gtk::cairo::Context, l: &pango::Layout) {
    pangocairo::functions::show_layout(cr, l);
}
