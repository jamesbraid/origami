//! One machine of the library: its state, display, input and console, and
//! the long calls that start and stop it.
//!
//! Every method may be called from any thread. Fast calls (`view`, `heads`,
//! `take_frame`, `send_*`, `console_*`) are safe on a UI thread; `start` and
//! `stop` return an [`Operation`].

use super::console::Console;
use super::cpu::{self, CpuLoadReport, CpuMonitor};
use super::error::{CoreError, CoreResult, ErrorKind};
use super::event::{EventKind, Slot};
use super::operation::{self, Control as OpControl, Operation};
use super::settings::{self, MachineSettings};
use crate::Change;
use origami_engine::{
    ConsoleIo, Control, CursorInfo, DisplayEndpoint, DisplayRequest, Engine, Frame, FrameSource,
    FrameStats, InputEvent, Key, PointerButton, RunState, Session, Stop, Wiring,
};
use origami_machine::dir::MachineDir;
use origami_machine::model;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex, Weak};
use std::time::{Duration, Instant};

/// Called from an engine thread when a head has a new picture. It must be
/// cheap and must not block.
pub type FrameHook = Arc<dyn Fn() + Send + Sync>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HookId(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MachineState {
    NotRunning,
    Starting,
    Running,
    Paused,
    Stopping,
    /// The engine's program is gone; the view says how.
    Exited,
    /// Another process runs it, such as a runner left by an earlier session.
    /// Only the library listing says so; `start` attaches to it.
    LeftRunning,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopKind {
    /// Ask the guest to quit and wait, up to the core's `stop_patience`; then
    /// the operation fails and the machine is left running.
    Clean,
    /// End the machine at once.
    Force,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HeadView {
    pub index: u32,
    pub width: u32,
    pub height: u32,
    pub seq: u64,
    pub active: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConsoleView {
    pub connected: bool,
    /// Why the console is not connected, when it is not.
    pub down_reason: Option<String>,
    /// Bytes the guest has printed since the machine started.
    pub total: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MachineView {
    pub key: String,
    pub name: String,
    pub product: String,
    pub engine: String,
    pub state: MachineState,
    pub exit_code: Option<i32>,
    /// Why the engine's program ended, once it has.
    pub exit_reason: Option<ExitReason>,
    /// What it said or how it ended, such as `signal 9` or its last error.
    pub exit_detail: Option<String>,
    pub last_error: Option<String>,
    /// One sentence for a status line.
    pub reason: String,
    pub heads: Vec<HeadView>,
    pub console: ConsoleView,
    /// The long call in flight on this machine.
    pub operation: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConsoleChunk {
    pub bytes: Vec<u8>,
    /// Pass as `from` to continue.
    pub next: u64,
    /// Bytes between `from` and the oldest kept that were lost.
    pub dropped: u64,
    /// Changes when the machine restarts and the offsets begin again.
    pub generation: u64,
}

/// Why the engine's program ended; `MachineView::exit_detail` says more.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExitReason {
    /// The guest shut itself down: a clean stop, not an unexpected one.
    GuestPoweredOff,
    /// This core asked: Power Off, Force Quit or the power button.
    HostStopped,
    /// A signal or a non-zero exit status, nobody having asked.
    Crashed,
    /// The engine or its runner left no word of how the machine ended.
    RunnerError,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Idle,
    Starting,
    Stopping,
    /// An edit is writing the file and reloading the model; a start waits.
    Editing,
}

/// How long after the power button a guest's own shutdown still counts as
/// the answer to it.
const POWER_BUTTON_PATIENCE: Duration = Duration::from_secs(300);

/// Puts the phase back to `Idle` when an edit ends, however it ends.
struct Editing<'a>(&'a Mutex<Run>);

impl Drop for Editing<'_> {
    fn drop(&mut self) {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).phase = Phase::Idle;
    }
}

struct Run {
    phase: Phase,
    session: Option<Arc<dyn Session>>,
    frames: Option<Arc<dyn FrameSource>>,
    /// The session has a display that is not in this process.
    remote_display: bool,
    operation: Option<u64>,
    /// This core's stop in flight or done since the last start; a failed stop
    /// takes it back.
    stop_requested: Option<StopKind>,
    /// When the power button was last pressed; a guest may ignore it, so it
    /// counts for a while only.
    power_button: Option<Instant>,
    /// How the runner says the machine ended, read once when it exits.
    exit_note: Option<origami_machine::control::Exit>,
    /// Samples the session's CPU threads; made by the first `cpu_load`.
    cpu: Option<Arc<CpuMonitor>>,
}

#[derive(Default, Clone)]
struct Held {
    keys: Vec<Key>,
    buttons: Vec<PointerButton>,
}

struct Hook {
    id: u64,
    run: FrameHook,
}

pub(crate) struct Shared {
    pub(crate) slot: Arc<Slot>,
    pub(crate) next_operation: Arc<AtomicU64>,
    pub(crate) ring_bytes: usize,
    pub(crate) stop_patience: Duration,
}

pub(crate) struct MachineInner {
    key: String,
    name: String,
    dir: MachineDir,
    /// Reloaded after each edit, so the next start launches what the file says.
    model: Mutex<model::Machine>,
    engine: Arc<dyn Engine>,
    shared: Shared,
    run: Mutex<Run>,
    console: Arc<Console>,
    held: Mutex<Held>,
    hooks: Mutex<Arc<Vec<Hook>>>,
    next_hook: AtomicU64,
}

/// A handle on a machine of the library. Clones share it. Dropping every
/// handle leaves a running guest running.
#[derive(Clone)]
pub struct Machine {
    pub(crate) inner: Arc<MachineInner>,
}

impl Machine {
    pub(crate) fn new(
        key: String,
        dir: MachineDir,
        model: model::Machine,
        engine: Arc<dyn Engine>,
        shared: Shared,
    ) -> Machine {
        let name = dir
            .path()
            .file_name()
            .map_or_else(|| key.clone(), |n| n.to_string_lossy().into_owned());
        Machine {
            inner: Arc::new(MachineInner {
                console: Console::new(shared.ring_bytes),
                key,
                name,
                dir,
                model: Mutex::new(model),
                engine,
                shared,
                run: Mutex::new(Run {
                    phase: Phase::Idle,
                    session: None,
                    frames: None,
                    remote_display: false,
                    operation: None,
                    stop_requested: None,
                    power_button: None,
                    exit_note: None,
                    cpu: None,
                }),
                held: Mutex::default(),
                hooks: Mutex::new(Arc::new(Vec::new())),
                next_hook: AtomicU64::new(1),
            }),
        }
    }

    /// The machine's canonical directory: its identity in the library.
    pub fn key(&self) -> &str {
        &self.inner.key
    }

    pub fn name(&self) -> &str {
        &self.inner.name
    }

    pub(crate) fn slot(&self) -> Arc<Slot> {
        self.inner.shared.slot.clone()
    }

    pub(crate) fn engine(&self) -> Arc<dyn Engine> {
        self.inner.engine.clone()
    }

    pub fn view(&self) -> MachineView {
        self.inner.view()
    }

    /// Start the guest. Returns at once; the state moves to `Starting`, then
    /// `Running`, and an `Operation` event reports the end. Cancel before the
    /// engine launches to abandon it.
    pub fn start(&self) -> CoreResult<Operation> {
        let inner = &self.inner;
        {
            let mut run = inner.run.lock().unwrap();
            if run.phase != Phase::Idle || inner.live(&run) {
                let message = if run.phase == Phase::Editing {
                    "the machine's settings are being changed"
                } else {
                    "the machine is already running"
                };
                return Err(CoreError::new(ErrorKind::Busy, message));
            }
            run.phase = Phase::Starting;
            run.stop_requested = None;
            run.power_button = None;
            run.exit_note = None;
        }
        inner.shared.slot.raise(&[EventKind::State]);
        let machine = self.inner.clone();
        Ok(self.spawn("Start", true, move |control| {
            let result = machine.launch(control);
            machine.run.lock().unwrap().phase = Phase::Idle;
            machine.finish_operation();
            result.map(|()| Some(machine.key.clone()))
        }))
    }

    /// Stop the guest. A clean stop cannot be cancelled and cannot be
    /// repeated while it runs; a forced one may follow it. A clean stop that
    /// outlasts `stop_patience` fails with `Busy` and leaves the machine up.
    pub fn stop(&self, how: StopKind) -> CoreResult<Operation> {
        let inner = &self.inner;
        let session = {
            let mut run = inner.run.lock().unwrap();
            let session = inner.live_session(&run)?;
            if run.phase == Phase::Stopping && how == StopKind::Clean {
                return Err(CoreError::new(
                    ErrorKind::Busy,
                    "the machine is already stopping",
                ));
            }
            if how == StopKind::Clean {
                run.phase = Phase::Stopping;
            }
            run.stop_requested = Some(how);
            session
        };
        inner.shared.slot.raise(&[EventKind::State]);
        let machine = self.inner.clone();
        Ok(self.spawn(
            if how == StopKind::Clean {
                "Stop"
            } else {
                "Force stop"
            },
            false,
            move |_| {
                let result = match how {
                    StopKind::Clean => stop_within(session, machine.shared.stop_patience),
                    StopKind::Force => session.stop(Stop::Force).map_err(CoreError::engine),
                };
                {
                    let mut run = machine.run.lock().unwrap();
                    if how == StopKind::Clean {
                        run.phase = Phase::Idle;
                    }
                    if result.is_err() {
                        run.stop_requested = None;
                    }
                }
                machine.finish_operation();
                result?;
                Ok(None)
            },
        ))
    }

    pub fn control(&self, control: Control) -> CoreResult<()> {
        self.inner
            .session()?
            .control(control)
            .map_err(CoreError::engine)?;
        if control == Control::PowerButton {
            self.inner.run.lock().unwrap().power_button = Some(Instant::now());
        }
        self.inner.shared.slot.raise(&[EventKind::State]);
        Ok(())
    }

    /// Put `image` in the removable drive `drive`, or eject with None.
    pub fn change_media(&self, drive: &str, image: Option<&Path>) -> CoreResult<()> {
        self.inner
            .session()?
            .media(drive, image)
            .map_err(CoreError::engine)
    }

    #[cfg(test)]
    pub(super) fn pressed_power_button_at(&self, at: Instant) {
        self.inner.run.lock().unwrap().power_button = Some(at);
    }

    /// What the machine's file says. Reads the disk and may start the
    /// engine's program: off the UI thread.
    pub fn settings(&self) -> CoreResult<MachineSettings> {
        let loaded = crate::open(self.inner.engine.as_ref(), self.inner.dir.path())
            .map_err(CoreError::machine)?;
        settings::read(&loaded).map_err(CoreError::machine)
    }

    /// Edit the machine's file: `Busy` unless it is stopped and no other edit
    /// runs, and a start waits out an edit. Off the UI thread.
    pub fn apply_change(&self, change: &Change) -> CoreResult<()> {
        let inner = &self.inner;
        {
            let mut run = inner.run.lock().unwrap();
            if run.phase != Phase::Idle || inner.live(&run) {
                return Err(CoreError::new(
                    ErrorKind::Busy,
                    "stop the machine before changing its settings",
                ));
            }
            run.phase = Phase::Editing;
        }
        let _editing = Editing(&inner.run);
        let (engine, dir) = (inner.engine.as_ref(), inner.dir.path());
        crate::apply_change(engine, dir, change).map_err(CoreError::machine)?;
        inner.shared.slot.raise(&[EventKind::Library]);
        let stale = |error: Box<dyn std::error::Error + Send + Sync>| {
            CoreError::new(
                ErrorKind::Internal,
                format!("the change was saved, but the machine could not be reloaded ({error}); open it again"),
            )
        };
        let loaded = crate::open(engine, dir).map_err(stale)?;
        let mut model =
            model::Machine::from_file(&loaded.catalogue, &loaded.file).map_err(stale)?;
        model.engine.id = engine.info().id;
        *inner.model.lock().unwrap() = model;
        Ok(())
    }

    pub fn screendump(&self, head: usize, path: &Path) -> CoreResult<()> {
        self.inner
            .session()?
            .screendump(head, path)
            .map_err(CoreError::engine)
    }

    pub fn send_input(&self, events: &[InputEvent]) -> CoreResult<()> {
        let session = self.inner.session()?;
        session.send_input(events).map_err(CoreError::engine)?;
        self.inner.track(events);
        Ok(())
    }

    pub fn send_key(&self, key: Key, down: bool) -> CoreResult<()> {
        self.send_input(&[InputEvent::Key { key, down }])
    }

    /// Lift every key and button the guest believes is down, for a window
    /// that lost focus mid-press. Returns how many it released.
    pub fn release_all_keys(&self) -> CoreResult<u32> {
        let session = self.inner.session()?;
        let held = self.inner.held.lock().unwrap().clone();
        let events: Vec<InputEvent> = held
            .keys
            .iter()
            .map(|&key| InputEvent::Key { key, down: false })
            .chain(held.buttons.iter().map(|&button| InputEvent::Button {
                button,
                down: false,
            }))
            .collect();
        if events.is_empty() {
            return Ok(0);
        }
        session.send_input(&events).map_err(CoreError::engine)?;
        // Forget the keys only once the guest has been told: after a failed
        // send they are still down.
        *self.inner.held.lock().unwrap() = Default::default();
        Ok(events.len() as u32)
    }

    pub fn heads(&self) -> Vec<HeadView> {
        self.inner.heads()
    }

    /// The latest per-CPU load, measured on the host (see [`CpuLoadReport`]).
    /// Never blocks: a background thread samples while this is being called,
    /// and stops a few seconds after the last call. Empty with a reason when
    /// the machine is not running or the engine cannot measure it.
    pub fn cpu_load(&self) -> CpuLoadReport {
        let Ok(session) = self.inner.session() else {
            return CpuLoadReport::none("the machine is not running");
        };
        let mut run = self.inner.run.lock().unwrap();
        let monitor = match run.cpu.clone().filter(|m| m.serves(&session)) {
            Some(monitor) => monitor,
            None => match CpuMonitor::start(&session, cpu::INTERVAL) {
                Some(monitor) => run.cpu.insert(Arc::new(monitor)).clone(),
                None => return CpuLoadReport::none("the sampler thread did not start"),
            },
        };
        drop(run);
        monitor.latest()
    }

    /// The rows of `head` that changed since the last call, copied into
    /// `pixels` (cleared first), or None. One consumer per head.
    pub fn take_frame(&self, head: usize, pixels: &mut Vec<u8>) -> CoreResult<Option<Frame>> {
        Ok(self.inner.frames()?.take_frame(head, pixels))
    }

    pub fn cursor(&self, head: usize, known_gen: u32) -> CoreResult<Option<CursorInfo>> {
        Ok(self.inner.frames()?.cursor(head, known_gen))
    }

    /// Frame counters of the display client, for tests of zero copy.
    pub fn frame_stats(&self) -> Option<FrameStats> {
        self.inner.frames().ok()?.stats()
    }

    /// Tell the engine whether any view shows the machine. While hidden it
    /// sends no frame wake-ups; showing it again wakes once if it changed.
    pub fn set_visible(&self, visible: bool) -> CoreResult<()> {
        self.inner.frames()?.set_visible(visible);
        Ok(())
    }

    /// Call `hook` from an engine thread whenever a head has news, with no
    /// allocation per call.
    pub fn add_frame_hook(&self, hook: FrameHook) -> HookId {
        let id = self.inner.next_hook.fetch_add(1, Ordering::Relaxed);
        let mut hooks = self.inner.hooks.lock().unwrap();
        let mut next: Vec<Hook> = hooks
            .iter()
            .map(|h| Hook {
                id: h.id,
                run: h.run.clone(),
            })
            .collect();
        next.push(Hook { id, run: hook });
        *hooks = Arc::new(next);
        HookId(id)
    }

    pub fn remove_frame_hook(&self, id: HookId) {
        let mut hooks = self.inner.hooks.lock().unwrap();
        let next: Vec<Hook> = hooks
            .iter()
            .filter(|h| h.id != id.0)
            .map(|h| Hook {
                id: h.id,
                run: h.run.clone(),
            })
            .collect();
        *hooks = Arc::new(next);
    }

    /// What the guest printed from byte `from` on, at most `max` bytes.
    pub fn console_read(&self, from: u64, max: usize) -> ConsoleChunk {
        self.inner.console.read(from, max)
    }

    pub fn console_send(&self, bytes: &[u8]) -> CoreResult<()> {
        self.inner.console.send(bytes)
    }

    fn spawn<F>(&self, title: &str, cancellable: bool, work: F) -> Operation
    where
        F: FnOnce(&OpControl) -> CoreResult<Option<String>> + Send + 'static,
    {
        let inner = &self.inner;
        let id = inner.shared.next_operation.fetch_add(1, Ordering::Relaxed);
        inner.run.lock().unwrap().operation = Some(id);
        operation::spawn(
            id,
            title,
            Some(inner.key.clone()),
            cancellable,
            inner.shared.slot.clone(),
            work,
        )
    }
}

/// Wait for a clean stop for at most `patience`. The engine's own wait goes on
/// in the background: QEMU may be folding a disk image and must not be killed.
fn stop_within(session: Arc<dyn Session>, patience: Duration) -> CoreResult<()> {
    let (done, result) = mpsc::channel();
    std::thread::Builder::new()
        .name("origami-stop".into())
        .spawn(move || {
            let _ = done.send(session.stop(Stop::Clean { timeout: None }));
        })
        .map_err(|e| CoreError::new(ErrorKind::Internal, e.to_string()))?;
    match result.recv_timeout(patience) {
        Ok(result) => result.map_err(CoreError::engine),
        Err(mpsc::RecvTimeoutError::Disconnected) => Err(CoreError::new(
            ErrorKind::Internal,
            "the stop thread ended without an answer",
        )),
        Err(mpsc::RecvTimeoutError::Timeout) => Err(CoreError::new(
            ErrorKind::Busy,
            format!(
                "The machine did not power off within {} seconds. The guest may be ignoring the request, or QEMU may still be saving a disk. Wait, or use Force Quit.",
                patience.as_secs()
            ),
        )),
    }
}

impl MachineInner {
    fn live(&self, run: &Run) -> bool {
        run.session
            .as_ref()
            .is_some_and(|s| !matches!(s.status().state, RunState::Exited { .. }))
    }

    fn live_session(&self, run: &Run) -> CoreResult<Arc<dyn Session>> {
        match &run.session {
            Some(s) if self.live(run) => Ok(s.clone()),
            _ => Err(CoreError::new(
                ErrorKind::NotRunning,
                "the machine is not running",
            )),
        }
    }

    fn session(&self) -> CoreResult<Arc<dyn Session>> {
        let run = self.run.lock().unwrap();
        self.live_session(&run)
    }

    fn frames(&self) -> CoreResult<Arc<dyn FrameSource>> {
        let run = self.run.lock().unwrap();
        match (&run.frames, run.remote_display) {
            (Some(frames), _) => Ok(frames.clone()),
            (None, true) => Err(CoreError::new(
                ErrorKind::Unsupported,
                "this machine's display is a socket, which needs the display client",
            )),
            (None, false) => Err(CoreError::new(
                ErrorKind::NotRunning,
                "the machine has no display",
            )),
        }
    }

    fn heads(&self) -> Vec<HeadView> {
        let frames = self.run.lock().unwrap().frames.clone();
        frames.map_or_else(Vec::new, |frames| {
            frames
                .heads()
                .iter()
                .enumerate()
                .map(|(index, head)| HeadView {
                    index: index as u32,
                    width: head.width,
                    height: head.height,
                    seq: head.seq,
                    active: head.active,
                })
                .collect()
        })
    }

    fn track(&self, events: &[InputEvent]) {
        let mut held = self.held.lock().unwrap();
        for event in events {
            match *event {
                InputEvent::Key { key, down: true } if !held.keys.contains(&key) => {
                    held.keys.push(key);
                }
                InputEvent::Key { key, down: false } => held.keys.retain(|&k| k != key),
                InputEvent::Button { button, down: true } if !held.buttons.contains(&button) => {
                    held.buttons.push(button);
                }
                InputEvent::Button {
                    button,
                    down: false,
                } => {
                    held.buttons.retain(|&b| b != button);
                }
                _ => {}
            }
        }
    }

    /// The stop asked for: this core's, a recent power button, or the runner's
    /// note of another process's (read once, at the exit).
    fn stop_requested(&self) -> Option<StopKind> {
        let mut run = self.run.lock().unwrap();
        let dir = self.dir.path();
        let note = run
            .exit_note
            .get_or_insert_with(|| origami_machine::control::read_exit(dir).unwrap_or_default());
        let asked = match note.requested.as_deref() {
            Some("force") => Some(StopKind::Force),
            Some(_) => Some(StopKind::Clean),
            None => None,
        };
        let button = run
            .power_button
            .filter(|at| at.elapsed() < POWER_BUTTON_PATIENCE)
            .map(|_| StopKind::Clean);
        run.stop_requested.or(asked).or(button)
    }

    fn view(&self) -> MachineView {
        let (phase, session, operation, frames) = {
            let run = self.run.lock().unwrap();
            (
                run.phase,
                run.session.clone(),
                run.operation,
                run.frames.clone(),
            )
        };
        let facts = session.map(|s| s.status().state);
        let requested = if matches!(facts, Some(RunState::Exited { .. })) {
            self.stop_requested()
        } else {
            None
        };
        let (exit_code, last_error) = match &facts {
            Some(RunState::Exited {
                code, last_error, ..
            }) => (*code, last_error.clone()),
            Some(RunState::Unknown(why)) => (None, Some(why.clone())),
            _ => (None, None),
        };
        let (exit_reason, exit_detail) = match &facts {
            Some(RunState::Exited {
                code,
                signal,
                last_error,
            }) => {
                let how = match (code, signal) {
                    (Some(0), _) => None,
                    (Some(code), _) => Some(format!("exit status {code}")),
                    (None, Some(signal)) => Some(format!("signal {signal}")),
                    (None, None) => None,
                };
                let detail = last_error.clone().or(how);
                let reason = match (code, signal) {
                    // A forced stop kills QEMU, so a signal after one is ours.
                    (None, Some(_)) if requested == Some(StopKind::Force) => {
                        ExitReason::HostStopped
                    }
                    (Some(0), _) if requested.is_some() => ExitReason::HostStopped,
                    (Some(0), _) => ExitReason::GuestPoweredOff,
                    (None, None) => ExitReason::RunnerError,
                    _ => ExitReason::Crashed,
                };
                (Some(reason), detail)
            }
            _ => (None, None),
        };
        let (state, reason) = match (phase, &facts) {
            (Phase::Starting, _) => (
                MachineState::Starting,
                "The machine is starting.".to_string(),
            ),
            (Phase::Stopping, _) => (
                MachineState::Stopping,
                "The machine is stopping.".to_string(),
            ),
            (_, None) => (
                MachineState::NotRunning,
                "The machine is not running.".to_string(),
            ),
            (_, Some(RunState::Running)) => {
                let blank = frames
                    .as_ref()
                    .filter(|frames| !frames.heads().iter().any(|h| h.active))
                    .and_then(|frames| frames.why_blank());
                let reason = match blank {
                    Some(why) => format!(
                        "The machine is running, with no picture: {}.",
                        why.trim_end_matches('.')
                    ),
                    None => "The machine is running.".to_string(),
                };
                (MachineState::Running, reason)
            }
            // The engine's program lives; the error says what it did not answer.
            (_, Some(RunState::Unknown(why))) => (
                MachineState::Running,
                format!("The machine's state is unknown: {why}"),
            ),
            (_, Some(RunState::Prelaunch | RunState::Paused)) => {
                (MachineState::Paused, "The machine is paused.".to_string())
            }
            (_, Some(RunState::Exited { last_error, .. })) => {
                let how = match (&exit_reason, last_error, &exit_detail) {
                    (_, Some(error), _) => format!("The machine exited: {error}"),
                    (Some(ExitReason::GuestPoweredOff), ..) => "The machine has shut down.".into(),
                    (Some(ExitReason::HostStopped), ..) => "The machine was stopped.".into(),
                    (Some(ExitReason::Crashed), _, Some(how)) => {
                        format!("The machine ended: {how}.")
                    }
                    _ => "The machine was ended.".to_string(),
                };
                (MachineState::Exited, how)
            }
        };
        let (connected, down_reason, total) = self.console.status();
        MachineView {
            key: self.key.clone(),
            name: self.name.clone(),
            product: self.model.lock().unwrap().product.clone(),
            engine: self.engine.info().id,
            state,
            exit_code,
            exit_reason,
            exit_detail,
            last_error,
            reason,
            heads: self.heads(),
            console: ConsoleView {
                connected,
                down_reason,
                total,
            },
            operation: operation
                .filter(|_| matches!(state, MachineState::Starting | MachineState::Stopping)),
        }
    }

    /// The engine has news: a new picture, or the exit.
    fn engine_woke(&self) {
        self.shared
            .slot
            .raise(&[EventKind::State, EventKind::Display]);
        let hooks = self.hooks.lock().unwrap().clone();
        for hook in hooks.iter() {
            (hook.run)();
        }
    }

    fn finish_operation(&self) {
        self.run.lock().unwrap().operation = None;
        self.shared.slot.raise(&[EventKind::State]);
    }

    fn launch(self: &Arc<Self>, control: &OpControl) -> CoreResult<()> {
        // A convenience setting must not keep the machine from starting.
        if let Err(error) = crate::apply_first_boot(self.engine.as_ref(), self.dir.path()) {
            eprintln!("warning: could not apply the first-boot settings: {error}");
        }
        let model = self.model.lock().unwrap().clone();
        let report = self.engine.validate(&model, &self.dir);
        if let Some(problem) = report.errors().next() {
            return Err(CoreError::new(ErrorKind::Invalid, problem.message.clone()));
        }
        let wiring = Wiring {
            display: DisplayRequest::Shared { gpu: None },
            console_port: origami_machine::control::free_port().map_err(CoreError::machine)?,
            control_port: origami_machine::control::free_port().map_err(CoreError::machine)?,
            name: format!("origami-{}", self.name),
            paused: false,
            shares: None,
            once: vec![],
        };
        let plan = self
            .engine
            .plan(&model, &self.dir, &wiring)
            .map_err(CoreError::engine)?;
        if control.is_cancelled() {
            return Err(CoreError::new(
                ErrorKind::Cancelled,
                "the start was cancelled",
            ));
        }
        let weak: Weak<MachineInner> = Arc::downgrade(self);
        let session = self
            .engine
            .launch(
                plan,
                Arc::new(move |_| {
                    if let Some(machine) = weak.upgrade() {
                        machine.engine_woke();
                    }
                }),
            )
            .map_err(CoreError::engine)?;
        let (frames, remote_display) = match session.display() {
            DisplayEndpoint::Frames(frames) => (Some(frames), false),
            DisplayEndpoint::Socket(_) => (None, true),
            DisplayEndpoint::None => (None, false),
        };
        self.console.reset();
        let opened = session
            .consoles()
            .into_iter()
            .find(|console| console.primary)
            .map(|console| session.open_console(&console.name));
        let has_frames = frames.is_some();
        {
            let mut run = self.run.lock().unwrap();
            run.session = Some(session);
            run.frames = frames;
            run.remote_display = remote_display;
        }
        // A wake that came before the frames were stored found nothing to take.
        if has_frames {
            self.engine_woke();
        }
        *self.held.lock().unwrap() = Held::default();
        match opened {
            Some(Ok(io)) => self.attach_console(io),
            Some(Err(error)) => self.console.down(error.to_string()),
            None => self.console.down("the machine has no console".into()),
        }
        Ok(())
    }

    fn attach_console(self: &Arc<Self>, io: Box<dyn ConsoleIo>) {
        let (reader, writer) = match io.split() {
            Ok(halves) => halves,
            Err(error) => return self.console.down(error.to_string()),
        };
        self.console.connect(writer);
        let machine = self.clone();
        let spawned = std::thread::Builder::new()
            .name("origami-console".into())
            .spawn(move || {
                let console = machine.console.clone();
                console.pump(reader, || {
                    machine.shared.slot.raise(&[EventKind::Console]);
                });
                machine
                    .shared
                    .slot
                    .raise(&[EventKind::Console, EventKind::State]);
            });
        if let Err(error) = spawned {
            self.console.down(error.to_string());
        }
    }
}
