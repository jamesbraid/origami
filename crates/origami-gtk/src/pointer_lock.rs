//! Capture mode's pointer lock. GTK 4 has no API to confine the pointer or
//! read relative motion, so this talks to the window system directly.
//!
//! Wayland: `zwp_pointer_constraints_v1` locks the pointer to the surface and
//! `zwp_relative_pointer_v1` reports unaccelerated motion, on a queue of our
//! own beside GDK's. X11: XInput2 raw motion gives the device's own movement,
//! which GDK's event compression and our warps cannot distort, and the
//! pointer is warped back to the middle so it never meets an edge. Without
//! either, the caller falls back to differencing absolute positions.

use adw::prelude::*;
use gdk4_wayland::prelude::*;
use gtk::glib;
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::wl_registry::WlRegistry;
use wayland_client::{delegate_noop, Connection, Dispatch, EventQueue, Proxy, QueueHandle};
use wayland_protocols::wp::pointer_constraints::zv1::client::{
    zwp_locked_pointer_v1::ZwpLockedPointerV1,
    zwp_pointer_constraints_v1::{Lifetime, ZwpPointerConstraintsV1},
};
use wayland_protocols::wp::relative_pointer::zv1::client::{
    zwp_relative_pointer_manager_v1::ZwpRelativePointerManagerV1,
    zwp_relative_pointer_v1::{Event as RelativeEvent, ZwpRelativePointerV1},
};
use x11rb::connection::Connection as _;
use x11rb::protocol::xproto::ConnectionExt as _;

/// Relative motion in the window system's own units, one count each.
pub type OnMotion = Rc<dyn Fn(f64, f64)>;

pub enum PointerLock {
    /// Held for its `Drop`, which releases the lock.
    #[allow(dead_code)]
    Wayland(WaylandLock),
    /// Held for its `Drop`.
    #[allow(dead_code)]
    X11(X11Lock),
}

impl PointerLock {
    /// Lock the pointer to `widget`'s window, or None where the window system
    /// offers no way to.
    pub fn acquire(widget: &impl IsA<gtk::Widget>, on_motion: OnMotion) -> Option<PointerLock> {
        let display = widget.display();
        let surface = widget.native()?.surface()?;
        if let (Some(d), Some(s)) = (
            display.downcast_ref::<gdk4_wayland::WaylandDisplay>(),
            surface.downcast_ref::<gdk4_wayland::WaylandSurface>(),
        ) {
            return WaylandLock::acquire(d, s, on_motion).map(PointerLock::Wayland);
        }
        if let Some(s) = surface.downcast_ref::<gdk4_x11::X11Surface>() {
            return X11Lock::acquire(widget, s.xid() as u32, on_motion).map(PointerLock::X11);
        }
        None
    }

    /// Put the pointer in the middle, so the first motion is not a jump from
    /// wherever the click happened.
    pub fn centre(&self, widget: &impl IsA<gtk::Widget>) {
        if let PointerLock::X11(l) = self {
            l.warp_centre(widget);
        }
    }
}

// ---- Wayland ----------------------------------------------------------------

#[derive(Default)]
struct State {
    motion: Vec<(f64, f64)>,
}

impl Dispatch<WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: <WlRegistry as Proxy>::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}
delegate_noop!(State: ZwpPointerConstraintsV1);
delegate_noop!(State: ZwpRelativePointerManagerV1);
delegate_noop!(State: ignore ZwpLockedPointerV1);

impl Dispatch<ZwpRelativePointerV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &ZwpRelativePointerV1,
        event: RelativeEvent,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let RelativeEvent::RelativeMotion {
            dx_unaccel,
            dy_unaccel,
            ..
        } = event
        {
            state.motion.push((dx_unaccel, dy_unaccel));
        }
    }
}

pub struct WaylandLock {
    conn: Connection,
    locked: ZwpLockedPointerV1,
    relative: ZwpRelativePointerV1,
    timer: RefCell<Option<glib::SourceId>>,
}

impl WaylandLock {
    fn acquire(
        display: &gdk4_wayland::WaylandDisplay,
        surface: &gdk4_wayland::WaylandSurface,
        on_motion: OnMotion,
    ) -> Option<WaylandLock> {
        let why = |what: &str| -> Option<WaylandLock> {
            eprintln!("origami-gtk: no Wayland pointer lock: {what}");
            None
        };
        let Some(wl_display) = display.wl_display() else {
            return why("no wl_display");
        };
        let Some(backend) = wl_display.backend().upgrade() else {
            return why("the connection is gone");
        };
        let conn = Connection::from_backend(backend);
        let Some(wl_surface) = surface.wl_surface() else {
            return why("no wl_surface");
        };
        let Some(pointer) = display
            .default_seat()
            .and_then(|s| s.pointer())
            .and_then(|d| d.downcast::<gdk4_wayland::WaylandDevice>().ok())
            .and_then(|d| d.wl_pointer())
        else {
            return why("no wl_pointer");
        };
        let (globals, mut queue): (_, EventQueue<State>) = match registry_queue_init(&conn) {
            Ok(v) => v,
            Err(e) => return why(&format!("registry: {e}")),
        };
        let qh = queue.handle();
        let Ok(constraints) = globals.bind::<ZwpPointerConstraintsV1, _, _>(&qh, 1..=1, ()) else {
            return why("the compositor has no pointer constraints");
        };
        let Ok(manager) = globals.bind::<ZwpRelativePointerManagerV1, _, _>(&qh, 1..=1, ()) else {
            return why("the compositor has no relative pointer");
        };
        let locked =
            constraints.lock_pointer(&wl_surface, &pointer, None, Lifetime::Persistent, &qh, ());
        let relative = manager.get_relative_pointer(&pointer, &qh, ());
        // The factories are not needed once they have made their objects.
        constraints.destroy();
        manager.destroy();
        let mut state = State::default();
        if let Err(e) = queue.roundtrip(&mut state) {
            return why(&format!("roundtrip: {e}"));
        }
        // GDK reads the socket and leaves our queue's events for us to
        // dispatch; a short timer does that for as long as the lock is held.
        let queue = Rc::new(RefCell::new((queue, state)));
        let id = {
            let (conn, queue) = (conn.clone(), queue.clone());
            glib::timeout_add_local(Duration::from_millis(4), move || {
                let mut q = queue.borrow_mut();
                let (eq, st) = &mut *q;
                let _ = eq.dispatch_pending(st);
                let _ = conn.flush();
                for (dx, dy) in st.motion.drain(..) {
                    on_motion(dx, dy);
                }
                glib::ControlFlow::Continue
            })
        };
        Some(WaylandLock {
            conn,
            locked,
            relative,
            timer: RefCell::new(Some(id)),
        })
    }
}

impl Drop for WaylandLock {
    fn drop(&mut self) {
        if let Some(id) = self.timer.borrow_mut().take() {
            id.remove();
        }
        self.relative.destroy();
        self.locked.destroy();
        let _ = self.conn.flush();
    }
}

// ---- X11 --------------------------------------------------------------------

pub struct X11Lock {
    conn: Rc<x11rb::rust_connection::RustConnection>,
    window: u32,
    timer: RefCell<Option<glib::SourceId>>,
}

impl X11Lock {
    fn acquire(
        widget: &impl IsA<gtk::Widget>,
        window: u32,
        on_motion: OnMotion,
    ) -> Option<X11Lock> {
        use x11rb::protocol::xinput::{self, ConnectionExt as _, XIEventMask};
        let (conn, screen) = x11rb::connect(None).ok()?;
        let conn = Rc::new(conn);
        conn.xinput_xi_query_version(2, 0).ok()?.reply().ok()?;
        let root = conn.setup().roots[screen].root;
        let mask = xinput::EventMask {
            deviceid: 1, /* XIAllMasterDevices */
            mask: vec![XIEventMask::RAW_MOTION],
        };
        conn.xinput_xi_select_events(root, &[mask])
            .ok()?
            .check()
            .ok()?;
        let lock = X11Lock {
            conn: conn.clone(),
            window,
            timer: RefCell::new(None),
        };
        let weak = widget.upcast_ref::<gtk::Widget>().downgrade();
        let id = glib::timeout_add_local(Duration::from_millis(4), move || {
            let (mut dx, mut dy) = (0.0, 0.0);
            while let Ok(Some(ev)) = conn.poll_for_event() {
                if let x11rb::protocol::Event::XinputRawMotion(r) = ev {
                    // Values come in the order of the mask's set bits: x, then y.
                    let mut values = r.axisvalues_raw.iter();
                    let bit = |i: u32| r.valuator_mask.first().is_some_and(|m| m & (1 << i) != 0);
                    let fp = |v: &xinput::Fp3232| {
                        f64::from(v.integral) + f64::from(v.frac) / 4_294_967_296.0
                    };
                    if bit(0) {
                        dx += values.next().map_or(0.0, fp);
                    }
                    if bit(1) {
                        dy += values.next().map_or(0.0, fp);
                    }
                }
            }
            if dx != 0.0 || dy != 0.0 {
                on_motion(dx, dy);
                // Keep the pointer off the edges.
                if let Some(w) = weak.upgrade() {
                    warp_centre(&conn, window, &w);
                }
            }
            glib::ControlFlow::Continue
        });
        *lock.timer.borrow_mut() = Some(id);
        Some(lock)
    }

    fn warp_centre(&self, widget: &impl IsA<gtk::Widget>) {
        warp_centre(&self.conn, self.window, widget);
    }
}

impl Drop for X11Lock {
    fn drop(&mut self) {
        if let Some(id) = self.timer.borrow_mut().take() {
            id.remove();
        }
        // Closing the connection, with the last reference, drops the event selection.
    }
}

/// Device pixels per logical pixel on the widget's window.
fn factor(widget: &impl IsA<gtk::Widget>) -> f64 {
    widget
        .native()
        .and_then(|n| n.surface())
        .map_or(1.0, |s| s.scale())
}

fn warp_centre(
    conn: &x11rb::rust_connection::RustConnection,
    window: u32,
    widget: &impl IsA<gtk::Widget>,
) {
    let Some(root) = widget.root() else { return };
    let (cx, cy) = (widget.width() as f32 / 2.0, widget.height() as f32 / 2.0);
    if let Some(p) = widget.compute_point(
        root.upcast_ref::<gtk::Widget>(),
        &gtk::graphene::Point::new(cx, cy),
    ) {
        // The X window's origin is the surface's, which can sit outside the
        // window widget (client-side shadow).
        let (sx, sy) = root.surface_transform();
        let f = factor(widget);
        let (x, y) = (
            ((f64::from(p.x()) + sx) * f).round(),
            ((f64::from(p.y()) + sy) * f).round(),
        );
        let _ = conn.warp_pointer(x11rb::NONE, window, 0, 0, 0, 0, x as i16, y as i16);
        let _ = conn.flush();
    }
}
