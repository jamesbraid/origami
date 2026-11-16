//! Keyboard and pointer, from GTK's controllers to the engine.
//!
//! Two pointer modes. Follow: the guest cursor goes to the host pointer's
//! position in the picture. Capture: a click takes the pointer, the cursor
//! hides, motion goes to the guest as relative counts, and Ctrl+Alt gives it
//! back. Either way a key that is down when focus leaves, or capture ends, is
//! released in the guest so nothing stays stuck.

use crate::app::App;
use crate::keys;
use crate::machine::MachineUi;
use crate::pointer_lock::PointerLock;
use crate::prefs::PointerMode;
use adw::prelude::*;
use gtk::{gdk, glib};
use origami_engine::{InputEvent, Key, PointerButton};
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

impl MachineUi {
    pub fn send(&self, ev: InputEvent) {
        self.inputs.set(self.inputs.get() + 1);
        let _ = self.machine.send_input(&[ev]);
    }

    /// The machine has stopped: nothing is held and nothing is captured.
    pub fn release_input(&self) {
        self.captured.set(false);
        self.last_pos.set(None);
    }
}

/// Release every held key in the guest, and end capture.
pub fn release_all(app: &Rc<App>, m: &Rc<MachineUi>) {
    // The core knows which keys and buttons it has sent down.
    let _ = m.machine.release_all_keys();
    m.ctrl_alt.set((false, false));
    set_captured(app, m, false);
}

pub fn set_captured(app: &Rc<App>, m: &Rc<MachineUi>, on: bool) {
    if m.captured.replace(on) == on {
        return;
    }
    m.last_pos.set(None);
    m.frac.set((0.0, 0.0));
    m.screen
        .set_cursor_from_name(if on { Some("none") } else { None });
    if on {
        let weak = Rc::downgrade(m);
        let lock = PointerLock::acquire(
            &m.screen,
            Rc::new(move |dx, dy| {
                if let Some(m) = weak.upgrade() {
                    send_motion(&m, dx, dy);
                }
            }),
        );
        if let Some(l) = &lock {
            l.centre(&m.screen);
        }
        *m.lock.borrow_mut() = lock;
    } else {
        m.lock.borrow_mut().take();
        // Whatever ended capture, nothing the guest holds stays held.
        let _ = m.machine.release_all_keys();
    }
    if on {
        flash_hud(m, "Input captured. Press Ctrl+Alt to release.");
    } else {
        flash_hud(m, "Input released");
    }
    // App shortcuts would swallow keys the guest should get.
    crate::window::sync_accels(app);
    crate::window::refresh(app);
}

fn flash_hud(m: &Rc<MachineUi>, text: &str) {
    m.hud.set_label(text);
    m.hud.set_visible(true);
    if let Some(old) = m.hud_timer.borrow_mut().take() {
        old.remove();
    }
    let weak = Rc::downgrade(m);
    let id = glib::timeout_add_local_once(Duration::from_secs(3), move || {
        if let Some(m) = weak.upgrade() {
            m.hud.set_visible(false);
            m.hud_timer.borrow_mut().take();
        }
    });
    *m.hud_timer.borrow_mut() = Some(id);
}

pub fn wire(app: &Rc<App>, m: &Rc<MachineUi>) {
    let by_evdev: Rc<HashMap<u32, Key>> = Rc::new(
        keys::ALL
            .iter()
            .map(|&k| (origami_display::input::evdev(k), k))
            .collect(),
    );
    let key = gtk::EventControllerKey::new();
    {
        let (app, m, map) = (app.clone(), m.clone(), by_evdev.clone());
        key.connect_key_pressed(move |_, keyval, code, _| {
            track_chord(&m, keyval, true);
            if m.captured.get() && m.ctrl_alt.get() == (true, true) {
                release_all(&app, &m);
                return glib::Propagation::Stop;
            }
            // GDK's hardware keycode is the evdev code plus 8 on X11 and Wayland.
            match code.checked_sub(8).and_then(|c| map.get(&c)) {
                Some(&k) => {
                    m.send(InputEvent::Key { key: k, down: true });
                    glib::Propagation::Stop
                }
                None => glib::Propagation::Proceed,
            }
        });
    }
    {
        let (m, map) = (m.clone(), by_evdev);
        key.connect_key_released(move |_, keyval, code, _| {
            track_chord(&m, keyval, false);
            if let Some(&k) = code.checked_sub(8).and_then(|c| map.get(&c)) {
                m.send(InputEvent::Key {
                    key: k,
                    down: false,
                });
            }
        });
    }
    m.screen.add_controller(key);

    let focus = gtk::EventControllerFocus::new();
    {
        let (app, m) = (app.clone(), m.clone());
        focus.connect_leave(move |_| release_all(&app, &m));
    }
    m.screen.add_controller(focus);

    let motion = gtk::EventControllerMotion::new();
    {
        let m = m.clone();
        motion.connect_motion(move |_, x, y| {
            if m.pointer.get() == PointerMode::Follow {
                if let Some((px, py)) = to_head(&m, x, y) {
                    m.send(InputEvent::PointerAt {
                        head: 0,
                        x: px,
                        y: py,
                    });
                }
            } else if m.captured.get() {
                // With a lock, motion comes from the lock itself. Without one,
                // difference absolute positions.
                if m.lock.borrow().is_none() {
                    if let Some((lx, ly)) = m.last_pos.get() {
                        send_motion(&m, x - lx, y - ly);
                    }
                    m.last_pos.set(Some((x, y)));
                }
            }
        });
    }
    {
        let m = m.clone();
        motion.connect_enter(move |_, x, y| m.last_pos.set(Some((x, y))));
    }
    m.screen.add_controller(motion);

    let click = gtk::GestureClick::builder().button(0).build();
    {
        let (app, m) = (app.clone(), m.clone());
        click.connect_pressed(move |g, _, x, y| {
            m.screen.grab_focus();
            if m.pointer.get() == PointerMode::Capture && !m.captured.get() {
                // The click that captures is not the guest's.
                set_captured(&app, &m, true);
                m.last_pos.set(Some((x, y)));
                return;
            }
            if let Some(button) = button_of(g.current_button()) {
                if m.pointer.get() == PointerMode::Follow {
                    if let Some((px, py)) = to_head(&m, x, y) {
                        m.send(InputEvent::PointerAt {
                            head: 0,
                            x: px,
                            y: py,
                        });
                    }
                }
                m.send(InputEvent::Button { button, down: true });
            }
        });
    }
    {
        let m = m.clone();
        click.connect_released(move |g, _, _, _| {
            if let Some(button) = button_of(g.current_button()) {
                if m.pointer.get() == PointerMode::Follow || m.captured.get() {
                    m.send(InputEvent::Button {
                        button,
                        down: false,
                    });
                }
            }
        });
    }
    m.screen.add_controller(click);

    let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
    {
        let m = m.clone();
        scroll.connect_scroll(move |_, _, dy| {
            let notches = -(dy.round() as i32);
            if notches != 0 && (m.pointer.get() == PointerMode::Follow || m.captured.get()) {
                m.send(InputEvent::Wheel { notches });
            }
            glib::Propagation::Stop
        });
    }
    m.screen.add_controller(scroll);
}

/// Send relative motion, carrying what is less than one count to the next event.
fn send_motion(m: &MachineUi, dx: f64, dy: f64) {
    let (fx, fy) = m.frac.get();
    let (tx, ty) = (dx + fx, dy + fy);
    let (ix, iy) = (tx.trunc(), ty.trunc());
    m.frac.set((tx - ix, ty - iy));
    if ix != 0.0 || iy != 0.0 {
        let (tx, ty) = m.motion_total.get();
        m.motion_total.set((tx + ix as i64, ty + iy as i64));
        m.send(InputEvent::PointerMove {
            dx: ix as i32,
            dy: iy as i32,
        });
    }
}

fn button_of(n: u32) -> Option<PointerButton> {
    match n {
        1 => Some(PointerButton::Left),
        2 => Some(PointerButton::Middle),
        3 => Some(PointerButton::Right),
        _ => None,
    }
}

fn track_chord(m: &MachineUi, keyval: gdk::Key, down: bool) {
    let (mut ctrl, mut alt) = m.ctrl_alt.get();
    match keyval {
        gdk::Key::Control_L | gdk::Key::Control_R => ctrl = down,
        gdk::Key::Alt_L | gdk::Key::Alt_R | gdk::Key::Meta_L | gdk::Key::Meta_R => alt = down,
        _ => return,
    }
    m.ctrl_alt.set((ctrl, alt));
}

/// Map a point in the screen widget to a pixel of head 0, accounting for the
/// letterbox around a picture that is scaled to fit.
pub fn to_head(m: &MachineUi, x: f64, y: f64) -> Option<(i32, i32)> {
    let (w, h) = m.size.get();
    if w == 0 {
        return None;
    }
    let (aw, ah) = (f64::from(m.screen.width()), f64::from(m.screen.height()));
    let one_to_one = m.picture.content_fit() == gtk::ContentFit::ScaleDown;
    let mut s = (aw / f64::from(w)).min(ah / f64::from(h));
    if one_to_one {
        s = s.min(1.0);
    }
    let (ox, oy) = ((aw - f64::from(w) * s) / 2.0, (ah - f64::from(h) * s) / 2.0);
    let (px, py) = ((x - ox) / s, (y - oy) / s);
    Some((
        px.clamp(0.0, f64::from(w) - 1.0) as i32,
        py.clamp(0.0, f64::from(h) - 1.0) as i32,
    ))
}
