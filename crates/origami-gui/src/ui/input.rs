//! The display view's keyboard and pointer, and capturing the pointer. Part
//! of [`super`], whose state it works on.
//!
//! By default the pointer is captured: a click on the picture hides and locks
//! the host cursor, and the raw motion the device reports goes to the guest
//! as relative motion until the release chord. In absolute mode the pointer's
//! place in the picture is sent instead, at a pace QEMU's loop can follow
//! (see [`crate::pointer`]).

use std::time::Instant;

use slint::winit_030::winit::event::{DeviceEvent, DeviceId, WindowEvent};
use slint::winit_030::winit::event_loop::ActiveEventLoop;
use slint::winit_030::winit::window::{CursorGrabMode, Window, WindowId};
use slint::winit_030::{CustomApplicationHandler, EventResult, WinitWindowAccessor};
use slint::ComponentHandle;

use super::Ui;
use crate::core::{CursorInfo, InputEvent, PointerButton};
use crate::keymap;
use crate::pointer::{self, PointerMode, Target};
use std::sync::atomic::Ordering;

/// Whether the guest shows a cursor of its own in the picture: QEMU composes
/// one (`on`), or the model draws it in its pixels and the head reports where
/// (it is off the origin once the guest has placed one).
pub fn guest_has_cursor(c: &CursorInfo) -> bool {
    c.on || c.x != 0 || c.y != 0
}

/// Where a confined (not locked) host cursor is put back to the middle of
/// the window: when it comes this close to an edge of the view, in physical
/// pixels. It is the window system's cursor, with its own acceleration, so
/// where it is is read from the position Slint reports, not summed from the
/// device's motion.
const CONFINED_EDGE: f32 = 48.0;

impl Ui {
    /// A key pressed or released in the display view.
    pub fn key_event(&mut self, win: usize, text: &str, down: bool) {
        let (Some(mi), Some(key)) = (self.machine_in(win), keymap::from_text(text)) else {
            return;
        };
        let x = &mut self.machines[mi].x;
        if down {
            x.held.insert(key);
            if x.captured && pointer::is_release_chord(&x.held) {
                // The guest saw the first of the chord go down and must see it
                // come up; it never saw this one.
                self.machines[mi].x.held.remove(&key);
                self.release_all(mi);
                return;
            }
        } else if !x.held.remove(&key) {
            // Released after a release, a focus change or a tab switch: the
            // guest has been told already.
            return;
        }
        self.send(mi, InputEvent::Key { key, down });
        self.counters.keys_sent.fetch_add(1, Ordering::Relaxed);
    }

    /// The host pointer moved over the picture, to (`x`, `y`) physical pixels
    /// of the view. Only absolute mode follows it; a captured pointer is
    /// followed by [`Ui::raw_motion`].
    pub fn pointer_moved(&mut self, win: usize, x: f32, y: f32) {
        let Some(mi) = self.machine_in(win) else {
            return;
        };
        if self.machines[mi].x.captured && self.machines[mi].x.grab == "confined" {
            let (w, h) = (
                self.machines[mi].area.0 as f32,
                self.machines[mi].area.1 as f32,
            );
            if x < CONFINED_EDGE
                || y < CONFINED_EDGE
                || x > w - CONFINED_EDGE
                || y > h - CONFINED_EDGE
            {
                self.centre_cursor(mi);
            }
        }
        if self.machines[mi].x.mode != PointerMode::Absolute {
            return;
        }
        let Some((hx, hy)) = self.guest_pos(mi, x, y) else {
            return;
        };
        let head = self.machines[mi].head_sel as u32;
        self.machines[mi].x.abs.offer(Target { head, x: hx, y: hy });
        self.abs_flush(mi);
    }

    /// Send the absolute target that is due, and arrange to send the one
    /// that is not yet.
    pub fn abs_flush(&mut self, mi: usize) {
        let now = Instant::now();
        let x = &mut self.machines[mi].x;
        let due = x.abs.take_due(now);
        match x.abs.wait(now) {
            Some(d) => {
                if !x.abs_timer.running() {
                    x.abs_timer.start(
                        slint::TimerMode::SingleShot,
                        d.max(std::time::Duration::from_millis(1)),
                        move || with_ui_later(mi),
                    );
                }
            }
            None => x.abs_timer.stop(),
        }
        if let Some(t) = due {
            self.send_target(mi, t);
        }
    }

    fn send_target(&mut self, mi: usize, t: Target) {
        self.counters.moves_sent.fetch_add(1, Ordering::Relaxed);
        self.send(
            mi,
            InputEvent::PointerAt {
                head: t.head,
                x: t.x,
                y: t.y,
            },
        );
    }

    /// A mouse button pressed or released over the picture.
    pub fn button_event(&mut self, win: usize, b: i32, down: bool) {
        let Some(mi) = self.machine_in(win) else {
            return;
        };
        let (button, slot) = match b {
            0 => (PointerButton::Left, 0),
            1 => (PointerButton::Middle, 1),
            _ => (PointerButton::Right, 2),
        };
        let x = &mut self.machines[mi].x;
        if !down && x.swallow[slot] {
            // The release of the click that captured the pointer.
            x.swallow[slot] = false;
            return;
        }
        if !down && x.held_buttons[slot] {
            // The end of a press the guest has seen, whatever has happened
            // to the capture since: the guest must not keep it down.
            x.held_buttons[slot] = false;
            self.send(mi, InputEvent::Button { button, down });
            return;
        }
        if x.mode == PointerMode::Capture && !x.captured {
            // A click on the picture is the request to capture, and goes no
            // further: the guest never sees where the host cursor was.
            if down && self.set_capture(mi, true) {
                self.machines[mi].x.swallow[slot] = true;
            }
            return;
        }
        if down && x.mode == PointerMode::Absolute {
            // The click lands where the pointer is, not where it was a
            // period ago. (QEMU walks the cursor there after the click, so a
            // click right after a long move can still land short; see
            // `pointer::ABS_PERIOD`.)
            if let Some(t) = x.abs.take_now(Instant::now()) {
                self.send_target(mi, t);
            }
        }
        self.machines[mi].x.held_buttons[slot] = down;
        self.send(mi, InputEvent::Button { button, down });
    }

    /// Raw motion from the pointing device, in the device's counts: the
    /// captured machine's guest is sent it as it is.
    pub fn raw_motion(&mut self, dx: f64, dy: f64) {
        let Some(mi) = self.machines.iter().position(|m| m.x.captured) else {
            return;
        };
        if let Some((cx, cy)) = self.machines[mi].x.rel.push(dx, dy) {
            self.counters.moves_sent.fetch_add(1, Ordering::Relaxed);
            self.send(mi, InputEvent::PointerMove { dx: cx, dy: cy });
        }
    }

    fn centre_cursor(&self, mi: usize) {
        if let Some(sh) = self.shell_for(mi) {
            sh.window().with_winit_window(|w| {
                let s = w.inner_size();
                let _ = w.set_cursor_position(slint::winit_030::winit::dpi::PhysicalPosition::new(
                    s.width / 2,
                    s.height / 2,
                ));
            });
        }
    }

    /// Whether a click on `mi`'s picture can capture the pointer now.
    fn can_capture(&self, mi: usize) -> bool {
        let m = &self.machines[mi];
        m.x.view == super::View::Display
            && self.win_for(mi).is_some()
            && matches!(m.heads.get(m.head_sel), Some(Some(_)))
    }

    /// Capture or free the host pointer for machine `mi`. True when it is in
    /// the state asked for.
    pub fn set_capture(&mut self, mi: usize, on: bool) -> bool {
        if self.machines[mi].x.captured == on {
            return true;
        }
        if on && !self.can_capture(mi) {
            return false;
        }
        if on {
            // One pointer: another machine's capture ends.
            for other in 0..self.machines.len() {
                if other != mi && self.machines[other].x.captured {
                    self.release_all(other);
                }
            }
        }
        let mut grab = "";
        if let Some(sh) = self.shell_for(mi) {
            sh.window().with_winit_window(|w| {
                if on {
                    // Locked keeps it still and is not offered everywhere
                    // (X11, Windows); confined holds it in the window.
                    if w.set_cursor_grab(CursorGrabMode::Locked).is_ok() {
                        grab = "locked";
                    } else if w.set_cursor_grab(CursorGrabMode::Confined).is_ok() {
                        grab = "confined";
                    }
                } else {
                    let _ = w.set_cursor_grab(CursorGrabMode::None);
                }
            });
        }
        let x = &mut self.machines[mi].x;
        if on && grab.is_empty() {
            x.notice = "the window system would not let the pointer be captured".into();
            self.push(mi);
            return false;
        }
        x.captured = on;
        x.grab = grab;
        x.rel.reset();
        x.notice.clear();
        if on {
            self.centre_cursor_if_confined(mi);
        }
        self.push(mi);
        true
    }

    fn centre_cursor_if_confined(&self, mi: usize) {
        if self.machines[mi].x.grab == "confined" {
            self.centre_cursor(mi);
        }
    }

    /// Free the pointer and tell the guest every key it holds is up: the
    /// view is going away, or the keyboard has left it, and the releases
    /// would not arrive.
    pub fn release_all(&mut self, mi: usize) {
        self.set_capture(mi, false);
        let held: Vec<_> = self.machines[mi].x.held.drain().collect();
        for key in held {
            self.send(mi, InputEvent::Key { key, down: false });
        }
        for (slot, button) in [
            PointerButton::Left,
            PointerButton::Middle,
            PointerButton::Right,
        ]
        .into_iter()
        .enumerate()
        {
            if std::mem::take(&mut self.machines[mi].x.held_buttons[slot]) {
                self.send(
                    mi,
                    InputEvent::Button {
                        button,
                        down: false,
                    },
                );
            }
        }
        self.machines[mi].x.swallow = [false; 3];
    }

    /// The window system took the keyboard away from every window.
    pub fn release_every_machine(&mut self) {
        for mi in 0..self.machines.len() {
            if self.machines[mi].x.captured || !self.machines[mi].x.held.is_empty() {
                self.release_all(mi);
            }
        }
    }

    /// Switch between capturing the pointer and sending absolute positions.
    pub fn set_pointer_mode(&mut self, mi: usize, mode: PointerMode) {
        if self.machines[mi].x.mode == mode {
            return;
        }
        self.release_all(mi);
        let x = &mut self.machines[mi].x;
        x.mode = mode;
        x.abs.reset();
        x.abs_timer.stop();
        self.push(mi);
    }

    /// What the status line says about the pointer.
    pub fn pointer_hint(&self, mi: usize) -> String {
        let x = &self.machines[mi].x;
        match (x.mode, x.captured) {
            (PointerMode::Capture, true) => format!(
                "pointer captured, press {} to release",
                pointer::release_chord_name()
            ),
            (PointerMode::Capture, false) => "click the picture to capture the pointer".into(),
            (PointerMode::Absolute, _) => "absolute pointer".into(),
        }
    }
}

/// Run `abs_flush` for machine `mi` from the event loop. The timer is
/// stopped first so that `abs_flush` can start it again whether or not Slint
/// still counts a single-shot timer as running inside its own callback.
fn with_ui_later(mi: usize) {
    super::with_ui(move |u| {
        u.machines[mi].x.abs_timer.stop();
        u.abs_flush(mi);
    });
}

/// Hooks into winit's event loop for what Slint does not pass on: the
/// device's own motion, which a locked pointer still reports, and the loss
/// of the keyboard.
pub struct RawPointer;

impl CustomApplicationHandler for RawPointer {
    fn device_event(
        &mut self,
        _: &ActiveEventLoop,
        _: DeviceId,
        event: DeviceEvent,
    ) -> EventResult {
        if let DeviceEvent::MouseMotion { delta: (dx, dy) } = event {
            super::with_ui(move |u| u.raw_motion(dx, dy));
        }
        EventResult::Propagate
    }

    fn window_event(
        &mut self,
        _: &ActiveEventLoop,
        _: WindowId,
        _: Option<&Window>,
        _: Option<&slint::Window>,
        event: &WindowEvent,
    ) -> EventResult {
        if matches!(event, WindowEvent::Focused(false)) {
            super::with_ui(|u| u.release_every_machine());
        }
        EventResult::Propagate
    }
}
