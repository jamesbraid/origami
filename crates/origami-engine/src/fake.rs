//! [`FakeEngine`]: an engine that runs no emulator, for tests of everything
//! above the engine boundary (the facade, the bindings, the front ends).
//!
//! It is deterministic and in-process. Its catalogue has three offerings;
//! a launched machine has synthetic frames that change when the test says
//! so, a console that prints a banner and then echoes what is written to it,
//! controls and media changes that are recorded, and an exit that the test
//! can cause with [`FakeSession::crash`]. The contract suite in
//! [`crate::contract`] passes on it.

use crate::{
    Capabilities, Catalogue, ConsoleInfo, ConsoleIo, ConsoleTransport, Control, CpuReading,
    CpuTimes, CursorInfo, DiskFormat, DisplayEndpoint, DisplayKind, DisplayRequest, DriveFormats,
    DriveKind, Engine, EngineInfo, Facts, Frame, FrameSource, FrameUpdate, HeadInfo, IdentityKind,
    InputEvent, LaunchPlan, MachineId, Report, Result, RunState, Session, Stop, Wake, Wiring,
};
use origami_machine::dir::MachineDir;
use origami_machine::model::Machine;
use origami_machine::stores::FirmwareSet;
use origami_machine::Offering;
use serde_json::json;
use std::collections::VecDeque;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Condvar, Mutex};

/// The engine id, and the key of the engine's data in the catalogue.
pub const ID: &str = "fake";

/// The one console of a fake session.
pub const CONSOLE: &str = "console";

const WIDTH: u32 = 64;
const HEIGHT: u32 = 48;

/// What the fake console prints when opened.
const BANNER: &[u8] = b"fake console\r\nlogin: ";

pub struct FakeEngine {
    catalogue: Arc<Catalogue>,
    next_id: AtomicU32,
    sessions: Mutex<Vec<Arc<FakeSession>>>,
    validations: AtomicU32,
    validate_hook: Mutex<Option<ValidateHook>>,
}

/// Called with the number of each `validate` call; an error text fails it.
/// It may block, to hold a caller inside the call.
pub type ValidateHook = Arc<dyn Fn(u32) -> Option<String> + Send + Sync>;

impl Default for FakeEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeEngine {
    pub fn new() -> Self {
        Self {
            catalogue: Arc::new(catalogue()),
            next_id: AtomicU32::new(1),
            sessions: Mutex::new(Vec::new()),
            validations: AtomicU32::new(0),
            validate_hook: Mutex::new(None),
        }
    }

    /// Run `hook` on every later `validate`, numbered from the count so far.
    pub fn set_validate_hook(&self, hook: ValidateHook) {
        *self.validate_hook.lock().unwrap() = Some(hook);
    }

    /// How many times `validate` has been called.
    pub fn validations(&self) -> u32 {
        self.validations.load(Ordering::SeqCst)
    }

    /// Every session launched so far, oldest first, for a test to drive.
    pub fn sessions(&self) -> Vec<Arc<FakeSession>> {
        self.sessions.lock().unwrap().clone()
    }

    /// The most recent session launched from the machine directory `dir`.
    pub fn session_in(&self, dir: &Path) -> Option<Arc<FakeSession>> {
        let sessions = self.sessions.lock().unwrap();
        sessions.iter().rev().find(|s| s.dir == dir).cloned()
    }
}

/// Three offerings: a one-node machine with one head, the same with two
/// nodes, and one with two heads.
fn catalogue() -> Catalogue {
    let offering = |id: &str, topology: &str, nodes: u32, heads: u32| {
        json!({
            "id": id,
            "product": "fake",
            "topology": topology,
            "nodes": nodes,
            "cpus": nodes,
            "cpus-per-node": vec![1; nodes as usize],
            "hostid-source": "text",
            "processor": {"id": "fake-cpu", "part": "FAKE1", "clock-hz": 100_000_000, "cache-mib": 1},
            "memory-per-node-mib": {"accepted": [64, 128, 256], "default": 128},
            "storage": [
                {"name": "nvram0", "size": 256, "read-only": false, "initial": "zero"},
                {"name": "flash0", "size": 4096, "read-only": false, "initial": "boot-prom"},
            ],
            "scsi-adapters": [{"bus": "scsi.0", "targets": [1, 2, 3, 4]}],
            "consoles": [{"kind": "console", "node": 0, "name": "ttyS0"}],
            "engines": [ID],
            "engine": {ID: {"heads": heads}},
        })
    };
    serde_json::from_value(json!({
        "version": 1,
        "offerings": [
            offering("fake.1", "fake", 1, 1),
            offering("fake.2", "fake", 2, 1),
            offering("fakeview.1", "fakeview", 1, 2),
            offering("fakeblind.1", "fakeblind", 1, 0),
        ],
    }))
    .expect("the fake catalogue is well formed")
}

fn heads_of(offering: &Offering) -> u32 {
    offering.engine[ID]["heads"].as_u64().unwrap_or(1) as u32
}

impl Engine for FakeEngine {
    fn info(&self) -> EngineInfo {
        EngineInfo {
            id: ID.into(),
            version: Some("fake".into()),
            capabilities: Capabilities {
                display: DisplayKind::InProcess,
                max_heads: 2,
                gpu_regions: false,
                controls: vec![
                    Control::Reset,
                    Control::PowerButton,
                    Control::Nmi,
                    Control::Pause,
                    Control::Resume,
                ],
                disk_formats: vec![
                    DriveFormats {
                        kind: DriveKind::Disk,
                        formats: vec![DiskFormat::Raw],
                        preferred: DiskFormat::Raw,
                    },
                    DriveFormats {
                        kind: DriveKind::Cdrom,
                        formats: vec![DiskFormat::Raw],
                        preferred: DiskFormat::Raw,
                    },
                ],
                console: ConsoleTransport::RawTcp,
                identity: IdentityKind::Qemu,
                attach: false,
                audio: vec![],
            },
        }
    }

    fn catalogue(&self) -> Result<Arc<Catalogue>> {
        Ok(self.catalogue.clone())
    }

    fn firmware(
        &self,
        _offering: &Offering,
        boot_prom: Option<&Path>,
        io_prom: Option<&Path>,
    ) -> Result<FirmwareSet> {
        let boot_prom = boot_prom.ok_or("supply --prom FILE")?;
        Ok(FirmwareSet {
            boot_prom: boot_prom.into(),
            io_prom: io_prom.map(Into::into),
        })
    }

    fn validate(&self, m: &Machine, dir: &MachineDir) -> Report {
        let mut report = Report::default();
        let call = self.validations.fetch_add(1, Ordering::SeqCst) + 1;
        let hook = self.validate_hook.lock().unwrap().clone();
        if let Some(error) = hook.and_then(|hook| hook(call)) {
            report.push(crate::Severity::Error, error);
            return report;
        }
        if m.engine.id != ID {
            report.push(
                crate::Severity::Error,
                format!("the machine is for {}, not {ID}", m.engine.id),
            );
        }
        let Some(offering) = self.catalogue.offerings.iter().find(|o| {
            Some(&o.topology) == m.topology.as_ref() && o.product == m.product && o.nodes == m.nodes
        }) else {
            report.push(crate::Severity::Error, "no such offering");
            return report;
        };
        if !offering.memory.accepted.contains(&m.memory.per_node_mib) {
            report.push(
                crate::Severity::Error,
                format!("{} MiB per node is not offered", m.memory.per_node_mib),
            );
        }
        for item in &offering.storage {
            if !dir.state().join(format!("{}.raw", item.name)).is_file() {
                report.push(
                    crate::Severity::Error,
                    format!("missing machine state {}", item.name),
                );
            }
        }
        let mut taken = Vec::new();
        for drive in &m.drives {
            let spot = (drive.bus, drive.target);
            let known = offering
                .scsi_bus(drive.bus)
                .is_some_and(|bus| bus.targets.contains(&drive.target));
            if !known {
                report.push(
                    crate::Severity::Error,
                    format!(
                        "unsupported SCSI target {} on bus {}",
                        drive.target, drive.bus
                    ),
                );
            } else if taken.contains(&spot) {
                report.push(
                    crate::Severity::Error,
                    format!(
                        "duplicate SCSI target {} on bus {}",
                        drive.target, drive.bus
                    ),
                );
            }
            taken.push(spot);
            if !dir.resolve(&drive.image).is_file() {
                report.push(
                    crate::Severity::Error,
                    format!("missing drive image {}", drive.image),
                );
            }
        }
        report
    }

    fn prepare_stores(&self, o: &Offering, proms: &FirmwareSet, state: &Path) -> Result<()> {
        for prom in std::iter::once(&proms.boot_prom).chain(&proms.io_prom) {
            if !prom.is_file() {
                return Err(format!("cannot read {}", prom.display()).into());
            }
        }
        fs::create_dir(state)?;
        let result = o.storage.iter().try_for_each(|item| {
            fs::File::create(state.join(format!("{}.raw", item.name)))?.set_len(item.size)
        });
        if let Err(error) = result {
            let _ = fs::remove_dir_all(state);
            return Err(error.into());
        }
        Ok(())
    }

    fn create_disk(&self, stem: &Path, size_mib: u64) -> Result<PathBuf> {
        let mut name = stem.as_os_str().to_owned();
        name.push(".");
        name.push(DiskFormat::Raw.extension());
        let path = PathBuf::from(name);
        if path.exists() {
            return Err(format!("disk exists: {}", path.display()).into());
        }
        fs::File::create(&path)?.set_len(size_mib << 20)?;
        Ok(path)
    }

    fn plan(&self, m: &Machine, dir: &MachineDir, w: &Wiring) -> Result<LaunchPlan> {
        let offering = self
            .catalogue
            .offerings
            .iter()
            .find(|o| Some(&o.topology) == m.topology.as_ref() && o.nodes == m.nodes)
            .ok_or("no such offering")?;
        let removable: Vec<&str> = m
            .drives
            .iter()
            .filter(|drive| drive.kind == "cdrom")
            .map(|drive| drive.name.as_str())
            .collect();
        Ok(LaunchPlan::new(
            ID,
            dir.path().to_path_buf(),
            json!({
                "heads": heads_of(offering),
                "memory_mib": m.memory.per_node_mib,
                "name": w.name,
                "paused": w.paused,
                "display": !matches!(w.display, DisplayRequest::None),
                "removable": removable,
            }),
        ))
    }

    fn launch(&self, plan: LaunchPlan, wake: Wake) -> Result<Arc<dyn Session>> {
        if plan.engine() != ID {
            return Err(format!("a {} plan is not a fake plan", plan.engine()).into());
        }
        let payload = plan.payload();
        let heads = payload["heads"].as_u64().unwrap_or(1) as usize;
        let removable = payload["removable"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|name| name.as_str().map(str::to_owned))
            .collect();
        let state = if payload["paused"].as_bool() == Some(true) {
            RunState::Prelaunch
        } else {
            RunState::Running
        };
        let console = Arc::new(ConsoleCore::default());
        console.write_out(BANNER);
        let session = Arc::new(FakeSession {
            ignores_stop: AtomicBool::new(false),
            id: MachineId(self.next_id.fetch_add(1, Ordering::Relaxed)),
            dir: plan.dir().to_path_buf(),
            wake,
            has_display: payload["display"].as_bool() == Some(true),
            frames: Arc::new(Frames::new(heads)),
            console,
            console_taken: AtomicBool::new(false),
            removable,
            born: std::time::Instant::now(),
            record: Mutex::new(Record {
                state,
                controls: vec![],
                media: vec![],
                screendumps: vec![],
                input: vec![],
            }),
        });
        self.sessions.lock().unwrap().push(session.clone());
        Ok(session)
    }
}

/// What a session was asked to do, and where it stands.
struct Record {
    state: RunState,
    controls: Vec<Control>,
    media: Vec<(String, Option<PathBuf>)>,
    screendumps: Vec<(usize, PathBuf)>,
    input: Vec<InputEvent>,
}

pub struct FakeSession {
    ignores_stop: AtomicBool,
    id: MachineId,
    dir: PathBuf,
    wake: Wake,
    has_display: bool,
    frames: Arc<Frames>,
    console: Arc<ConsoleCore>,
    console_taken: AtomicBool,
    removable: Vec<String>,
    record: Mutex<Record>,
    born: std::time::Instant,
}

impl FakeSession {
    /// Change the picture of every head: the next `take_frame` of each
    /// returns a frame, and the session's `Wake` is called.
    pub fn advance_frame(&self) {
        self.frames.advance();
        (self.wake)(self.id);
    }

    /// Whether the front end last told the frame source it is shown.
    pub fn frames_visible(&self) -> bool {
        self.frames.visible.load(Ordering::Acquire)
    }

    /// The guest shows a cursor: 8 by 8 opaque white, hot spot at its top
    /// left, with its hot spot at (`x`, `y`) in the picture.
    pub fn show_cursor(&self, x: i32, y: i32) {
        *self.frames.cursor.lock().unwrap() = Some((x, y));
        (self.wake)(self.id);
    }

    /// A clean stop is not acted on: it blocks until the session is forced
    /// or exits, as a guest that ignores the request does.
    pub fn ignore_clean_stop(&self) {
        self.ignores_stop.store(true, Ordering::Release);
    }

    /// The guest dies with `code`, having said `message` last.
    pub fn crash(&self, code: i32, message: &str) {
        self.exit(Some(code), None, Some(message.into()));
    }

    /// The guest shuts itself down.
    pub fn shut_down(&self) {
        self.exit(Some(0), None, None);
    }

    /// The guest's program is gone and left no word of how.
    pub fn vanish(&self) {
        self.exit(None, None, None);
    }

    /// The guest's program is killed by `signal`.
    pub fn kill(&self, signal: i32) {
        self.exit(None, Some(signal), None);
    }

    /// Text the guest prints on its console.
    pub fn say(&self, text: &str) {
        self.console.write_out(text.as_bytes());
    }

    /// What the host has written to the console so far.
    pub fn typed(&self) -> Vec<u8> {
        self.console.inner.lock().unwrap().typed.clone()
    }

    /// The controls sent, in order.
    pub fn controls(&self) -> Vec<Control> {
        self.record.lock().unwrap().controls.clone()
    }

    /// The media changes made, in order: the drive and the image, None for
    /// an eject.
    pub fn media_changes(&self) -> Vec<(String, Option<PathBuf>)> {
        self.record.lock().unwrap().media.clone()
    }

    /// The input events sent, in order.
    pub fn input(&self) -> Vec<InputEvent> {
        self.record.lock().unwrap().input.clone()
    }

    pub fn screendumps(&self) -> Vec<(usize, PathBuf)> {
        self.record.lock().unwrap().screendumps.clone()
    }

    fn exit(&self, code: Option<i32>, signal: Option<i32>, last_error: Option<String>) {
        {
            let mut record = self.record.lock().unwrap();
            if matches!(record.state, RunState::Exited { .. }) {
                return;
            }
            record.state = RunState::Exited {
                code,
                signal,
                last_error,
            };
        }
        self.console.close();
        (self.wake)(self.id);
    }

    fn running(&self) -> Result<()> {
        match self.record.lock().unwrap().state {
            RunState::Exited { .. } => Err("the machine has exited".into()),
            _ => Ok(()),
        }
    }
}

impl Session for FakeSession {
    fn id(&self) -> MachineId {
        self.id
    }

    fn display(&self) -> DisplayEndpoint {
        if self.has_display {
            DisplayEndpoint::Frames(self.frames.clone())
        } else {
            DisplayEndpoint::None
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
        Ok(Box::new(FakeConsole(self.console.clone())))
    }

    fn control(&self, c: Control) -> Result<()> {
        self.running()?;
        let mut record = self.record.lock().unwrap();
        record.controls.push(c);
        match c {
            Control::Pause => record.state = RunState::Paused,
            Control::Resume => record.state = RunState::Running,
            Control::Reset | Control::Nmi | Control::PowerButton => {}
        }
        drop(record);
        // The power button makes the guest shut down and exit.
        if c == Control::PowerButton {
            self.exit(Some(0), None, None);
        }
        Ok(())
    }

    fn send_input(&self, events: &[InputEvent]) -> Result<()> {
        self.running()?;
        self.record.lock().unwrap().input.extend_from_slice(events);
        Ok(())
    }

    fn media(&self, drive: &str, image: Option<&Path>) -> Result<()> {
        self.running()?;
        if !self.removable.iter().any(|name| name == drive) {
            return Err(format!("{drive} is not a removable drive of this machine").into());
        }
        let mut record = self.record.lock().unwrap();
        record.media.push((drive.into(), image.map(Into::into)));
        Ok(())
    }

    fn screendump(&self, head: usize, path: &Path) -> Result<()> {
        self.running()?;
        let ppm = self.frames.ppm(head).ok_or("no such head")?;
        fs::write(path, ppm)?;
        self.record
            .lock()
            .unwrap()
            .screendumps
            .push((head, path.into()));
        Ok(())
    }

    fn stop(&self, how: Stop) -> Result<()> {
        match how {
            Stop::Clean { .. } if self.ignores_stop.load(Ordering::Acquire) => {
                while !matches!(self.record.lock().unwrap().state, RunState::Exited { .. }) {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
            }
            Stop::Clean { .. } => self.exit(Some(0), None, None),
            Stop::Force => self.exit(None, Some(9), None),
        }
        Ok(())
    }

    fn status(&self) -> Facts {
        let state = self.record.lock().unwrap().state.clone();
        Facts { state, pid: None }
    }

    /// Two CPUs at a steady 30% user and 10% system, and 60% and 5%.
    fn cpu_times(&self) -> Result<CpuReading> {
        let up = self.born.elapsed();
        let at = |user: f64, sys: f64| CpuTimes {
            user: up.mul_f64(user),
            sys: up.mul_f64(sys),
        };
        Ok(CpuReading::Times(vec![at(0.30, 0.10), at(0.60, 0.05)]))
    }
}

/// The synthetic pictures: every head is `WIDTH` by `HEIGHT`, and pixel
/// (x, y) at picture number `seq` is the bytes `[seq, y, x, 0]`.
struct Frames {
    heads: Mutex<Vec<HeadState>>,
    cursor: Mutex<Option<(i32, i32)>>,
    visible: AtomicBool,
}

struct HeadState {
    seq: u64,
    taken: u64,
    sent: bool,
}

impl Frames {
    fn new(heads: usize) -> Self {
        Self {
            cursor: Mutex::new(None),
            visible: AtomicBool::new(true),
            heads: Mutex::new(
                (0..heads)
                    .map(|_| HeadState {
                        seq: 1,
                        taken: 0,
                        sent: false,
                    })
                    .collect(),
            ),
        }
    }

    fn advance(&self) {
        for head in self.heads.lock().unwrap().iter_mut() {
            head.seq += 1;
        }
    }

    fn fill(seq: u64, pixels: &mut Vec<u8>) {
        pixels.clear();
        for y in 0..HEIGHT {
            for x in 0..WIDTH {
                pixels.extend_from_slice(&[seq as u8, y as u8, x as u8, 0]);
            }
        }
    }

    fn ppm(&self, head: usize) -> Option<Vec<u8>> {
        let seq = self.heads.lock().unwrap().get(head)?.seq;
        let mut pixels = Vec::new();
        Self::fill(seq, &mut pixels);
        let mut ppm = format!("P6\n{WIDTH} {HEIGHT}\n255\n").into_bytes();
        for bgrx in pixels.as_chunks::<4>().0 {
            ppm.extend_from_slice(&[bgrx[2], bgrx[1], bgrx[0]]);
        }
        Some(ppm)
    }
}

impl FrameSource for Frames {
    fn heads(&self) -> Vec<HeadInfo> {
        self.heads
            .lock()
            .unwrap()
            .iter()
            .map(|head| HeadInfo {
                width: WIDTH,
                height: HEIGHT,
                seq: head.seq,
                active: true,
            })
            .collect()
    }

    fn set_visible(&self, visible: bool) {
        self.visible.store(visible, Ordering::Release);
    }

    fn why_blank(&self) -> Option<String> {
        self.heads
            .lock()
            .unwrap()
            .is_empty()
            .then(|| "the fake display has no head".into())
    }

    fn cursor(&self, _head: usize, known_gen: u32) -> Option<CursorInfo> {
        let (x, y) = (*self.cursor.lock().unwrap())?;
        Some(CursorInfo {
            on: true,
            x,
            y,
            hot_x: 0,
            hot_y: 0,
            width: 8,
            height: 8,
            gen: 1,
            image: (known_gen != 1).then(|| vec![0xff; 8 * 8 * 4]),
        })
    }

    fn take_frame(&self, head: usize, pixels: &mut Vec<u8>) -> Option<Frame> {
        let mut heads = self.heads.lock().unwrap();
        let state = heads.get_mut(head)?;
        if state.taken == state.seq {
            return None;
        }
        let resized = !state.sent;
        state.sent = true;
        state.taken = state.seq;
        Self::fill(state.seq, pixels);
        Some(Frame::Rows(FrameUpdate {
            seq: state.seq,
            width: WIDTH,
            height: HEIGHT,
            runs: vec![(0, HEIGHT)],
            resized,
        }))
    }
}

#[derive(Default)]
struct ConsoleCore {
    inner: Mutex<ConsoleBuffer>,
    ready: Condvar,
}

#[derive(Default)]
struct ConsoleBuffer {
    out: VecDeque<u8>,
    typed: Vec<u8>,
    closed: bool,
}

impl ConsoleCore {
    fn write_out(&self, bytes: &[u8]) {
        self.inner.lock().unwrap().out.extend(bytes);
        self.ready.notify_all();
    }

    fn close(&self) {
        self.inner.lock().unwrap().closed = true;
        self.ready.notify_all();
    }
}

/// The host's end of the fake console: reads block until the guest has
/// output, and end when the guest is gone; writes are echoed.
struct FakeConsole(Arc<ConsoleCore>);

impl ConsoleIo for FakeConsole {
    fn split(self: Box<Self>) -> io::Result<(Box<dyn Read + Send>, Box<dyn Write + Send>)> {
        Ok((Box::new(FakeConsole(self.0.clone())), self))
    }
}

impl Read for FakeConsole {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut inner = self.0.inner.lock().unwrap();
        while inner.out.is_empty() && !inner.closed {
            inner = self.0.ready.wait(inner).unwrap();
        }
        let count = buf.len().min(inner.out.len());
        for (slot, byte) in buf.iter_mut().zip(inner.out.drain(..count)) {
            *slot = byte;
        }
        Ok(count)
    }
}

impl Write for FakeConsole {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        {
            let mut inner = self.0.inner.lock().unwrap();
            inner.typed.extend_from_slice(buf);
            inner.out.extend(buf);
        }
        self.0.ready.notify_all();
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
