//! A virtual pointer that stays alive, for the Wayland checks. wlrctl makes a
//! new device per command, so the seat loses its pointer and the surface its
//! focus between commands. This keeps one device and reads commands from
//! standard input, one per line:
//!
//!   abs X Y W H     put the pointer at (X, Y) of a W x H output
//!   move DX DY      move it by that much
//!   click [N]       press and release button N (default left)
//!   down N | up N   hold or release a button

use std::io::BufRead;
use std::time::{SystemTime, UNIX_EPOCH};
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::wl_pointer::ButtonState;
use wayland_client::protocol::wl_registry::WlRegistry;
use wayland_client::{delegate_noop, Connection, Dispatch, QueueHandle};
use wayland_protocols_wlr::virtual_pointer::v1::client::zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1;
use wayland_protocols_wlr::virtual_pointer::v1::client::zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1;

struct State;

impl Dispatch<WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: <WlRegistry as wayland_client::Proxy>::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}
delegate_noop!(State: ZwlrVirtualPointerManagerV1);
delegate_noop!(State: ZwlrVirtualPointerV1);

fn ms() -> u32 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u32)
}

fn main() {
    let conn = Connection::connect_to_env().expect("no Wayland display");
    let (globals, mut queue) = registry_queue_init::<State>(&conn).expect("registry");
    let qh = queue.handle();
    let manager: ZwlrVirtualPointerManagerV1 = globals
        .bind(&qh, 1..=2, ())
        .expect("the compositor has no virtual pointer");
    let pointer = manager.create_virtual_pointer(None, &qh, ());
    queue.roundtrip(&mut State).expect("roundtrip");
    let button = |n: u32, state: ButtonState| {
        pointer.button(ms(), 0x10f + n, state);
        pointer.frame();
    };
    for line in std::io::stdin().lock().lines().map_while(Result::ok) {
        let f: Vec<&str> = line.split_whitespace().collect();
        let num = |i: usize| f.get(i).and_then(|v| v.parse::<i64>().ok()).unwrap_or(0);
        match f.first().copied() {
            Some("abs") => {
                pointer.motion_absolute(
                    ms(),
                    num(1) as u32,
                    num(2) as u32,
                    num(3) as u32,
                    num(4) as u32,
                );
                pointer.frame();
            }
            Some("move") => {
                pointer.motion(ms(), num(1) as f64, num(2) as f64);
                pointer.frame();
            }
            Some("click") => {
                let n = if f.len() > 1 { num(1) as u32 } else { 1 };
                button(n, ButtonState::Pressed);
                button(n, ButtonState::Released);
            }
            Some("down") => button(num(1) as u32, ButtonState::Pressed),
            Some("up") => button(num(1) as u32, ButtonState::Released),
            _ => {}
        }
        let _ = queue.roundtrip(&mut State);
    }
}
