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

use crate::core::status::Facts;
use crate::core::{
    ConsoleKind, ConsoleLink, CursorInfo, Engine, Frame, HeadInfo, InputEvent, InputSink,
    MachineId, Power, Result, Wake,
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
    console: Console,
    /// One `nvram` command at a time: each holds the machine's edit lock, and
    /// one that finds it held reports a running machine.
    nvram: Mutex<()>,
}

/// The serial console's connection: a thread keeps it up and collects what
/// the guest writes until the UI takes it.
struct Console {
    inbox: Mutex<Vec<u8>>,
    link: Mutex<ConsoleLink>,
    writer: Mutex<Option<std::net::TcpStream>>,
    /// Bytes the guest wrote since the machine was attached, for the debug
    /// display.
    total: std::sync::atomic::AtomicU64,
}

/// What the console keeps for a UI that is slow to look: more than this and
/// the oldest is dropped, as a terminal's scrollback would drop it anyway.
const CONSOLE_INBOX: usize = 4 << 20;

/// How much of `serial.log` primes a console that connects to a guest that
/// has been talking: the console socket carries only what is written while
/// someone is connected, so a login prompt printed earlier would never show.
const CONSOLE_PRIME: u64 = 16 << 10;

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

    /// Start the machine in `dir` in the background with the shared-memory
    /// display, exporting its scanout memory on the viewer's GPU where it can.
    fn launch(&self, dir: &Path) -> Result<()> {
        let importer = lock(&self.importer).clone();
        let gpu = importer
            .as_ref()
            .and_then(|i| i.export_uuid())
            .map(runtime::GpuUuid);
        let start = |gpu| {
            runtime::start_background_as(&self.runner, dir, runtime::Display::Shm { gpu }, &[])
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
        Ok(())
    }

    /// Run the product's own command and return what it printed.
    fn product(&self, args: &[&std::ffi::OsStr]) -> Result<String> {
        let out = std::process::Command::new(&self.runner)
            .args(args)
            .stdin(std::process::Stdio::null())
            .output()?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).into_owned())
        } else {
            let err = String::from_utf8_lossy(&out.stderr);
            let err = err.trim();
            Err(if err.is_empty() {
                format!("origami exited with {}", out.status)
            } else {
                err.trim_start_matches("error: ").to_string()
            }
            .into())
        }
    }

    /// Run the product's `nvram` command on `m`. Another origami, such as a
    /// `get` typed at a shell, can hold the edit lock for an instant; that
    /// looks like a running machine, so try again before believing it.
    fn nvram(&self, m: &Machine, args: &[&str]) -> Result<String> {
        let _one_at_a_time = lock(&m.nvram);
        let mut all: Vec<&std::ffi::OsStr> = vec!["nvram".as_ref(), m.dir.as_os_str()];
        all.extend(args.iter().map(|a| std::ffi::OsStr::new(*a)));
        let mut last = String::new();
        for _ in 0..20 {
            match self.product(&all) {
                Err(e) if e.to_string().contains("stop the machine before") => {
                    last = e.to_string();
                    if runner_alive(&m.dir) {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(150));
                }
                other => return other,
            }
        }
        Err(last.into())
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
            self.launch(&dir)?;
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
            console: Console {
                inbox: Mutex::new(Vec::new()),
                link: Mutex::new(ConsoleLink::Down("not connected yet".into())),
                writer: Mutex::new(None),
                total: std::sync::atomic::AtomicU64::new(0),
            },
            nvram: Mutex::new(()),
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
        let console = {
            let m = machine.clone();
            std::thread::Builder::new()
                .name("serial-console".into())
                .spawn(move || console_thread(&m))?
        };
        lock(&machine.threads).push(reader);
        lock(&machine.threads).push(console);
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

    fn dir(&self, id: MachineId) -> Option<PathBuf> {
        Some(self.machine(id).ok()?.dir.clone())
    }

    fn facts(&self, id: MachineId) -> Facts {
        let Ok(m) = self.machine(id) else {
            return Facts::default();
        };
        let socket = runtime::display_socket(&m.dir);
        let link = m.display.link_facts();
        let picture = m
            .display
            .heads()
            .into_iter()
            .find(|h| h.active)
            .map(|h| (h.width, h.height));
        Facts {
            running: runner_alive(&m.dir),
            socket_exists: socket.exists(),
            socket,
            link,
            runner_error: None,
            picture,
        }
    }

    fn debug(&self, id: MachineId) -> Vec<String> {
        let Ok(m) = self.machine(id) else {
            return Vec::new();
        };
        let l = m.display.link_facts();
        let s = m.display.stats();
        let heads = m.display.heads();
        let mut out = vec![
            format!(
                "protocol: socket {} | connected {} | welcomed {} | regions {} | messages {}",
                if runtime::display_socket(&m.dir).exists() {
                    "present"
                } else {
                    "absent"
                },
                l.connected,
                l.welcomed,
                l.regions,
                l.messages
            ),
            format!(
                "link: {} connects, {} failed attempts{}{}",
                l.connects,
                l.connect_failures,
                l.connect_error
                    .map_or(String::new(), |e| format!(", last: {e}")),
                l.protocol_error
                    .map_or(String::new(), |e| format!(", dropped: {e}")),
            ),
            format!(
                "frames: {} taken, {} lines ({} imported), {} bytes copied | backend: {}",
                s.frames,
                s.lines,
                s.imported_lines,
                s.copied_bytes,
                m.display.backend().unwrap_or_else(|| "none yet".into())
            ),
        ];
        for (i, h) in heads.iter().enumerate() {
            out.push(format!(
                "head {i}: {}x{} seq {} {}",
                h.width,
                h.height,
                h.seq,
                if h.active { "active" } else { "no picture" }
            ));
        }
        out.push(format!(
            "serial: {} | {} bytes received",
            match &*lock(&m.console.link) {
                ConsoleLink::Connected => "connected".to_string(),
                ConsoleLink::Down(why) => format!("down ({why})"),
            },
            m.console.total.load(Ordering::Relaxed)
        ));
        out
    }

    fn power(&self, id: MachineId, action: Power) -> Result<()> {
        let m = self.machine(id)?;
        match action {
            Power::Start => {
                // Not while an nvram command holds the machine's edit lock.
                let _one_at_a_time = lock(&m.nvram);
                if machine_control::is_locked(&m.dir).map_err(|e| e.to_string())? {
                    return Err("the machine is already running".into());
                }
                self.launch(&m.dir)
            }
            Power::Stop => machine_control::stop(&m.dir).map_err(|e| e.to_string().into()),
            Power::ForceStop => {
                machine_control::force_stop(&m.dir).map_err(|e| e.to_string().into())
            }
            Power::Reset => machine_control::reset(&m.dir).map_err(|e| e.to_string().into()),
        }
    }

    fn console_send(&self, id: MachineId, bytes: &[u8]) -> Result<()> {
        use std::io::Write;
        let m = self.machine(id)?;
        let mut w = lock(&m.console.writer);
        match w.as_mut() {
            Some(stream) => stream.write_all(bytes).map_err(|e| e.to_string().into()),
            None => Err("the serial console is not connected".into()),
        }
    }

    fn console_take(&self, id: MachineId, out: &mut Vec<u8>) -> ConsoleLink {
        let Ok(m) = self.machine(id) else {
            return ConsoleLink::Down("no such machine".into());
        };
        out.append(&mut lock(&m.console.inbox));
        let link = lock(&m.console.link).clone();
        link
    }

    fn console_kind(&self, id: MachineId) -> Result<ConsoleKind> {
        let m = self.machine(id)?;
        let text = self.nvram(&m, &["get", "console"])?;
        match text.trim().rsplit('=').next() {
            Some("g") => Ok(ConsoleKind::Graphics),
            Some("d") => Ok(ConsoleKind::Serial),
            other => Err(format!("the PROM's console is {other:?}").into()),
        }
    }

    fn set_console_kind(&self, id: MachineId, kind: ConsoleKind) -> Result<()> {
        let m = self.machine(id)?;
        let value = match kind {
            ConsoleKind::Graphics => "console=g",
            ConsoleKind::Serial => "console=d",
        };
        self.nvram(&m, &["set", value])?;
        Ok(())
    }
}

/// Whether the machine's runner is up: it has written its run record and the
/// process lives. This does not probe the machine lock, which a probe holds
/// for an instant and so can make a start, a stop or `origami status` at the
/// same moment see a machine that is not there. A runner that is still
/// starting has no record yet and counts as not running.
fn runner_alive(dir: &Path) -> bool {
    let Ok(record) = machine_control::read(dir) else {
        return false;
    };
    #[cfg(unix)]
    {
        // SAFETY: signal 0 only checks that the process exists.
        let r = unsafe { libc::kill(record.pid as libc::pid_t, 0) };
        r == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
    #[cfg(not(unix))]
    {
        let _ = record;
        true
    }
}

/// The console thread: keep the console connected, collect what the guest
/// writes, and wake the UI. It reconnects after the machine is started again.
fn console_thread(m: &Arc<Machine>) {
    let mut primed = false;
    while !m.stop.load(Ordering::Acquire) {
        let console = origami::console::Console::connect(&m.dir).and_then(|c| {
            let w = c.try_clone_stream()?;
            Ok((c, w))
        });
        let (mut console, writer) = match console {
            Ok(c) => c,
            Err(e) => {
                let why = if runner_alive(&m.dir) {
                    e.to_string()
                } else {
                    "the machine is not running".to_string()
                };
                *lock(&m.console.link) = ConsoleLink::Down(why);
                m.shared.wake(m.id);
                sleep_unless_stopped(m, Duration::from_secs(1));
                continue;
            }
        };
        if !primed {
            primed = true;
            if let Some(tail) = serial_log_tail(&m.dir.join("logs/serial.log"), CONSOLE_PRIME) {
                lock(&m.console.inbox).extend_from_slice(&tail);
            }
        }
        *lock(&m.console.writer) = Some(writer);
        *lock(&m.console.link) = ConsoleLink::Connected;
        m.shared.wake(m.id);
        let mut buf = [0u8; 8192];
        let why = loop {
            if m.stop.load(Ordering::Acquire) {
                break String::new();
            }
            match console.read_some(&mut buf) {
                Ok(Some(0)) => break "the guest closed the console".to_string(),
                Ok(Some(n)) => {
                    m.console.total.fetch_add(n as u64, Ordering::Relaxed);
                    {
                        let mut inbox = lock(&m.console.inbox);
                        inbox.extend_from_slice(&buf[..n]);
                        let over = inbox.len().saturating_sub(CONSOLE_INBOX);
                        if over > 0 {
                            inbox.drain(..over);
                        }
                    }
                    m.shared.wake(m.id);
                }
                Ok(None) => {}
                Err(e) => break e.to_string(),
            }
        };
        *lock(&m.console.writer) = None;
        *lock(&m.console.link) = ConsoleLink::Down(why);
        m.shared.wake(m.id);
        sleep_unless_stopped(m, Duration::from_secs(1));
    }
}

fn sleep_unless_stopped(m: &Machine, total: Duration) {
    let end = std::time::Instant::now() + total;
    while std::time::Instant::now() < end && !m.stop.load(Ordering::Acquire) {
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The last `len` bytes of a log, starting at a line.
fn serial_log_tail(path: &Path, len: u64) -> Option<Vec<u8>> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).ok()?;
    let size = f.metadata().ok()?.len();
    let from = size.saturating_sub(len);
    f.seek(SeekFrom::Start(from)).ok()?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).ok()?;
    if from > 0 {
        let start = buf.iter().position(|&b| b == b'\n').map_or(0, |p| p + 1);
        buf.drain(..start);
    }
    Some(buf)
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
