//! The QEMU engine: machines are started with the product's own runner and
//! `display-shm`, pictures come from QEMU's shared-memory display (protocol
//! version 3, the generic xrgb path), and input goes back as binary messages
//! on the same socket.
//!
//! Where the GUI's GPU can import memory ([`Importer`]), QEMU is asked to
//! allocate scanout memory the GPU imports and the UI draws from it in place;
//! otherwise regions are plain shared memory and the changed lines are copied.
//! Both sit behind boundaries meant to be replaced: the frame source
//! ([`display::Display`], behind [`Engine::take_frame`]) and the input sink
//! ([`crate::core::InputSink`]).

mod control;
mod display;
mod import;
mod input;
mod proto;
mod region;
mod socket_input;
mod transport;

use crate::core::{
    CursorInfo, Engine, Frame, HeadInfo, InputEvent, InputSink, MachineId, Result, Wake,
};
use display::{Display, Taken};
use origami::{control as machine_control, runtime};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

pub use display::Stats;
pub use import::Importer;
pub use proto::Region;
#[cfg(target_os = "macos")]
pub use proto::CAP_METAL;
#[cfg(any(target_os = "linux", windows))]
pub use proto::CAP_VULKAN;
#[cfg(any(target_os = "linux", windows))]
pub use proto::REGION_DEDICATED;
pub use region::{Handle, Memory};

pub struct QemuEngine {
    /// The `origami` executable that runs machines in the background.
    runner: PathBuf,
    shared: Arc<Shared>,
    next: AtomicU32,
    importer: Mutex<Option<Arc<dyn Importer>>>,
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
    display: Display,
    uplink: transport::Uplink,
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
            importer: Mutex::new(None),
        }
    }

    /// Take regions into the GPU with `importer`, for machines started after
    /// this. Where it imports exported memory, QEMU is told to export on its
    /// adapter.
    pub fn set_importer(&self, importer: Arc<dyn Importer>) {
        *lock(&self.importer) = Some(importer);
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
        let importer = lock(&self.importer).clone();
        if machine_control::running(&dir)
            .map_err(|e| e.to_string())?
            .is_none()
        {
            let gpu = importer
                .as_ref()
                .and_then(|i| i.export_uuid())
                .map(runtime::GpuUuid);
            let start = |gpu| {
                runtime::start_background_as(&self.runner, &dir, runtime::Display::Shm { gpu }, &[])
                    .map_err(|e| e.to_string())
            };
            if let Err(e) = start(gpu) {
                // A QEMU that predates the GPU export does not know the
                // property and refuses to start; ask for plain shared memory.
                if gpu.is_none() || !e.contains("display-shm-gpu") {
                    return Err(e.into());
                }
                eprintln!("origami-gui: this QEMU cannot export GPU memory; copying instead");
                start(None)?;
            }
        }
        let id = MachineId(self.next.fetch_add(1, Ordering::Relaxed));
        let display = Display::default();
        if let Some(importer) = importer {
            display.set_importer(importer);
        }
        let machine = Arc::new(Machine {
            id,
            dir,
            display,
            uplink: Arc::new(Mutex::new(None)),
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
            let sink = Box::new(socket_input::SocketInput::new(m.uplink.clone()));
            std::thread::Builder::new()
                .name("display-input".into())
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
        // Input stops before the machine does.
        if let Some(t) = lock(&m.input_thread).take() {
            let _ = t.join();
        }
        let result = machine_control::stop(&m.dir).map_err(|e| e.to_string().into());
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
    ) -> Result<Option<Frame>> {
        let m = self.machine(id)?;
        let taken = m.display.take(head, pixels)?;
        match taken {
            Taken::Retry => {
                // Ask again from the UI thread rather than spin here.
                m.shared.wake(id);
                return Ok(None);
            }
            _ if m.display.visible() => {
                // Armed again for the next change, then check it did not
                // already happen.
                if m.display.arm(head) {
                    m.shared.wake(id);
                }
            }
            _ => {}
        }
        Ok(match taken {
            Taken::Frame(update) => Some(Frame::Rows(update)),
            Taken::Imported(imported) => Some(Frame::Imported(imported)),
            _ => None,
        })
    }

    fn display_backend(&self, id: MachineId) -> Option<String> {
        self.machine(id).ok()?.display.backend()
    }

    fn cursor(&self, id: MachineId, head: usize, known_gen: u32) -> Option<CursorInfo> {
        self.machine(id).ok()?.display.cursor(head, known_gen)
    }

    fn set_visible(&self, id: MachineId, visible: bool) {
        let Ok(m) = self.machine(id) else { return };
        let was = m.display.visible();
        m.display.set_visible(visible);
        if visible && !was && m.display.arm_all() {
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
    let mut link = transport::SocketLink {
        socket: runtime::display_socket(&m.dir),
        dir: m.dir.clone(),
    };
    let wake = || {
        if m.display.visible() {
            m.shared.wake(m.id);
        }
    };
    loop {
        let exit = transport::run(
            &mut link,
            &m.display,
            &m.uplink,
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
    // Everything that arrived while the last batch was being written goes out
    // together, so one host frame of input is one guest report.
    while let Ok(first) = rx.recv() {
        let mut batch = vec![first];
        while let Ok(e) = rx.try_recv() {
            batch.push(e);
        }
        if let Err(e) = sink.send(&batch) {
            eprintln!("origami-gui: input: {e}");
        }
    }
}
