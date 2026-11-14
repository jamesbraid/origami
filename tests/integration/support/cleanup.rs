//! Cleanup that still runs when a test fails or the job is cancelled.
//!
//! A guest or lab registers how to undo itself. Its `Drop` runs that on a
//! normal exit or an unwinding panic. A SIGINT or SIGTERM, which is what a
//! CI timeout or a Ctrl-C sends, takes the same path from a signal thread:
//! every registered cleanup runs, newest first, then the process exits.
//! Registrations from a test that is already gone are never left behind.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Once};

type Undo = Arc<dyn Fn() + Send + Sync>;

static NEXT: AtomicU64 = AtomicU64::new(1);
static PENDING: Mutex<Vec<(u64, String, Undo)>> = Mutex::new(Vec::new());
static HANDLER: Once = Once::new();

fn pending() -> std::sync::MutexGuard<'static, Vec<(u64, String, Undo)>> {
    PENDING.lock().unwrap_or_else(|e| e.into_inner())
}

/// Held while the thing it registered is alive. Dropping it only forgets the
/// registration; the owner runs its own cleanup first.
pub struct Registration(u64);

pub fn register(name: &str, undo: impl Fn() + Send + Sync + 'static) -> Registration {
    install_signal_handler();
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    pending().push((id, name.to_string(), Arc::new(undo)));
    Registration(id)
}

impl Drop for Registration {
    fn drop(&mut self) {
        pending().retain(|(id, _, _)| *id != self.0);
    }
}

/// Run every registered cleanup, newest first.
pub fn run_all() {
    let all: Vec<_> = std::mem::take(&mut *pending());
    for (_, name, undo) in all.into_iter().rev() {
        eprintln!("cleanup on signal: {name}");
        undo();
    }
}

#[cfg(unix)]
fn install_signal_handler() {
    use signal_hook::consts::{SIGINT, SIGTERM};
    use signal_hook::iterator::Signals;
    HANDLER.call_once(|| {
        let mut signals = Signals::new([SIGINT, SIGTERM]).expect("install the signal handler");
        std::thread::spawn(move || {
            if let Some(signal) = signals.forever().next() {
                run_all();
                std::process::exit(128 + signal);
            }
        });
    });
}

// A hosted Windows runner is discarded after the job, so there is nothing to
// leave behind that matters.
#[cfg(not(unix))]
fn install_signal_handler() {
    HANDLER.call_once(|| {});
}
