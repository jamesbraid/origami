//! The QEMU engine: machines are started with the product's own runner and
//! `display-shm`, pictures come from QEMU's shared-memory display, and input
//! goes back over QMP.
//!
//! Both transports are v1 and sit behind boundaries meant to be replaced: the
//! frame source ([`display::Display`], behind [`Engine::take_frame`]) and the
//! input sink ([`crate::core::InputSink`]).

mod display;
mod msg;
mod qcode;
mod qmp_input;
pub mod shm;
mod transport;

use crate::core::{Engine, FrameUpdate, HeadInfo, InputEvent, InputSink, MachineId, Result, Wake};
use display::{Display, Taken};
use origami::{control, runtime};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

pub use display::Stats;

/// How long the input thread waits with nothing to send before telling its sink.
const INPUT_IDLE: Duration = Duration::from_millis(300);

pub struct QemuEngine {
    /// The `origami` executable that runs machines in the background.
    runner: PathBuf,
    shared: Arc<Shared>,
    next: AtomicU32,
}

struct Shared {
    machines: Mutex<HashMap<MachineId, Arc<Machine>>>,
    wake: Mutex<Option<Wake>>,
}

impl Shared {
    fn wake(&self, id: MachineId) {
        if let Some(w) = lock(&self.wake).clone() {
            w(id);
        }
    }
}

struct Machine {
    id: MachineId,
    dir: PathBuf,
    shm_name: String,
    display: Display,
    visible: AtomicBool,
    stop: AtomicBool,
    /// Set once the display's machine is gone.
    gone: AtomicBool,
    input: Mutex<Option<Sender<InputEvent>>>,
    shared: Arc<Shared>,
    threads: Mutex<Vec<std::thread::JoinHandle<()>>>,
    input_thread: Mutex<Option<std::thread::JoinHandle<()>>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl QemuEngine {
    /// An engine that starts machines with `runner`, the `origami` executable.
    pub fn new(runner: PathBuf) -> QemuEngine {
        QemuEngine {
            runner,
            shared: Arc::new(Shared {
                machines: Mutex::new(HashMap::new()),
                wake: Mutex::new(None),
            }),
            next: AtomicU32::new(1),
        }
    }

    /// The `origami` beside this executable, or the one `ORIGAMI_BIN` names.
    pub fn locate_runner() -> Result<PathBuf> {
        if let Some(path) = std::env::var_os("ORIGAMI_BIN") {
            return Ok(path.into());
        }
        let exe = std::env::current_exe()?;
        let name = if cfg!(windows) {
            "origami.exe"
        } else {
            "origami"
        };
        let beside = exe.parent().ok_or("no executable directory")?.join(name);
        if beside.is_file() {
            Ok(beside)
        } else {
            Err(format!("no {name} beside {}; set ORIGAMI_BIN", exe.display()).into())
        }
    }

    fn machine(&self, id: MachineId) -> Result<Arc<Machine>> {
        lock(&self.shared.machines)
            .get(&id)
            .cloned()
            .ok_or_else(|| format!("no machine {}", id.0).into())
    }

    pub fn stats(&self, id: MachineId) -> Option<Stats> {
        Some(self.machine(id).ok()?.display.stats())
    }
}

impl Engine for QemuEngine {
    fn id(&self) -> &'static str {
        "qemu"
    }

    fn set_wake(&self, wake: Wake) {
        *lock(&self.shared.wake) = Some(wake);
    }

    fn start(&self, dir: &Path) -> Result<MachineId> {
        let dir = dir
            .canonicalize()
            .map_err(|e| format!("cannot open machine directory {}: {e}", dir.display()))?;
        if let Some(m) = lock(&self.shared.machines).values().find(|m| m.dir == dir) {
            return Ok(m.id);
        }
        if !control::is_running(&dir).map_err(|e| e.to_string())? {
            runtime::start_background_as(&self.runner, &dir, runtime::Display::Shm, &[])
                .map_err(|e| e.to_string())?;
        }
        let id = MachineId(self.next.fetch_add(1, Ordering::Relaxed));
        let machine = Arc::new(Machine {
            id,
            shm_name: runtime::shm_name(&dir),
            dir,
            display: Display::default(),
            visible: AtomicBool::new(true),
            stop: AtomicBool::new(false),
            gone: AtomicBool::new(false),
            input: Mutex::new(None),
            shared: self.shared.clone(),
            threads: Mutex::new(Vec::new()),
            input_thread: Mutex::new(None),
        });
        let reader = {
            let m = machine.clone();
            std::thread::Builder::new()
                .name("shm-display".into())
                .spawn(move || reader_thread(&m))?
        };
        let (tx, rx) = mpsc::channel();
        *lock(&machine.input) = Some(tx);
        let input = {
            let m = machine.clone();
            let sink = Box::new(qmp_input::QmpInput::new(m.dir.clone()));
            std::thread::Builder::new()
                .name("qmp-input".into())
                .spawn(move || input_worker(sink, rx))?
        };
        lock(&machine.threads).push(reader);
        *lock(&machine.input_thread) = Some(input);
        lock(&self.shared.machines).insert(id, machine);
        Ok(id)
    }

    fn is_running(&self, id: MachineId) -> bool {
        self.machine(id)
            .is_ok_and(|m| !m.gone.load(Ordering::Acquire))
    }

    fn stop(&self, id: MachineId) -> Result<()> {
        let m = self.machine(id)?;
        m.stop.store(true, Ordering::Release);
        *lock(&m.input) = None;
        // The input thread's QMP connection must be gone before `stop` asks
        // QEMU to quit over a connection of its own.
        if let Some(t) = lock(&m.input_thread).take() {
            let _ = t.join();
        }
        let result = control::stop(&m.dir).map_err(|e| e.to_string().into());
        for t in lock(&m.threads).drain(..) {
            let _ = t.join();
        }
        lock(&self.shared.machines).remove(&id);
        result
    }

    fn heads(&self, id: MachineId) -> Result<Vec<HeadInfo>> {
        Ok(self.machine(id)?.display.heads())
    }

    fn take_frame(
        &self,
        id: MachineId,
        head: usize,
        pixels: &mut Vec<u8>,
    ) -> Result<Option<FrameUpdate>> {
        let m = self.machine(id)?;
        match m.display.take(head, pixels)? {
            Taken::Frame(update) => Ok(Some(update)),
            Taken::Nothing => Ok(None),
            Taken::Retry => {
                // Ask again from the UI thread rather than spin here.
                m.shared.wake(id);
                Ok(None)
            }
        }
    }

    fn set_visible(&self, id: MachineId, visible: bool) {
        let Ok(m) = self.machine(id) else { return };
        let was = m.visible.swap(visible, Ordering::AcqRel);
        if visible && !was && m.display.pending() {
            self.shared.wake(id);
        }
    }

    fn send_input(&self, id: MachineId, event: InputEvent) -> Result<()> {
        let m = self.machine(id)?;
        let tx = lock(&m.input);
        match tx.as_ref() {
            Some(tx) => tx
                .send(event)
                .map_err(|_| "the input channel is closed".into()),
            None => Err("the machine is stopping".into()),
        }
    }
}

/// The display thread: keep the connection, wake the UI when something moves,
/// and pick the machine up again if it is started again after it ended.
fn reader_thread(m: &Arc<Machine>) {
    let mut link = transport::UnixLink {
        name: m.shm_name.clone(),
        dir: m.dir.clone(),
    };
    let wake = || {
        if m.visible.load(Ordering::Acquire) {
            m.shared.wake(m.id);
        }
    };
    loop {
        let exit = transport::run(
            &mut link,
            &m.display,
            &m.stop,
            transport::Timing::default(),
            &wake,
        );
        if exit == transport::Exit::NeverCame {
            eprintln!(
                "origami-gui: no shared-memory display from {} after 300 s",
                m.dir.display()
            );
        }
        if exit == transport::Exit::Stopped {
            return;
        }
        m.gone.store(true, Ordering::Release);
        // The UI shows the state even for a machine nobody is looking at.
        m.shared.wake(m.id);
        while !m.stop.load(Ordering::Acquire) && !transport::Link::alive(&mut link) {
            std::thread::sleep(Duration::from_secs(2));
        }
        if m.stop.load(Ordering::Acquire) {
            return;
        }
        m.gone.store(false, Ordering::Release);
        m.shared.wake(m.id);
    }
}

fn input_worker(mut sink: Box<dyn InputSink>, rx: Receiver<InputEvent>) {
    // QEMU serves one QMP client at a time, so the sink drops its connection
    // after a quiet spell, which leaves the machine's QMP port to `origami
    // status`, `stop` and the rest.
    loop {
        let first = match rx.recv_timeout(INPUT_IDLE) {
            Ok(e) => e,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                sink.idle();
                continue;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        };
        let mut batch = vec![first];
        while let Ok(e) = rx.try_recv() {
            batch.push(e);
        }
        if let Err(e) = sink.send(&qcode::coalesce(batch)) {
            eprintln!("origami-gui: input: {e}");
        }
    }
}
