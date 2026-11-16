//! Long calls: they return an [`Operation`] at once and report through an
//! `Operation` event on their machine (or on the core) and through
//! [`Operation::state`].

use super::error::{CoreError, ErrorKind};
use super::event::{EventKind, Slot};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OperationState {
    Running,
    Succeeded,
    Failed(CoreError),
    Cancelled,
}

impl OperationState {
    pub fn is_finished(&self) -> bool {
        *self != OperationState::Running
    }
}

struct Inner {
    id: u64,
    title: String,
    machine: Option<String>,
    cancellable: bool,
    cancelled: AtomicBool,
    state: Mutex<(OperationState, Option<String>)>,
    progress: Mutex<String>,
    changed: Condvar,
    slot: Arc<Slot>,
}

/// A running long call. Clones share it.
#[derive(Clone)]
pub struct Operation {
    inner: Arc<Inner>,
}

/// What the work of an operation sees of it.
pub(crate) struct Control(Arc<Inner>);

impl Control {
    pub(crate) fn is_cancelled(&self) -> bool {
        self.0.cancelled.load(Ordering::Acquire)
    }

    /// Say what the operation is doing now; observers hear an `Operation`
    /// event and read it with [`Operation::progress`].
    pub(crate) fn progress(&self, text: impl Into<String>) {
        *self.0.progress.lock().unwrap() = text.into();
        self.0.slot.raise(&[EventKind::Operation]);
    }

    /// The cancel flag as a function, for a helper that outlives the borrow.
    pub(crate) fn canceller(&self) -> Arc<dyn Fn() -> bool + Send + Sync> {
        let inner = self.0.clone();
        Arc::new(move || inner.cancelled.load(Ordering::Acquire))
    }
}

impl Operation {
    pub fn id(&self) -> u64 {
        self.inner.id
    }

    pub fn title(&self) -> &str {
        &self.inner.title
    }

    /// The machine the operation works on, when it has one.
    pub fn machine(&self) -> Option<&str> {
        self.inner.machine.as_deref()
    }

    pub fn state(&self) -> OperationState {
        self.inner.state.lock().unwrap().0.clone()
    }

    /// The latest line of progress the work reported; empty before any.
    pub fn progress(&self) -> String {
        self.inner.progress.lock().unwrap().clone()
    }

    /// The machine a finished create or start operation produced.
    pub fn result_machine(&self) -> Option<String> {
        self.inner.state.lock().unwrap().1.clone()
    }

    /// Ask the work to stop. True when the operation can be cancelled and was
    /// still running. A stop cannot be cancelled.
    pub fn cancel(&self) -> bool {
        if !self.inner.cancellable || self.state().is_finished() {
            return false;
        }
        self.inner.cancelled.store(true, Ordering::Release);
        true
    }

    /// Block until the operation finishes or `timeout` passes; the state then.
    /// Not for a UI thread.
    pub fn wait(&self, timeout: Option<Duration>) -> OperationState {
        let deadline = timeout.map(|timeout| Instant::now() + timeout);
        let mut guard = self.inner.state.lock().unwrap();
        while !guard.0.is_finished() {
            let Some(left) = deadline.map_or(Some(Duration::from_secs(3600)), |deadline| {
                deadline.checked_duration_since(Instant::now())
            }) else {
                break;
            };
            guard = self.inner.changed.wait_timeout(guard, left).unwrap().0;
        }
        guard.0.clone()
    }
}

/// Run `work` on its own thread as operation `id`.
pub(crate) fn spawn<F>(
    id: u64,
    title: &str,
    machine: Option<String>,
    cancellable: bool,
    slot: Arc<Slot>,
    work: F,
) -> Operation
where
    F: FnOnce(&Control) -> Result<Option<String>, CoreError> + Send + 'static,
{
    let inner = Arc::new(Inner {
        id,
        title: title.into(),
        machine,
        cancellable,
        cancelled: AtomicBool::new(false),
        state: Mutex::new((OperationState::Running, None)),
        progress: Mutex::new(String::new()),
        changed: Condvar::new(),
        slot,
    });
    let task = inner.clone();
    std::thread::Builder::new()
        .name(format!("origami-op-{id}"))
        .spawn(move || {
            let control = Control(task.clone());
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| work(&control)))
                .unwrap_or_else(|_| {
                    Err(CoreError::new(
                        ErrorKind::Internal,
                        "the operation panicked",
                    ))
                });
            let (state, output) = match result {
                Ok(output) => (OperationState::Succeeded, output),
                Err(error) if error.kind == ErrorKind::Cancelled => {
                    (OperationState::Cancelled, None)
                }
                Err(error) => (OperationState::Failed(error), None),
            };
            *task.state.lock().unwrap() = (state, output);
            task.changed.notify_all();
            task.slot.raise(&[EventKind::Operation]);
        })
        .expect("an operation thread starts");
    Operation { inner }
}
