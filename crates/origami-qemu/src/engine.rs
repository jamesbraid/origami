//! [`QemuEngine`]: QEMU behind the product's engine interface.

use crate::control::{verified_qmp, Qmp};
use crate::runtime::{self, Display, GpuUuid, Runtime};
use crate::ENGINE;
use origami_engine::{
    Capabilities, Catalogue, ConsoleInfo, ConsoleIo, ConsoleTransport, Control, DiskFormat,
    DisplayEndpoint, DisplayKind, DisplayRequest, DriveFormats, DriveKind, Engine, EngineInfo,
    Facts, IdentityKind, LaunchPlan, MachineId, Polled, Report, Result, RunState, Session, Stop,
    Wake, Wiring,
};
use origami_machine::control::Record;
use origami_machine::dir::MachineDir;
use origami_machine::model::Machine;
use origami_machine::stores::{FirmwareSet, StorePreparer};
use origami_machine::Offering;
use qapi::qmp;
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader};
use std::net::{Ipv4Addr, SocketAddrV4, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// The only console name a QEMU session has: the primary serial line.
pub const CONSOLE: &str = "console";

/// Display v3 carries up to this many heads.
const MAX_HEADS: u32 = 8;

/// How often the monitor looks at the QEMU process.
const POLL: Duration = Duration::from_millis(25);

pub struct QemuEngine {
    rt: Runtime,
    catalogue: Mutex<Option<Arc<Catalogue>>>,
    next_id: AtomicU32,
}

impl QemuEngine {
    /// An engine that asks the runtime's QEMU for its machine list when
    /// first needed.
    pub fn new(rt: Runtime) -> Self {
        Self {
            rt,
            catalogue: Mutex::new(None),
            next_id: AtomicU32::new(1),
        }
    }

    /// An engine with a machine list the caller already has.
    pub fn with_catalogue(rt: Runtime, catalogue: Arc<Catalogue>) -> Self {
        let engine = Self::new(rt);
        *engine.catalogue.lock().unwrap() = Some(catalogue);
        engine
    }

    pub fn runtime(&self) -> &Runtime {
        &self.rt
    }
}

/// What a QEMU launch needs, as the plan's payload.
#[derive(Debug, Serialize, Deserialize)]
struct QemuPlan {
    program: PathBuf,
    args: Vec<String>,
    keymap: PathBuf,
    qmp_port: u16,
    console_port: u16,
    name: String,
    /// The display v3 socket, when the plan has one.
    display: Option<PathBuf>,
    /// The drives that take media: guest name and QEMU block device id.
    removable: Vec<(String, String)>,
    /// Each graphic head's display device id and head number there.
    heads: Vec<(String, u32)>,
}

fn display_of(request: DisplayRequest) -> Display {
    match request {
        DisplayRequest::Window => Display::Local,
        DisplayRequest::Vnc { port } => Display::Vnc { port },
        DisplayRequest::Shared { gpu } => Display::Shm {
            gpu: gpu.map(GpuUuid),
        },
        DisplayRequest::None => Display::None,
    }
}

impl Engine for QemuEngine {
    fn info(&self) -> EngineInfo {
        let formats = |kind, formats: &[DiskFormat], preferred| DriveFormats {
            kind,
            formats: formats.to_vec(),
            preferred,
        };
        EngineInfo {
            id: ENGINE.into(),
            version: None,
            capabilities: Capabilities {
                display: DisplayKind::Socket,
                max_heads: MAX_HEADS,
                gpu_regions: true,
                controls: vec![
                    Control::Reset,
                    Control::PowerButton,
                    Control::Nmi,
                    Control::Pause,
                    Control::Resume,
                ],
                disk_formats: vec![
                    formats(
                        DriveKind::Disk,
                        &[DiskFormat::Raw, DiskFormat::Qcow2, DiskFormat::Chd],
                        DiskFormat::Qcow2,
                    ),
                    formats(DriveKind::Cdrom, &[DiskFormat::Raw], DiskFormat::Raw),
                    formats(DriveKind::Tape, &[DiskFormat::Raw], DiskFormat::Raw),
                ],
                console: ConsoleTransport::RawTcp,
                identity: IdentityKind::Qemu,
                attach: false,
                audio: vec!["octane".into(), "octane2".into(), "o2".into()],
            },
        }
    }

    fn catalogue(&self) -> Result<Arc<Catalogue>> {
        let mut cached = self.catalogue.lock().unwrap();
        if let Some(catalogue) = &*cached {
            return Ok(catalogue.clone());
        }
        let catalogue = Arc::new(crate::machines(&self.rt)?);
        *cached = Some(catalogue.clone());
        Ok(catalogue)
    }

    fn firmware(
        &self,
        offering: &Offering,
        boot_prom: Option<&Path>,
        io_prom: Option<&Path>,
    ) -> Result<FirmwareSet> {
        let (boot_prom, io_prom) = self.rt.firmware(offering, boot_prom, io_prom)?;
        Ok(FirmwareSet { boot_prom, io_prom })
    }

    fn validate(&self, m: &Machine, dir: &MachineDir) -> Report {
        let catalogue = match self.catalogue() {
            Ok(catalogue) => catalogue,
            Err(error) => return Report::error(error.to_string()),
        };
        match crate::validate(&catalogue, dir.path(), &m.to_file()) {
            Ok(_) => Report::default(),
            Err(error) => Report::error(error.to_string()),
        }
    }

    fn preflight_create(&self) -> Result<()> {
        let tool = self.rt.machine_init();
        if !tool.is_file() {
            return Err(format!("packaged machine init tool missing: {}", tool.display()).into());
        }
        Ok(())
    }

    fn check_options(&self, m: &Machine) -> Result<()> {
        runtime::check_extra_args(&m.engine.extra_args())
    }

    fn prepare_stores(&self, o: &Offering, proms: &FirmwareSet, state: &Path) -> Result<()> {
        let staged = self.rt.stage_boot_prom(o, &proms.boot_prom)?;
        let boot_prom = staged.as_ref().map_or(proms.boot_prom.as_path(), |p| p);
        self.rt
            .create_stores(o, boot_prom, proms.io_prom.as_deref(), state)
    }

    fn create_disk(&self, stem: &Path, size_mib: u64) -> Result<PathBuf> {
        let mut name = stem.as_os_str().to_owned();
        name.push(".");
        name.push(DiskFormat::Qcow2.extension());
        let path = PathBuf::from(name);
        if path.exists() {
            return Err(format!("disk exists: {}", path.display()).into());
        }
        let output = Command::new(self.rt.qemu_img())
            .args(["create", "-f", "qcow2"])
            .arg(&path)
            .arg(format!("{size_mib}M"))
            .output()?;
        if !output.status.success() {
            return Err(format!("qemu-img: {}", String::from_utf8_lossy(&output.stderr)).into());
        }
        Ok(path)
    }

    fn plan(&self, m: &Machine, dir: &MachineDir, w: &Wiring) -> Result<LaunchPlan> {
        let catalogue = self.catalogue()?;
        let file = m.to_file();
        let offering = origami_machine::offering(&catalogue, &file.machine)?;
        let display = display_of(w.display);
        let mut args = runtime::arguments_with(
            &catalogue,
            dir.path(),
            &file,
            offering,
            display,
            w.shares.as_ref(),
        )?;
        args.splice(0..0, self.rt.data_arguments());
        // A plan touches no program, so it cannot ask QEMU for a backend.
        args.extend(["-audio".into(), "none".into()]);
        runtime::background_arguments(
            &mut args,
            &file,
            &w.once,
            w.console_port,
            w.control_port,
            &w.name,
            w.paused,
        )?;
        let removable = file
            .drive
            .iter()
            .enumerate()
            .filter(|(_, drive)| drive.kind == "cdrom")
            .map(|(index, drive)| (drive.name.clone(), format!("drive{index}")))
            .collect();
        let plan = QemuPlan {
            program: self.rt.qemu(),
            args,
            keymap: self.rt.data().join("keymaps/en-us"),
            qmp_port: w.control_port,
            console_port: w.console_port,
            name: w.name.clone(),
            display: matches!(display, Display::Shm { .. })
                .then(|| runtime::display_socket(dir.path())),
            removable,
            heads: runtime::head_devices(&catalogue, offering, &file),
        };
        Ok(LaunchPlan::new(
            ENGINE,
            dir.path().to_path_buf(),
            serde_json::to_value(plan)?,
        ))
    }

    fn launch(&self, plan: LaunchPlan, wake: Wake) -> Result<Arc<dyn Session>> {
        if plan.engine() != ENGINE {
            return Err(format!("a {} plan is not a QEMU plan", plan.engine()).into());
        }
        let spec: QemuPlan = serde_json::from_value(plan.payload().clone())?;
        if !spec.program.is_file() {
            return Err(format!("packaged QEMU missing: {}", spec.program.display()).into());
        }
        if !spec.keymap.is_file() {
            return Err(format!("packaged QEMU keymap missing: {}", spec.keymap.display()).into());
        }
        std::fs::create_dir_all(plan.dir().join("logs"))?;
        let mut child = Command::new(&spec.program)
            .args(&spec.args)
            .current_dir(plan.dir())
            .stdin(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()?;
        let last_error = Arc::new(Mutex::new(None));
        let pump = child.stderr.take().map(|stderr| {
            let last_error = last_error.clone();
            std::thread::spawn(move || {
                for line in BufReader::new(stderr).lines().map_while(|line| line.ok()) {
                    eprintln!("{line}");
                    if !line.trim().is_empty() {
                        *last_error.lock().unwrap() = Some(line);
                    }
                }
            })
        });
        let record = Record {
            pid: std::process::id(),
            qmp_port: spec.qmp_port,
            console_port: spec.console_port,
            name: spec.name.clone(),
            qemu_pid: child.id(),
            ..Default::default()
        };
        let deadline = Instant::now() + runtime::qmp_ready_timeout();
        loop {
            if verified_qmp(&record).is_ok() {
                break;
            }
            if let Some(status) = child.try_wait()? {
                if let Some(pump) = pump {
                    let _ = pump.join();
                }
                let detail = last_error
                    .lock()
                    .unwrap()
                    .as_deref()
                    .map_or(String::new(), |line| format!(": {line}"));
                return Err(format!("QEMU exited before QMP was ready ({status}){detail}").into());
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                return Err("QEMU did not open its QMP control endpoint".into());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        Ok(self.start_session(
            child,
            pump,
            last_error,
            record,
            spec.display,
            spec.removable,
            spec.heads,
            wake,
            None,
        ))
    }
}

impl QemuEngine {
    /// A session over a QEMU the runner started with [`runtime::launch`].
    /// Its console is the private chardev the runner alone connects to.
    pub fn adopt(&self, launched: &mut runtime::Launched, wake: Wake) -> Arc<dyn Session> {
        let record = Record {
            pid: std::process::id(),
            qmp_port: launched.qmp_port,
            console_port: launched.engine_console,
            name: launched.name.clone(),
            qemu_pid: launched.pid(),
            ..Default::default()
        };
        self.start_session(
            launched.take_child(),
            None,
            Arc::new(Mutex::new(None)),
            record,
            launched.display.clone(),
            launched.removable.clone(),
            launched.heads.clone(),
            wake,
            launched.take_display_guard(),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn start_session(
        &self,
        child: Child,
        pump: Option<std::thread::JoinHandle<()>>,
        last_error: Arc<Mutex<Option<String>>>,
        record: Record,
        display: Option<PathBuf>,
        removable: Vec<(String, String)>,
        heads: Vec<(String, u32)>,
        wake: Wake,
        guard: Option<Box<dyn std::any::Any + Send>>,
    ) -> Arc<dyn Session> {
        let id = MachineId(self.next_id.fetch_add(1, Ordering::Relaxed));
        let inner = Arc::new(Inner {
            child: Mutex::new(child),
            exit: Mutex::new(None),
            exited: Condvar::new(),
            // Asked once here, off any UI thread, so the first status is true.
            polled: Polled::new(query_state(&record).unwrap_or(RunState::Running)),
            gate: Mutex::new(()),
        });
        let poller = (inner.clone(), record.clone(), wake.clone());
        std::thread::spawn(move || poll_state(&poller.0, &poller.1, &poller.2, id));
        let session = Arc::new(QemuSession {
            id,
            inner: inner.clone(),
            record,
            display,
            removable,
            heads,
            console_taken: AtomicBool::new(false),
        });
        std::thread::spawn(move || {
            monitor(&inner, pump, &last_error, &wake, id);
            // Nothing may connect to a machine that has exited.
            drop(guard);
        });
        session
    }
}

struct Exit {
    code: Option<i32>,
    signal: Option<i32>,
    last_error: Option<String>,
}

struct Inner {
    child: Mutex<Child>,
    exit: Mutex<Option<Exit>>,
    exited: Condvar,
    /// QEMU's run state, kept by [`poll_state`] so `status` never waits on QMP.
    polled: Polled,
    /// QMP takes one client at a time: a command holds this, and the poller
    /// skips a round rather than take the connection from it.
    gate: Mutex<()>,
}

/// QMP with the gate held, so the poller stays off the single connection.
struct Leased<'a> {
    qmp: Qmp,
    _gate: std::sync::MutexGuard<'a, ()>,
}

impl std::ops::Deref for Leased<'_> {
    type Target = Qmp;
    fn deref(&self) -> &Qmp {
        &self.qmp
    }
}

impl std::ops::DerefMut for Leased<'_> {
    fn deref_mut(&mut self) -> &mut Qmp {
        &mut self.qmp
    }
}

/// How often the run state is asked of QMP, and the longest back-off after a
/// failed connection.
const STATE_POLL: Duration = Duration::from_secs(1);
const STATE_BACKOFF: Duration = Duration::from_secs(8);

/// What QMP says the run state is.
fn query_state(record: &Record) -> Result<RunState> {
    let info = verified_qmp(record)?.execute(&qmp::query_status {})?;
    Ok(if matches!(info.status, qmp::RunState::prelaunch) {
        RunState::Prelaunch
    } else if info.running {
        RunState::Running
    } else {
        RunState::Paused
    })
}

/// Keep `inner.polled` current until QEMU exits, only while someone reads
/// it, backing off when QMP will not connect. A hung QMP stalls this thread.
fn poll_state(inner: &Inner, record: &Record, wake: &Wake, id: MachineId) {
    let mut wait = STATE_POLL;
    loop {
        if inner.polled.wanted() {
            if let Ok(_gate) = inner.gate.try_lock() {
                let generation = inner.polled.begin();
                match query_state(record) {
                    Ok(state) => {
                        wait = STATE_POLL;
                        if inner.polled.finish(generation, state) {
                            wake(id);
                        }
                    }
                    Err(_) => wait = (wait * 2).min(STATE_BACKOFF),
                }
            }
        }
        let exit = inner.exit.lock().unwrap();
        if exit.is_some() || inner.exited.wait_timeout(exit, wait).unwrap().0.is_some() {
            return;
        }
    }
}

/// Watch the QEMU process until it exits, record how, and say so.
fn monitor(
    inner: &Inner,
    pump: Option<std::thread::JoinHandle<()>>,
    last_error: &Mutex<Option<String>>,
    wake: &Wake,
    id: MachineId,
) {
    let status = loop {
        match inner.child.lock().unwrap().try_wait() {
            Ok(None) => {}
            Ok(Some(status)) => break Some(status),
            Err(_) => break None,
        }
        std::thread::sleep(POLL);
    };
    // The error output ends when the process does; join it for the last line.
    if let Some(pump) = pump {
        let _ = pump.join();
    }
    let success = status.is_some_and(|status| status.success());
    let exit = Exit {
        code: status.and_then(|status| status.code()),
        signal: status.and_then(signal_of),
        last_error: last_error.lock().unwrap().take().filter(|_| !success),
    };
    // The runner's stderr is runner.log: say why QEMU ended, not only what it printed.
    eprintln!("origami: {}", exit_text(&exit));
    *inner.exit.lock().unwrap() = Some(exit);
    inner.exited.notify_all();
    wake(id);
}

#[cfg(unix)]
fn signal_of(status: std::process::ExitStatus) -> Option<i32> {
    std::os::unix::process::ExitStatusExt::signal(&status)
}

#[cfg(not(unix))]
fn signal_of(_: std::process::ExitStatus) -> Option<i32> {
    None
}

fn exit_text(exit: &Exit) -> String {
    let how = match (exit.code, exit.signal) {
        (Some(0), _) => "QEMU exited cleanly".to_string(),
        (Some(code), _) => format!("QEMU exited with code {code}"),
        (None, Some(signal)) => format!("QEMU was ended by signal {signal}"),
        (None, None) => "QEMU ended and the system gave no status".to_string(),
    };
    match &exit.last_error {
        Some(error) => format!("{how}; its last error: {error}"),
        None => how,
    }
}

/// QMP takes `head` only with `device`; a machine with no named display
/// device has just its primary console.
pub(crate) fn screendump_target(
    heads: &[(String, u32)],
    head: usize,
) -> Result<(Option<String>, Option<i64>)> {
    match heads.get(head) {
        Some((device, head)) => Ok((Some(device.clone()), Some(i64::from(*head)))),
        None if heads.is_empty() && head == 0 => Ok((None, None)),
        None => Err(format!("head {head} does not exist").into()),
    }
}

struct QemuSession {
    id: MachineId,
    inner: Arc<Inner>,
    record: Record,
    display: Option<PathBuf>,
    removable: Vec<(String, String)>,
    heads: Vec<(String, u32)>,
    console_taken: AtomicBool,
}

impl QemuSession {
    fn exited(&self) -> bool {
        self.inner.exit.lock().unwrap().is_some()
    }

    /// QMP is single-client, so each request connects and disconnects.
    fn qmp(&self) -> Result<Leased<'_>> {
        if self.exited() {
            return Err("the machine has exited".into());
        }
        let gate = self.inner.gate.lock().unwrap_or_else(|e| e.into_inner());
        Ok(Leased {
            qmp: verified_qmp(&self.record)?,
            _gate: gate,
        })
    }

    /// Wait for the exit up to `timeout`; true when it happened.
    fn wait_exit(&self, timeout: Option<Duration>) -> bool {
        let deadline = timeout.map(|timeout| Instant::now() + timeout);
        let mut exit = self.inner.exit.lock().unwrap();
        while exit.is_none() {
            let wait = match deadline {
                None => Duration::from_secs(3600),
                Some(deadline) => match deadline.checked_duration_since(Instant::now()) {
                    Some(left) => left,
                    None => return false,
                },
            };
            exit = self.inner.exited.wait_timeout(exit, wait).unwrap().0;
        }
        true
    }
}

impl Session for QemuSession {
    fn id(&self) -> MachineId {
        self.id
    }

    fn display(&self) -> DisplayEndpoint {
        match &self.display {
            Some(socket) => DisplayEndpoint::Socket(socket.clone()),
            None => DisplayEndpoint::None,
        }
    }

    fn consoles(&self) -> Vec<ConsoleInfo> {
        vec![ConsoleInfo {
            name: CONSOLE.into(),
            primary: true,
        }]
    }

    fn open_console(&self, name: &str) -> Result<Box<dyn ConsoleIo>> {
        if name != CONSOLE {
            return Err(format!("no console named {name}").into());
        }
        if self.console_taken.swap(true, Ordering::SeqCst) {
            return Err("the console is already open".into());
        }
        let address = SocketAddrV4::new(Ipv4Addr::LOCALHOST, self.record.console_port);
        match TcpStream::connect_timeout(&address.into(), Duration::from_secs(5)) {
            Ok(stream) => Ok(Box::new(stream)),
            Err(error) => {
                self.console_taken.store(false, Ordering::SeqCst);
                Err(error.into())
            }
        }
    }

    fn control(&self, c: Control) -> Result<()> {
        let mut qmp = self.qmp()?;
        match c {
            Control::Reset => qmp.execute(&qmp::system_reset {})?,
            Control::PowerButton => qmp.execute(&qmp::system_powerdown {})?,
            Control::Nmi => qmp.execute(&qmp::inject_nmi {})?,
            Control::Pause => qmp.execute(&qmp::stop {})?,
            Control::Resume => qmp.execute(&qmp::cont {})?,
        };
        // The poller would say so within a second; callers expect it now.
        match c {
            Control::Pause => self.inner.polled.set_now(RunState::Paused),
            Control::Resume => self.inner.polled.set_now(RunState::Running),
            _ => {}
        }
        Ok(())
    }

    // The drives have block names (`drive0`) and no qdev ids, so `device`.
    #[allow(deprecated)]
    fn media(&self, drive: &str, image: Option<&Path>) -> Result<()> {
        let Some((_, device)) = self.removable.iter().find(|(name, _)| name == drive) else {
            return Err(format!("{drive} is not a removable drive of this machine").into());
        };
        let mut qmp = self.qmp()?;
        match image {
            Some(image) => qmp.execute(&qmp::blockdev_change_medium {
                device: Some(device.clone()),
                id: None,
                filename: image.display().to_string(),
                format: None,
                force: None,
                read_only_mode: None,
            })?,
            None => qmp.execute(&qmp::eject {
                device: Some(device.clone()),
                id: None,
                force: None,
            })?,
        };
        Ok(())
    }

    fn screendump(&self, head: usize, path: &Path) -> Result<()> {
        let (device, head) = screendump_target(&self.heads, head)?;
        self.qmp()?.execute(&qmp::screendump {
            filename: path.display().to_string(),
            device,
            head,
            format: None,
        })?;
        Ok(())
    }

    fn stop(&self, how: Stop) -> Result<()> {
        match how {
            Stop::Clean { timeout } => {
                // QEMU may already have taken a quit and stopped answering
                // while it folds a CHD disk; then only the wait is left.
                if let Ok(mut qmp) = self.qmp() {
                    let _ = qmp.write_command(&qmp::quit {});
                }
                if self.wait_exit(timeout) {
                    Ok(())
                } else {
                    Err("QEMU is still exiting; it was not killed".into())
                }
            }
            Stop::Force => {
                let _ = self.inner.child.lock().unwrap().kill();
                if self.wait_exit(Some(Duration::from_secs(30))) {
                    Ok(())
                } else {
                    Err("QEMU was killed but has not exited after 30 seconds".into())
                }
            }
        }
    }

    fn status(&self) -> Facts {
        if let Some(exit) = &*self.inner.exit.lock().unwrap() {
            return Facts {
                state: RunState::Exited {
                    code: exit.code,
                    signal: exit.signal,
                    last_error: exit.last_error.clone(),
                },
                pid: None,
            };
        }
        Facts {
            state: self.inner.polled.get(),
            pid: Some(self.record.qemu_pid),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn a_head_is_dumped_with_its_display_device() {
        let heads = vec![("io4".to_string(), 0), ("io4".to_string(), 1)];
        assert_eq!(
            screendump_target(&heads, 1).unwrap(),
            (Some("io4".into()), Some(1))
        );
        assert!(screendump_target(&heads, 2).is_err());
        // No named device: only the primary console, and no head to give.
        assert_eq!(screendump_target(&[], 0).unwrap(), (None, None));
        assert!(screendump_target(&[], 1).is_err());
    }

    /// A UI thread asks for the status every frame, so a QMP that accepts and
    /// never answers must not hold it up.
    #[cfg(unix)]
    #[test]
    fn status_does_not_wait_for_a_qmp_that_never_answers() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let record = Record {
            qmp_port: listener.local_addr().unwrap().port(),
            name: "hung".into(),
            ..Default::default()
        };
        // Every connection is accepted and held, none answered, for the whole test.
        let held = Arc::new(Mutex::new(Vec::new()));
        let keep = held.clone();
        std::thread::spawn(move || {
            while let Ok((stream, _)) = listener.accept() {
                keep.lock().unwrap().push(stream);
            }
        });
        let child = Command::new("sleep").arg("30").spawn().unwrap();
        let inner = Arc::new(Inner {
            child: Mutex::new(child),
            exit: Mutex::new(None),
            exited: Condvar::new(),
            polled: Polled::new(RunState::Running),
            gate: Mutex::new(()),
        });
        let wake: Wake = Arc::new(|_| {});
        let poller = (inner.clone(), record.clone());
        std::thread::spawn(move || poll_state(&poller.0, &poller.1, &wake, MachineId(1)));
        let session = QemuSession {
            id: MachineId(1),
            inner: inner.clone(),
            record,
            display: None,
            removable: vec![],
            heads: vec![],
            console_taken: AtomicBool::new(false),
        };
        // The poller is now stuck waiting on its first answer.
        let deadline = Instant::now() + Duration::from_secs(5);
        while held.lock().unwrap().is_empty() {
            assert!(Instant::now() < deadline, "the poller never connected");
            std::thread::sleep(Duration::from_millis(10));
        }
        // A regression would take two seconds a read, so bound the loop.
        let (done, finished) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for _ in 0..100 {
                assert_eq!(session.status().state, RunState::Running);
            }
            let _ = done.send(());
        });
        let result = finished.recv_timeout(Duration::from_secs(2));
        let _ = inner.child.lock().unwrap().kill();
        assert!(result.is_ok(), "status waited on the hung QMP");
    }
}
