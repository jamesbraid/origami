//! An iris board's picture as display protocol 4: the push host between its
//! display thread and the producer.

use origami_display_producer::{Handle, Host, InputEvent, Marks};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

/// Most lines a head can have.
const MAX_LINES: u32 = 16384;

/// What the producer asks of a board.
pub trait Board: Send + Sync {
    /// Compose a frame soon, even if nothing changed.
    fn request_frame(&self);
}

/// The machine's keyboard and mouse.
pub trait Hands: Send {
    fn key(&mut self, evdev: u32, down: bool);
    /// Hold exactly these PS/2 buttons (1 left, 2 right, 4 middle).
    fn buttons(&mut self, mask: u8);
    fn nudge(&mut self, dx: i32, dy: i32);
    fn wheel(&mut self, dz: i32);
}

struct Link {
    head: u32,
    marks: Arc<Marks>,
    handle: Arc<OnceLock<Handle>>,
    asked: AtomicBool,
}

/// One head's end of the display thread.
pub struct Presenter {
    link: Arc<Link>,
    known: (u32, u32),
}

/// A composition the producer is waiting for.
pub struct Ticket(u64);

impl Presenter {
    /// The board rendered a frame of `size`; `changed` is false only if it proves
    /// nothing moved. Returns a ticket when a viewer wants a picture: compose, `finish`.
    pub fn begin(&mut self, size: (u32, u32), changed: bool) -> Option<Ticket> {
        let handle = self.link.handle.get()?;
        let head = self.link.head;
        if size.0 == 0 || size.1 == 0 || size.1 > MAX_LINES {
            return None;
        }
        if size != self.known {
            self.known = size;
            handle.with(|e| {
                if e.head_size(head) != size {
                    e.set_mode(head, size.0, size.1);
                }
            });
        }
        if changed {
            self.link.marks.mark_all();
        }
        if !self.link.asked.swap(false, Ordering::AcqRel) {
            return None;
        }
        handle
            .with(|e| e.push_begin(head))
            .map(|(id, _)| Ticket(id))
    }

    /// The composition is done: `rows` is the picture and the distance
    /// between its rows in pixels, or `None` when there was none to make.
    pub fn finish(&mut self, ticket: Ticket, rows: Option<(&[u32], usize)>) {
        let Some(handle) = self.link.handle.get() else {
            return;
        };
        handle.with(|e| match rows {
            Some((pixels, stride)) => {
                // SAFETY: a u32 slice viewed as bytes; the host is little endian
                // (the producer refuses big-endian hosts).
                let bytes = unsafe {
                    std::slice::from_raw_parts(pixels.as_ptr().cast::<u8>(), pixels.len() * 4)
                };
                e.push_complete_strided(self.link.head, ticket.0, Some(bytes), stride * 4)
            }
            None => e.push_complete(self.link.head, ticket.0, None),
        });
    }
}

/// Makes the board for a head the producer attaches, given its presenter.
pub type Attach = Box<dyn FnMut(u32, Presenter) -> Option<Arc<dyn Board>> + Send>;

/// The producer's host for iris: pictures are pushed by the boards, input
/// goes to the machine's keyboard and mouse.
pub struct IrisHost {
    handle: Arc<OnceLock<Handle>>,
    attach: Attach,
    boards: HashMap<u32, (Arc<Link>, Arc<dyn Board>)>,
    hands: Box<dyn Hands>,
    buttons: u8,
}

impl IrisHost {
    /// `handle` is set once the producer runs; `attach` is asked for the
    /// board of each head the producer adds.
    pub fn new(handle: Arc<OnceLock<Handle>>, hands: Box<dyn Hands>, attach: Attach) -> Self {
        Self {
            handle,
            attach,
            boards: HashMap::new(),
            hands,
            buttons: 0,
        }
    }
}

impl Host for IrisHost {
    fn attach(&mut self, head: u32, marks: Arc<Marks>) {
        let link = Arc::new(Link {
            head,
            marks,
            handle: self.handle.clone(),
            asked: AtomicBool::new(false),
        });
        let presenter = Presenter {
            link: link.clone(),
            known: (0, 0),
        };
        if let Some(board) = (self.attach)(head, presenter) {
            self.boards.insert(head, (link, board));
        }
    }

    fn request_frame(&mut self, head: u32) {
        if let Some((link, board)) = self.boards.get(&head) {
            link.asked.store(true, Ordering::Release);
            board.request_frame();
        }
    }

    fn inject(&mut self, ev: &InputEvent) {
        match *ev {
            InputEvent::Key { code, down, .. } => self.hands.key(code, down != 0),
            InputEvent::Button { button, down, .. } => {
                let bit = match button {
                    0 => 1,
                    1 => 4,
                    2 => 2,
                    3 | 4 => {
                        if down != 0 {
                            self.hands.wheel(if button == 3 { 1 } else { -1 });
                        }
                        return;
                    }
                    _ => return,
                };
                self.buttons = if down != 0 {
                    self.buttons | bit
                } else {
                    self.buttons & !bit
                };
                self.hands.buttons(self.buttons);
            }
            InputEvent::Rel { dx, dy, .. } => self.hands.nudge(dx, dy),
            // The heads are relative: the producer drops absolute motion.
            InputEvent::Abs { .. } | InputEvent::ReportEnd => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Log(Arc<Mutex<Vec<String>>>);

    impl Hands for Log {
        fn key(&mut self, evdev: u32, down: bool) {
            self.0.lock().unwrap().push(format!("key {evdev} {down}"));
        }
        fn buttons(&mut self, mask: u8) {
            self.0.lock().unwrap().push(format!("buttons {mask}"));
        }
        fn nudge(&mut self, dx: i32, dy: i32) {
            self.0.lock().unwrap().push(format!("nudge {dx} {dy}"));
        }
        fn wheel(&mut self, dz: i32) {
            self.0.lock().unwrap().push(format!("wheel {dz}"));
        }
    }

    #[test]
    fn buttons_become_ps2_bits_and_wheel_notches() {
        let log = Log::default();
        let seen = log.0.clone();
        let mut host = IrisHost::new(Default::default(), Box::new(log), Box::new(|_, _| None));
        let button = |button, down| InputEvent::Button {
            head: 0,
            button,
            down,
        };
        for ev in [
            button(0, 1),
            button(2, 1),
            button(0, 0),
            button(1, 1),
            button(3, 1),
            button(3, 0),
            button(4, 1),
            button(5, 1),
        ] {
            host.inject(&ev);
        }
        assert_eq!(
            *seen.lock().unwrap(),
            [
                "buttons 1",
                "buttons 3",
                "buttons 2",
                "buttons 6",
                "wheel 1",
                "wheel -1"
            ]
        );
    }
}
