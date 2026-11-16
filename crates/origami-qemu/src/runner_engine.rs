//! The QEMU engine a viewer hands to `origami_core::facade::Core`.
//!
//! [`QemuEngine`] starts QEMU as a child of the caller, so
//! a machine would die with the viewer and would run without the runner's
//! private switch and NFS. Until the runner hosts sessions for every front
//! end, this engine delegates everything but `launch` to it and launches the
//! way the viewer always has: it starts (or finds) the product's own runner
//! with `display-shm`, then attaches to it. Pictures come from QEMU's
//! shared-memory display, and input goes back on the same socket. A QEMU
//! that has the `display-shm` device speaks protocol version 4, and an older
//! one version 3.
//!
//! Where the GPU can import memory ([`Importer`]), version 3 asks QEMU to
//! allocate scanout memory the GPU imports and the UI draws from it in
//! place, and version 4 imports the slots QEMU lends; otherwise the changed
//! lines are copied.

use crate::engine::screendump_target;
use crate::{control as qemu_control, runtime, QemuEngine, Runtime, ENGINE};
#[cfg(unix)]
use origami_display::v4::session::SessionError;
#[cfg(unix)]
use origami_display::v4::viewer::{Viewer, ViewerInput};
use origami_display::{Display, Taken};
use origami_engine::status::Facts as DisplayFacts;
use origami_engine::{
    Catalogue, ConsoleInfo, ConsoleIo, ConsoleKind, Control, CursorInfo, DisplayEndpoint, Engine,
    EngineInfo, Facts, Frame, FrameSource, HeadInfo, InputEvent, InputSink, LaunchPlan, MachineId,
    Report, Result, RunState, Session, Stop, Wake, Wiring,
};
use origami_machine::control as machine_control;
use origami_machine::dir::MachineDir;
use origami_machine::model::Machine;
use origami_machine::stores::FirmwareSet;
use origami_machine::Offering;
use qapi::qmp;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

pub use origami_display::{Importer, Stats};

pub struct RunnerEngine {
    inner: QemuEngine,
    /// The `origami` executable that runs machines in the background.
    runner: PathBuf,
    importer: Mutex<Option<Arc<dyn Importer>>>,
    next: AtomicU32,
    /// The latest session of each machine, by directory, for the calls the
    /// facade has no name for: visibility, counters, why there is no picture.
    shown: Mutex<HashMap<PathBuf, Arc<Shown>>>,
}

/// Which protocol a session's pictures arrive by.
enum Backend {
    V3 {
        display: Box<Display>,
        uplink: origami_display::Uplink,
    },
    #[cfg(unix)]
    V4(Arc<Viewer>),
}

/// One attached session's display, input and liveness.
struct Shown {
    id: MachineId,
    dir: PathBuf,
    /// Each graphic head's display device id and head number there.
    heads: Vec<(String, u32)>,
    backend: Backend,
    wake: Wake,
    stop: AtomicBool,
    /// Set once the machine is gone.
    gone: AtomicBool,
    paused: AtomicBool,
    input: Mutex<Option<Sender<InputEvent>>>,
    /// Ends the display thread's blocked read when the session drops.
    interrupt: Mutex<Option<origami_display::Interrupt>>,
}

impl Shown {
    fn set_visible(&self, visible: bool) {
        let wake = match &self.backend {
            Backend::V3 { display, .. } => {
                let was = display.visible();
                display.set_visible(visible);
                visible && !was && display.arm_all()
            }
            #[cfg(unix)]
            Backend::V4(viewer) => viewer.set_visible(visible),
        };
        if wake {
            (self.wake)(self.id);
        }
    }

    fn heads(&self) -> Vec<HeadInfo> {
        match &self.backend {
            Backend::V3 { display, .. } => display.heads(),
            #[cfg(unix)]
            Backend::V4(viewer) => viewer.heads(),
        }
    }

    fn stats(&self) -> Stats {
        match &self.backend {
            Backend::V3 { display, .. } => display.stats(),
            #[cfg(unix)]
            Backend::V4(viewer) => viewer.stats(),
        }
    }

    fn backend_name(&self) -> Option<String> {
        match &self.backend {
            Backend::V3 { display, .. } => display.backend(),
            #[cfg(unix)]
            Backend::V4(viewer) => viewer.backend(),
        }
    }

    fn link_facts(&self) -> origami_engine::status::LinkFacts {
        match &self.backend {
            Backend::V3 { display, .. } => display.link_facts(),
            #[cfg(unix)]
            Backend::V4(viewer) => viewer.link_facts(),
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl RunnerEngine {
    pub fn new(runtime: Runtime, runner: PathBuf) -> RunnerEngine {
        RunnerEngine {
            inner: QemuEngine::new(runtime),
            runner,
            importer: Mutex::new(None),
            next: AtomicU32::new(1),
            shown: Mutex::new(HashMap::new()),
        }
    }

    /// Take regions into the GPU with `importer`, for machines started after
    /// this. Where it imports exported memory, QEMU is told to export on its
    /// adapter.
    pub fn set_importer(&self, importer: Arc<dyn Importer>) {
        *lock(&self.importer) = Some(importer);
    }

    /// The `origami` beside this executable or in the sibling `bin`, or the one
    /// `ORIGAMI_BIN` names.
    pub fn locate_runner() -> Result<PathBuf> {
        if let Some(path) = std::env::var_os("ORIGAMI_BIN") {
            return Ok(path.into());
        }
        let exe = std::env::current_exe()?;
        let found =
            runtime::product_program(exe.parent().ok_or("no executable directory")?, "origami");
        if found.is_file() {
            Ok(found)
        } else {
            Err(format!(
                "no origami beside {} or in its sibling bin; set ORIGAMI_BIN",
                exe.display()
            )
            .into())
        }
    }

    fn shown(&self, dir: &Path) -> Option<Arc<Shown>> {
        lock(&self.shown).get(dir).cloned()
    }

    /// Start the machine in `dir` in the background with the shared-memory
    /// display, exporting its scanout memory on the viewer's GPU where it can.
    fn start_runner(&self, dir: &Path) -> Result<()> {
        let gpu = lock(&self.importer)
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
            eprintln!("origami: this QEMU cannot export GPU memory; copying instead");
            start(None)?;
        }
        Ok(())
    }

    /// Run the product's `nvram` command on `dir`. Another origami, such as a
    /// `get` typed at a shell, can hold the edit lock for an instant; that
    /// looks like a running machine, so try again before believing it.
    fn nvram(&self, dir: &Path, args: &[&str]) -> Result<String> {
        let mut last = String::new();
        for _ in 0..20 {
            let out = std::process::Command::new(&self.runner)
                .arg("nvram")
                .arg(dir)
                .args(args)
                .stdin(std::process::Stdio::null())
                .output()?;
            if out.status.success() {
                return Ok(String::from_utf8_lossy(&out.stdout).into_owned());
            }
            let err = String::from_utf8_lossy(&out.stderr);
            let err = err.trim().trim_start_matches("error: ").to_string();
            if !err.contains("stop the machine before") {
                return Err(err.into());
            }
            last = err;
            if runner_alive(dir) {
                break;
            }
            std::thread::sleep(Duration::from_millis(150));
        }
        Err(last.into())
    }

    /// The console the PROM uses. Readable only while the machine is stopped.
    pub fn console_kind(&self, dir: &Path) -> Result<ConsoleKind> {
        let text = self.nvram(dir, &["get", "console"])?;
        match text.trim().rsplit('=').next() {
            Some("g") => Ok(ConsoleKind::Graphics),
            Some("d") => Ok(ConsoleKind::Serial),
            other => Err(format!("the PROM's console is {other:?}").into()),
        }
    }

    /// Choose the console the PROM uses. Only a stopped machine takes it.
    pub fn set_console_kind(&self, dir: &Path, kind: ConsoleKind) -> Result<()> {
        let value = match kind {
            ConsoleKind::Graphics => "console=g",
            ConsoleKind::Serial => "console=d",
        };
        self.nvram(dir, &["set", value])?;
        Ok(())
    }

    /// Tell the display whether anyone is looking, so it can skip work for a
    /// machine nobody sees. Changes keep accumulating while invisible; the
    /// next frame taken returns them.
    pub fn set_visible(&self, dir: &Path, visible: bool) {
        if let Some(s) = self.shown(dir) {
            s.set_visible(visible);
        }
    }

    pub fn stats(&self, dir: &Path) -> Option<Stats> {
        Some(self.shown(dir)?.stats())
    }

    pub fn is_running(&self, dir: &Path) -> bool {
        self.shown(dir)
            .is_some_and(|s| !s.gone.load(Ordering::Acquire))
    }

    /// How the machine's pictures reach the GPU, in a few words, or None
    /// before it has any.
    pub fn display_backend(&self, dir: &Path) -> Option<String> {
        self.shown(dir)?.backend_name()
    }

    /// Where the machine's display connection stands, for naming why there
    /// is no picture. The runner log's error is not here: the caller reads
    /// that file and adds it.
    pub fn facts(&self, dir: &Path) -> DisplayFacts {
        let Some(s) = self.shown(dir) else {
            return DisplayFacts::default();
        };
        let socket = runtime::display_socket(dir);
        let picture = s
            .heads()
            .into_iter()
            .find(|h| h.active)
            .map(|h| (h.width, h.height));
        DisplayFacts {
            running: runner_alive(dir),
            socket_exists: socket.exists(),
            socket,
            link: s.link_facts(),
            runner_error: None,
            picture,
        }
    }

    /// Lines for a debug display: the protocol's state and counters.
    pub fn debug(&self, dir: &Path) -> Vec<String> {
        let Some(s) = self.shown(dir) else {
            return Vec::new();
        };
        let l = s.link_facts();
        let st = s.stats();
        let mut out = vec![
            format!(
                "protocol: socket {} | connected {} | welcomed {} | regions {} | messages {}",
                if runtime::display_socket(dir).exists() {
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
                st.frames,
                st.lines,
                st.imported_lines,
                st.copied_bytes,
                s.backend_name().unwrap_or_else(|| "none yet".into())
            ),
        ];
        for (i, h) in s.heads().iter().enumerate() {
            out.push(format!(
                "head {i}: {}x{} seq {} {}",
                h.width,
                h.height,
                h.seq,
                if h.active { "active" } else { "no picture" }
            ));
        }
        out
    }
}

impl Engine for RunnerEngine {
    fn info(&self) -> EngineInfo {
        self.inner.info()
    }

    fn catalogue(&self) -> Result<Arc<Catalogue>> {
        self.inner.catalogue()
    }

    fn firmware(
        &self,
        offering: &Offering,
        boot_prom: Option<&Path>,
        io_prom: Option<&Path>,
    ) -> Result<FirmwareSet> {
        self.inner.firmware(offering, boot_prom, io_prom)
    }

    fn validate(&self, m: &Machine, dir: &MachineDir) -> Report {
        self.inner.validate(m, dir)
    }

    fn prepare_stores(&self, o: &Offering, proms: &FirmwareSet, state: &Path) -> Result<()> {
        self.inner.prepare_stores(o, proms, state)
    }

    fn create_disk(&self, stem: &Path, size_mib: u64) -> Result<PathBuf> {
        self.inner.create_disk(stem, size_mib)
    }

    /// The runner plans its own launch, so the plan names the directory and
    /// the display device of each head, which screendump needs.
    fn plan(&self, m: &Machine, dir: &MachineDir, _w: &Wiring) -> Result<LaunchPlan> {
        let catalogue = self.inner.catalogue()?;
        let file = m.to_file();
        let offering = origami_machine::offering(&catalogue, &file.machine)?;
        let heads = runtime::head_devices(&catalogue, offering, &file);
        Ok(LaunchPlan::new(
            ENGINE,
            dir.path().to_path_buf(),
            serde_json::json!({ "heads": heads }),
        ))
    }

    fn launch(&self, plan: LaunchPlan, wake: Wake) -> Result<Arc<dyn Session>> {
        let dir = plan.dir().canonicalize()?;
        let heads = serde_json::from_value(plan.payload()["heads"].clone()).unwrap_or_default();
        if qemu_control::running(&dir)
            .map_err(|e| e.to_string())?
            .is_none()
        {
            self.start_runner(&dir)?;
        }
        let id = MachineId(self.next.fetch_add(1, Ordering::Relaxed));
        let importer = lock(&self.importer).clone();
        // The runner recorded what it started QEMU with; the viewer follows it.
        let v4 = cfg!(unix) && machine_control::read(&dir).is_ok_and(|r| r.display_protocol == 4);
        let (backend, sink): (Backend, Box<dyn InputSink>) = if v4 {
            #[cfg(unix)]
            {
                let viewer = Arc::new(Viewer::new());
                if let Some(importer) = importer {
                    viewer.set_importer(importer);
                }
                let sink = Box::new(ViewerInput(viewer.clone()));
                (Backend::V4(viewer), sink)
            }
            #[cfg(not(unix))]
            unreachable!("version 4 is chosen on unix only")
        } else {
            let display = Display::default();
            if let Some(importer) = importer {
                display.set_importer(importer);
            }
            let uplink: origami_display::Uplink = Arc::new(Mutex::new(None));
            let sink = Box::new(origami_display::SocketInput::new(uplink.clone()));
            (
                Backend::V3 {
                    display: Box::new(display),
                    uplink,
                },
                sink,
            )
        };
        let (tx, rx) = mpsc::channel();
        let shown = Arc::new(Shown {
            id,
            dir: dir.clone(),
            heads,
            backend,
            wake,
            stop: AtomicBool::new(false),
            gone: AtomicBool::new(false),
            paused: AtomicBool::new(false),
            input: Mutex::new(Some(tx)),
            interrupt: Mutex::new(None),
        });
        let input = std::thread::Builder::new()
            .name("display-input".into())
            .spawn(move || input_worker(sink, rx))?;
        let reader = {
            let shown = shown.clone();
            std::thread::Builder::new()
                .name("shm-display".into())
                .spawn(move || reader_thread(&shown))?
        };
        lock(&self.shown).insert(dir, shown.clone());
        Ok(Arc::new(RunnerSession {
            shown,
            threads: Mutex::new(vec![input, reader]),
        }))
    }
}

struct RunnerSession {
    shown: Arc<Shown>,
    threads: Mutex<Vec<std::thread::JoinHandle<()>>>,
}

impl Drop for RunnerSession {
    fn drop(&mut self) {
        self.shown.stop.store(true, Ordering::Release);
        if let Some(interrupt) = lock(&self.shown.interrupt).as_ref() {
            interrupt();
        }
        *lock(&self.shown.input) = None;
        for t in lock(&self.threads).drain(..) {
            let _ = t.join();
        }
    }
}

impl RunnerSession {
    /// QMP is single-client, so each request connects and disconnects.
    fn qmp(&self) -> Result<qemu_control::Qmp> {
        if self.shown.gone.load(Ordering::Acquire) {
            return Err("the machine has exited".into());
        }
        qemu_control::verified_qmp(&machine_control::read(&self.shown.dir)?)
    }
}

impl Session for RunnerSession {
    fn id(&self) -> MachineId {
        self.shown.id
    }

    fn display(&self) -> DisplayEndpoint {
        DisplayEndpoint::Frames(self.shown.clone())
    }

    fn consoles(&self) -> Vec<ConsoleInfo> {
        vec![ConsoleInfo {
            name: "console".into(),
            primary: true,
        }]
    }

    fn open_console(&self, _name: &str) -> Result<Box<dyn ConsoleIo>> {
        let dir = &self.shown.dir;
        let record = machine_control::read(dir)?;
        let stream = std::net::TcpStream::connect(("127.0.0.1", record.console_port))?;
        // The console socket carries only what is written while someone is
        // connected, so a login prompt printed earlier would never show.
        let prime =
            serial_log_tail(&dir.join("logs/serial.log"), CONSOLE_PRIME).unwrap_or_default();
        Ok(Box::new(PrimedConsole {
            prime,
            at: 0,
            stream,
        }))
    }

    fn control(&self, c: Control) -> Result<()> {
        let mut qmp = self.qmp()?;
        match c {
            Control::Reset => qmp.execute(&qmp::system_reset {}).map(drop)?,
            Control::PowerButton => qmp.execute(&qmp::system_powerdown {}).map(drop)?,
            Control::Nmi => qmp.execute(&qmp::inject_nmi {}).map(drop)?,
            Control::Pause => {
                qmp.execute(&qmp::stop {})?;
                self.shown.paused.store(true, Ordering::Release);
            }
            Control::Resume => {
                qmp.execute(&qmp::cont {})?;
                self.shown.paused.store(false, Ordering::Release);
            }
        }
        Ok(())
    }

    fn send_input(&self, events: &[InputEvent]) -> Result<()> {
        let tx = lock(&self.shown.input);
        let tx = tx.as_ref().ok_or("the machine is stopping")?;
        for &event in events {
            tx.send(event).map_err(|_| "the input channel is closed")?;
        }
        Ok(())
    }

    fn media(&self, _drive: &str, _image: Option<&Path>) -> Result<()> {
        Err("the viewer does not change media".into())
    }

    fn screendump(&self, head: usize, path: &Path) -> Result<()> {
        let (device, head) = screendump_target(&self.shown.heads, head)?;
        self.qmp()?.execute(&qmp::screendump {
            filename: path.display().to_string(),
            device,
            head,
            format: None,
        })?;
        Ok(())
    }

    fn stop(&self, how: Stop) -> Result<()> {
        let dir = &self.shown.dir;
        match how {
            Stop::Clean { .. } => qemu_control::stop(dir),
            Stop::Force => qemu_control::force_stop(dir),
        }
        .map_err(|e| e.to_string())?;
        self.shown.gone.store(true, Ordering::Release);
        (self.shown.wake)(self.shown.id);
        Ok(())
    }

    fn status(&self) -> Facts {
        let state = if self.shown.gone.load(Ordering::Acquire) {
            RunState::Exited {
                code: None,
                last_error: None,
            }
        } else if self.shown.paused.load(Ordering::Acquire) {
            RunState::Paused
        } else {
            RunState::Running
        };
        Facts { state, pid: None }
    }
}

impl FrameSource for Shown {
    fn heads(&self) -> Vec<HeadInfo> {
        Shown::heads(self)
    }

    fn take_frame(&self, head: usize, pixels: &mut Vec<u8>) -> Option<Frame> {
        let display = match &self.backend {
            Backend::V3 { display, .. } => display,
            // A version 4 frame needs no re-arming: the next one is already asked for.
            #[cfg(unix)]
            Backend::V4(viewer) => {
                return match viewer.take(head, pixels) {
                    Taken::Frame(update) => Some(Frame::Rows(update)),
                    Taken::Imported(imported) => Some(Frame::Imported(imported)),
                    _ => None,
                };
            }
        };
        let taken = match display.take(head, pixels) {
            Ok(taken) => taken,
            Err(e) => {
                eprintln!("origami: {}: {e}", self.dir.display());
                return None;
            }
        };
        match taken {
            Taken::Retry => {
                // Ask again from the UI thread rather than spin here.
                (self.wake)(self.id);
                return None;
            }
            // Armed again for the next change, then check it did not
            // already happen.
            _ if display.visible() && display.arm(head) => (self.wake)(self.id),
            _ => {}
        }
        match taken {
            Taken::Frame(update) => Some(Frame::Rows(update)),
            Taken::Imported(imported) => Some(Frame::Imported(imported)),
            _ => None,
        }
    }

    fn cursor(&self, head: usize, known_gen: u32) -> Option<CursorInfo> {
        match &self.backend {
            Backend::V3 { display, .. } => display.cursor(head, known_gen),
            #[cfg(unix)]
            Backend::V4(viewer) => viewer.cursor(head, known_gen),
        }
    }

    fn set_visible(&self, visible: bool) {
        Shown::set_visible(self, visible);
    }

    fn stats(&self) -> Option<origami_engine::FrameStats> {
        let s = Shown::stats(self);
        Some(origami_engine::FrameStats {
            frames: s.frames,
            lines: s.lines,
            imported_lines: s.imported_lines,
            copied_bytes: s.copied_bytes,
        })
    }
}

/// How much of `serial.log` primes a console that connects to a guest that
/// has been talking.
const CONSOLE_PRIME: u64 = 16 << 10;

/// The console socket, preceded by what the serial log already holds.
struct PrimedConsole {
    prime: Vec<u8>,
    at: usize,
    stream: std::net::TcpStream,
}

impl Read for PrimedConsole {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.at < self.prime.len() {
            let n = buf.len().min(self.prime.len() - self.at);
            buf[..n].copy_from_slice(&self.prime[self.at..self.at + n]);
            self.at += n;
            return Ok(n);
        }
        self.stream.read(buf)
    }
}

impl Write for PrimedConsole {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.stream.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.stream.flush()
    }
}

impl ConsoleIo for PrimedConsole {
    fn split(self: Box<Self>) -> std::io::Result<(Box<dyn Read + Send>, Box<dyn Write + Send>)> {
        let writer = self.stream.try_clone()?;
        Ok((Box::new(*self), Box::new(writer)))
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

/// The last `len` bytes of a log, starting at a line.
fn serial_log_tail(path: &Path, len: u64) -> Option<Vec<u8>> {
    use std::io::{Seek, SeekFrom};
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

/// The real link: the socket the product's runtime gave QEMU, the machine's
/// liveness from the product's own QMP check.
struct SocketLink {
    /// Where QEMU listens, as the runtime told it to (`display-shm-socket`).
    socket: PathBuf,
    dir: PathBuf,
    shown: Arc<Shown>,
}

impl origami_display::Link for SocketLink {
    fn connect(&mut self) -> std::io::Result<origami_display::Conn> {
        let (conn, interrupt) = origami_display::connect_socket_blocking(&self.socket)?;
        *lock(&self.shown.interrupt) = Some(interrupt);
        // A drop that came before the interrupt was stored would wait forever.
        if self.shown.stop.load(Ordering::Acquire) {
            if let Some(interrupt) = lock(&self.shown.interrupt).as_ref() {
                interrupt();
            }
        }
        Ok(conn)
    }

    fn alive(&mut self) -> bool {
        qemu_control::running(&self.dir).is_ok_and(|r| r.is_some())
    }
}

/// The display thread: keep the connection, wake the UI when something moves,
/// and mark the machine gone when it ends.
fn reader_thread(m: &Arc<Shown>) {
    let (display, uplink) = match &m.backend {
        Backend::V3 { display, uplink } => (display, uplink),
        #[cfg(unix)]
        Backend::V4(viewer) => return reader_thread_v4(m, viewer),
    };
    let mut link = SocketLink {
        socket: runtime::display_socket(&m.dir),
        dir: m.dir.clone(),
        shown: m.clone(),
    };
    let wake = || {
        if display.visible() {
            (m.wake)(m.id);
        }
    };
    loop {
        let exit = origami_display::run(
            &mut link,
            display,
            uplink,
            &m.stop,
            origami_display::Timing::default(),
            &wake,
        );
        if exit == origami_display::Exit::Stopped {
            return;
        }
        if exit == origami_display::Exit::NeverCame {
            eprintln!(
                "origami: no shared-memory display from {} after 300 s",
                m.dir.display()
            );
        }
        if !origami_display::Link::alive(&mut link) {
            m.gone.store(true, Ordering::Release);
            // The UI shows the state even for a machine nobody is looking at.
            (m.wake)(m.id);
            return;
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

/// The version 4 display thread: connect, serve the session until it ends,
/// and mark the machine gone when it does. The runner holds the listening
/// socket, so a connection waits for QEMU to accept it.
#[cfg(unix)]
fn reader_thread_v4(m: &Arc<Shown>, viewer: &Arc<Viewer>) {
    let socket = runtime::display_socket(&m.dir);
    let started = std::time::Instant::now();
    let (mut ever, mut misses) = (false, 0u32);
    let wake = || {
        if viewer.visible() {
            (m.wake)(m.id);
        }
    };
    while !m.stop.load(Ordering::Acquire) {
        match std::os::unix::net::UnixStream::connect(&socket) {
            Ok(stream) => {
                misses = 0;
                ever = true;
                if let Ok(end) = stream.try_clone() {
                    let interrupt: origami_display::Interrupt =
                        Box::new(move || drop(end.shutdown(std::net::Shutdown::Both)));
                    *lock(&m.interrupt) = Some(interrupt);
                }
                // A drop that came before the interrupt was stored would wait forever.
                if m.stop.load(Ordering::Acquire) {
                    return;
                }
                if let Err(e) = viewer.run(stream, &wake) {
                    if m.stop.load(Ordering::Acquire) {
                        return;
                    }
                    eprintln!("origami: display: {e}");
                    // A producer that breaks the protocol will do it again.
                    if matches!(e, SessionError::Protocol(_) | SessionError::Producer(_)) {
                        return;
                    }
                }
            }
            Err(e) => {
                misses += 1;
                viewer.connect_failed(e.to_string());
            }
        }
        if m.stop.load(Ordering::Acquire) {
            return;
        }
        // Asking takes the QMP port other clients want, so ask now and then.
        if (misses == 0 || misses % 10 == 1)
            && !qemu_control::running(&m.dir).is_ok_and(|r| r.is_some())
        {
            m.gone.store(true, Ordering::Release);
            (m.wake)(m.id);
            return;
        }
        if !ever && started.elapsed() > Duration::from_secs(300) {
            eprintln!(
                "origami: no shared-memory display from {} after 300 s",
                m.dir.display()
            );
            return;
        }
        std::thread::sleep(Duration::from_millis(if misses > 0 { 200 } else { 1000 }));
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
            eprintln!("origami: input: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{TcpListener, TcpStream};

    #[test]
    fn the_console_reads_the_serial_log_before_the_socket() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let stream = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (mut peer, _) = listener.accept().unwrap();
        peer.write_all(b"live").unwrap();
        let console = Box::new(PrimedConsole {
            prime: b"login: ".to_vec(),
            at: 0,
            stream,
        });
        let (mut reader, mut writer) = console.split().unwrap();
        let mut got = [0u8; 11];
        reader.read_exact(&mut got).unwrap();
        assert_eq!(&got, b"login: live");
        writer.write_all(b"root").unwrap();
        let mut sent = [0u8; 4];
        peer.read_exact(&mut sent).unwrap();
        assert_eq!(&sent, b"root");
    }

    #[test]
    fn the_log_tail_starts_at_a_line() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("serial.log");
        std::fs::write(&log, "one\ntwo\nthree\n").unwrap();
        assert_eq!(serial_log_tail(&log, 8).unwrap(), b"three\n");
        assert_eq!(serial_log_tail(&log, 100).unwrap(), b"one\ntwo\nthree\n");
    }
}
