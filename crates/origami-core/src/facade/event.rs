//! Events and their dispatcher.
//!
//! Level-triggered and coalesced: each machine has an atomic set of pending
//! kinds. Raising a kind sets a bit and wakes the dispatcher thread, which
//! swaps the bits out and calls the observer once per machine, with no lock
//! held, so the observer may call back into the core. The event says what
//! changed, never the new value: the observer pulls the state it wants.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::thread::JoinHandle;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EventKind {
    /// The machine's state, heads or exit changed.
    State,
    /// A head has a new picture or the cursor moved.
    Display,
    /// The console has new bytes or its link changed.
    Console,
    /// An operation made progress or finished.
    Operation,
    /// The machine library changed.
    Library,
}

impl EventKind {
    pub const ALL: [EventKind; 5] = [
        EventKind::State,
        EventKind::Display,
        EventKind::Console,
        EventKind::Operation,
        EventKind::Library,
    ];

    pub(crate) fn bit(self) -> u32 {
        1 << self as u32
    }
}

/// Pending kinds of one source: a machine, or the core itself (`machine`
/// is None) for operations and library changes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Event {
    pub machine: Option<String>,
    pub kinds: Vec<EventKind>,
}

/// Implemented by the front end. Called on a thread the core owns, never on
/// the caller's thread and never on a UI thread: hop to the UI thread inside
/// it. A slow observer delays later events, never the engine.
pub trait Observer: Send + Sync {
    fn on_event(&self, event: Event);
}

pub(crate) struct Slot {
    key: Option<String>,
    bits: AtomicU32,
    shared: Arc<Shared>,
}

struct Shared {
    state: Mutex<DispatchState>,
    wake: Condvar,
}

struct DispatchState {
    pending: bool,
    shutdown: bool,
    slots: Vec<Weak<Slot>>,
}

impl Slot {
    /// Mark `kind` pending and wake the dispatcher. Allocates nothing, so an
    /// engine thread may call it per frame.
    pub(crate) fn raise(&self, kinds: &[EventKind]) {
        let bits = kinds.iter().fold(0, |bits, kind| bits | kind.bit());
        // Bits already pending were announced by whoever set them.
        if self.bits.fetch_or(bits, Ordering::AcqRel) & bits == bits {
            return;
        }
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.pending = true;
        drop(state);
        self.shared.wake.notify_one();
    }
}

pub(crate) struct Dispatcher {
    shared: Arc<Shared>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl Dispatcher {
    pub(crate) fn new(observer: Option<Arc<dyn Observer>>) -> Self {
        let shared = Arc::new(Shared {
            state: Mutex::new(DispatchState {
                pending: false,
                shutdown: false,
                slots: Vec::new(),
            }),
            wake: Condvar::new(),
        });
        let thread = observer.map(|observer| {
            let shared = shared.clone();
            std::thread::Builder::new()
                .name("origami-events".into())
                .spawn(move || run(&shared, observer.as_ref()))
                .expect("the event thread starts")
        });
        Self {
            shared,
            thread: Mutex::new(thread),
        }
    }

    pub(crate) fn slot(&self, key: Option<String>) -> Arc<Slot> {
        let slot = Arc::new(Slot {
            key,
            bits: AtomicU32::new(0),
            shared: self.shared.clone(),
        });
        let mut state = self.shared.state.lock().unwrap();
        state.slots.retain(|slot| slot.strong_count() > 0);
        state.slots.push(Arc::downgrade(&slot));
        slot
    }

    /// Stop the dispatcher thread. Called from the observer itself, it only
    /// asks the thread to end.
    pub(crate) fn shutdown(&self) {
        self.shared.state.lock().unwrap().shutdown = true;
        self.shared.wake.notify_all();
        let handle = self.thread.lock().unwrap().take();
        if let Some(handle) = handle {
            if handle.thread().id() != std::thread::current().id() {
                let _ = handle.join();
            }
        }
    }
}

impl Drop for Dispatcher {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn run(shared: &Shared, observer: &dyn Observer) {
    loop {
        let slots: Vec<Arc<Slot>> = {
            let mut state = shared.state.lock().unwrap();
            while !state.pending && !state.shutdown {
                state = shared.wake.wait(state).unwrap();
            }
            if state.shutdown {
                return;
            }
            state.pending = false;
            state.slots.retain(|slot| slot.strong_count() > 0);
            state.slots.iter().filter_map(Weak::upgrade).collect()
        };
        for slot in slots {
            let bits = slot.bits.swap(0, Ordering::AcqRel);
            if bits == 0 {
                continue;
            }
            let event = Event {
                machine: slot.key.clone(),
                kinds: EventKind::ALL
                    .into_iter()
                    .filter(|kind| bits & kind.bit() != 0)
                    .collect(),
            };
            // A panicking observer must not end event delivery.
            let _ = catch_unwind(AssertUnwindSafe(|| observer.on_event(event)));
        }
    }
}
