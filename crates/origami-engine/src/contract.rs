//! The contract every [`Engine`] meets, as a suite a test runs against an
//! engine and a [`Harness`] that supplies what the suite cannot invent: a
//! scratch directory, firmware images, free ports, and a way to see what the
//! engine's program was asked to do. Each check panics on a violation.
//!
//! `FakeEngine` passes it in this crate's tests; the QEMU adapter passes it
//! against the fake QEMU in the product's integration tests.

use crate::{
    ConsoleIo, Control, Engine, LaunchPlan, MachineId, RunState, Session, Stop, Wake, Wiring,
};
use origami_machine::dir::MachineDir;
use origami_machine::model::{EngineChoice, Machine};
use origami_machine::stores::FirmwareSet;
use origami_machine::{Drive, Offering};
use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

/// What the suite needs from the engine under test.
pub trait Harness {
    fn engine(&self) -> &dyn Engine;

    /// An empty directory the suite makes machine directories in.
    fn root(&self) -> &Path;

    /// Firmware images `prepare_stores` reads.
    fn firmware(&self) -> FirmwareSet;

    /// The id of the offering the suite builds machines from. It must have
    /// a SCSI bus 0 with at least two targets.
    fn offering(&self) -> String;

    /// A wiring for a launch called `name`, with ports nothing else uses.
    fn wiring(&self, name: &str) -> Wiring;

    /// Launch `plan` so that the guest dies by itself soon after, having
    /// written an error.
    fn launch_crashing(&self, plan: LaunchPlan, wake: Wake) -> crate::Result<Arc<dyn Session>>;

    /// The controls the engine's program was sent for the last session
    /// launched in `dir`, in order.
    fn controls_seen(&self, dir: &MachineDir) -> Vec<Control>;

    /// The media changes it was sent for that session, in order.
    fn media_seen(&self, dir: &MachineDir) -> Vec<(String, Option<PathBuf>)>;

    /// The controls the engine must offer. Every engine offers all five
    /// unless its emulator models fewer.
    fn expected_controls(&self) -> Vec<Control> {
        vec![
            Control::Reset,
            Control::PowerButton,
            Control::Nmi,
            Control::Pause,
            Control::Resume,
        ]
    }

    /// Whether the crash `launch_crashing` causes is a signal, which leaves
    /// no exit status, rather than a failing exit.
    fn crashes_by_signal(&self) -> bool {
        false
    }

    /// Whether `controls_seen` and `media_seen` can see what the engine's
    /// program was asked. An engine that runs the real emulator has no log
    /// to read, and the suite then checks the effects it can see.
    fn observes_requests(&self) -> bool {
        true
    }
}

/// Run every check.
pub fn run(h: &dyn Harness) {
    catalogue_shape(h);
    validate_reports(h);
    stores_and_disks(h);
    plan_is_pure(h);
    lifecycle(h);
    consoles_open_once(h);
    controls_and_media(h);
    screendump_to_a_path(h);
    exit_is_reported(h);
}

/// A counter for a session's `Wake` calls.
fn counter() -> (Wake, Arc<AtomicUsize>) {
    let count = Arc::new(AtomicUsize::new(0));
    let seen = count.clone();
    (
        Arc::new(move |_: MachineId| {
            seen.fetch_add(1, Ordering::SeqCst);
        }),
        count,
    )
}

fn wait_until(what: &str, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !condition() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn exited(session: &dyn Session) -> Option<(Option<i32>, Option<String>)> {
    match session.status().state {
        RunState::Exited {
            code, last_error, ..
        } => Some((code, last_error)),
        _ => None,
    }
}

fn offering(h: &dyn Harness) -> Offering {
    let id = h.offering();
    let catalogue = h.engine().catalogue().expect("the catalogue loads");
    catalogue
        .offerings
        .iter()
        .find(|offering| offering.id == id)
        .unwrap_or_else(|| panic!("the catalogue has no offering {id}"))
        .clone()
}

/// A machine directory with a machine of the harness's offering in it:
/// stores prepared when `stores` is set, a disk on the first target and a
/// CD-ROM on the second.
struct Fixture {
    dir: MachineDir,
    machine: Machine,
}

fn fixture(h: &dyn Harness, name: &str, stores: bool) -> Fixture {
    let offering = offering(h);
    let dir = MachineDir::new(h.root().join(name));
    for part in [dir.path().to_path_buf(), dir.drives(), dir.logs()] {
        fs::create_dir_all(part).unwrap();
    }
    if stores {
        h.engine()
            .prepare_stores(&offering, &h.firmware(), &dir.state())
            .expect("prepare_stores");
    }
    let mut machine = Machine::from_offering(&offering, None, None, vec![], BTreeMap::new());
    machine.engine = EngineChoice {
        id: h.engine().info().id,
        private: Default::default(),
    };
    let targets = &offering.scsi_adapters[0].targets;
    let disk = h
        .engine()
        .create_disk(&dir.drives().join("disk"), 1)
        .expect("create_disk");
    let cd = dir.drives().join("cd.iso");
    fs::write(&cd, vec![0u8; 2048]).unwrap();
    let drive = |name: &str, kind: &str, target: u32, image: &Path| Drive {
        name: name.into(),
        kind: kind.into(),
        bus: 0,
        target,
        image: image
            .strip_prefix(dir.path())
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/"),
        read_only: kind == "cdrom",
        overlay: false,
    };
    machine.drives = vec![
        drive("disk", "disk", targets[0], &disk),
        drive("cd", "cdrom", targets[1], &cd),
    ];
    Fixture { dir, machine }
}

fn plan_of(h: &dyn Harness, fixture: &Fixture, name: &str, paused: bool) -> LaunchPlan {
    let mut wiring = h.wiring(name);
    wiring.paused = paused;
    h.engine()
        .plan(&fixture.machine, &fixture.dir, &wiring)
        .expect("plan")
}

pub fn catalogue_shape(h: &dyn Harness) {
    let engine = h.engine();
    let info = engine.info();
    assert!(!info.id.is_empty());
    let caps = &info.capabilities;
    assert!(caps.max_heads >= 1);
    assert!(caps.controls.contains(&Control::Reset));
    assert!(!caps.disk_formats.is_empty());
    for kind in &caps.disk_formats {
        assert!(
            kind.formats.contains(&kind.preferred),
            "{:?}: the preferred format is not among the formats",
            kind.kind
        );
    }
    let catalogue = engine.catalogue().expect("catalogue");
    assert!(!catalogue.offerings.is_empty());
    let mut ids = std::collections::HashSet::new();
    for offering in &catalogue.offerings {
        assert!(
            ids.insert(&offering.id),
            "duplicate offering {}",
            offering.id
        );
        assert!(
            offering.engine.contains_key(&info.id),
            "{} has no data for {}",
            offering.id,
            info.id
        );
        assert!(
            !offering.storage.is_empty(),
            "{} has no stores",
            offering.id
        );
    }
    // Asking again gives the same list.
    let again = engine.catalogue().expect("catalogue again");
    let again: Vec<_> = again.offerings.iter().map(|o| &o.id).collect();
    let first: Vec<_> = catalogue.offerings.iter().map(|o| &o.id).collect();
    assert_eq!(first, again);
    offering(h);
}

pub fn validate_reports(h: &dyn Harness) {
    // Stores are missing, so the machine is refused; once they are there it
    // is accepted.
    let bare = fixture(h, "validate-bare", false);
    let report = h.engine().validate(&bare.machine, &bare.dir);
    assert!(!report.is_ok(), "a machine without stores validated");
    assert!(report.into_result().is_err());

    let good = fixture(h, "validate-good", true);
    let report = h.engine().validate(&good.machine, &good.dir);
    assert!(report.is_ok(), "{:?}", report.problems);
    report.into_result().unwrap();

    let mut too_small = good.machine.clone();
    too_small.memory.per_node_mib = 1;
    let report = h.engine().validate(&too_small, &good.dir);
    assert!(!report.is_ok(), "1 MiB per node validated");
    assert!(report.errors().all(|p| !p.message.is_empty()));

    let mut missing = good.machine.clone();
    missing.drives[1].image = "drives/absent.iso".into();
    assert!(!h.engine().validate(&missing, &good.dir).is_ok());

    let mut twice = good.machine.clone();
    twice.drives[1].target = twice.drives[0].target;
    assert!(!h.engine().validate(&twice, &good.dir).is_ok());
}

pub fn stores_and_disks(h: &dyn Harness) {
    let engine = h.engine();
    let offering = offering(h);
    let dir = MachineDir::new(h.root().join("stores"));
    fs::create_dir_all(dir.path()).unwrap();
    let state = dir.state();
    engine
        .prepare_stores(&offering, &h.firmware(), &state)
        .expect("prepare_stores");
    for item in &offering.storage {
        let file = state.join(format!("{}.raw", item.name));
        assert_eq!(fs::metadata(&file).unwrap().len(), item.size, "{file:?}");
    }
    // A second call finds the state and fails without touching it.
    assert!(engine
        .prepare_stores(&offering, &h.firmware(), &state)
        .is_err());
    for item in &offering.storage {
        let file = state.join(format!("{}.raw", item.name));
        assert_eq!(fs::metadata(&file).unwrap().len(), item.size);
    }
    // A failure leaves nothing behind.
    let missing = FirmwareSet {
        boot_prom: h.root().join("no-such-prom"),
        io_prom: None,
    };
    let other = dir.path().join("other-state");
    assert!(engine.prepare_stores(&offering, &missing, &other).is_err());
    assert!(!other.exists(), "a failed prepare_stores left its state");

    let preferred = engine
        .info()
        .capabilities
        .disk_formats
        .iter()
        .find(|formats| formats.kind == crate::DriveKind::Disk)
        .expect("disk formats")
        .preferred;
    let disk = engine
        .create_disk(&dir.path().join("new"), 2)
        .expect("create_disk");
    assert!(disk.is_file());
    assert_eq!(
        disk.extension().and_then(|e| e.to_str()),
        Some(preferred.extension())
    );
    assert!(engine.create_disk(&dir.path().join("new"), 2).is_err());
}

fn listing(root: &Path) -> Vec<(PathBuf, u64)> {
    let mut found = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(dir).unwrap().flatten() {
            let meta = entry.metadata().unwrap();
            if meta.is_dir() {
                pending.push(entry.path());
            }
            found.push((entry.path(), meta.len()));
        }
    }
    found.sort();
    found
}

pub fn plan_is_pure(h: &dyn Harness) {
    let fixture = fixture(h, "plan", true);
    let engine = h.engine();
    let wiring = h.wiring("alpha");
    let plan = |wiring: &Wiring| {
        engine
            .plan(&fixture.machine, &fixture.dir, wiring)
            .expect("plan")
    };
    let before = listing(h.root());
    let first = plan(&wiring);
    let second = plan(&wiring);
    assert_eq!(first, second, "equal inputs gave different plans");
    assert_eq!(first.engine(), engine.info().id);
    assert_eq!(first.dir(), fixture.dir.path());
    assert_eq!(before, listing(h.root()), "planning touched the disk");
    let other = plan(&Wiring {
        name: "beta".into(),
        ..wiring.clone()
    });
    assert_ne!(first, other, "the wiring did not reach the plan");
    let paused = plan(&Wiring {
        paused: true,
        ..wiring
    });
    assert_ne!(first, paused, "starting paused did not reach the plan");
    // A plan is data: it survives a round trip through JSON.
    let again: LaunchPlan = serde_json::from_str(&serde_json::to_string(&first).unwrap()).unwrap();
    assert_eq!(first, again);
}

pub fn lifecycle(h: &dyn Harness) {
    let fixture = fixture(h, "lifecycle", true);
    let (wake, woken) = counter();

    let session = h
        .engine()
        .launch(plan_of(h, &fixture, "clean", false), wake.clone())
        .expect("launch");
    assert_eq!(session.status().state, RunState::Running);
    assert!(exited(session.as_ref()).is_none());
    session
        .stop(Stop::Clean {
            timeout: Some(Duration::from_secs(30)),
        })
        .expect("clean stop");
    let (code, error) = exited(session.as_ref()).expect("a stopped session has exited");
    assert_eq!(code, Some(0));
    assert_eq!(error, None);
    wait_until("the exit wake", || woken.load(Ordering::SeqCst) >= 1);
    // Stopping what has stopped is not an error, and nothing else works.
    session.stop(Stop::Force).expect("stop after exit");
    assert!(session.control(Control::Reset).is_err());

    let session = h
        .engine()
        .launch(plan_of(h, &fixture, "forced", true), wake)
        .expect("launch paused");
    assert_eq!(session.status().state, RunState::Prelaunch);
    session.control(Control::Resume).expect("resume");
    wait_until("running", || session.status().state == RunState::Running);
    session.stop(Stop::Force).expect("force stop");
    wait_until("the forced exit", || exited(session.as_ref()).is_some());
    // The wake follows the exit by a moment, longer under load.
    wait_until("the forced exit wake", || woken.load(Ordering::SeqCst) >= 2);
}

/// A launch whose console port is already taken still starts, on another.
pub fn launch_survives_a_taken_port(h: &dyn Harness) {
    let fixture = fixture(h, "taken-port", true);
    let wiring = h.wiring("taken-port");
    let _held = std::net::TcpListener::bind(("127.0.0.1", wiring.console_port)).expect("hold");
    let plan = h
        .engine()
        .plan(&fixture.machine, &fixture.dir, &wiring)
        .expect("plan");
    let (wake, _) = counter();
    let session = h.engine().launch(plan, wake).expect("launch");
    session.stop(Stop::Force).expect("force stop");
}

fn launch(h: &dyn Harness, fixture: &Fixture, name: &str) -> Arc<dyn Session> {
    let (wake, _) = counter();
    h.engine()
        .launch(plan_of(h, fixture, name, false), wake)
        .expect("launch")
}

/// Read once from `console`, failing the test rather than hanging it.
fn read_some(mut console: Box<dyn ConsoleIo>) -> (Box<dyn ConsoleIo>, Vec<u8>) {
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buffer = [0u8; 256];
        let count = console.read(&mut buffer).unwrap_or(0);
        let _ = sender.send((console, buffer[..count].to_vec()));
    });
    receiver
        .recv_timeout(Duration::from_secs(120))
        .expect("the console gave no output")
}

pub fn consoles_open_once(h: &dyn Harness) {
    let fixture = fixture(h, "console", true);
    let session = launch(h, &fixture, "console");
    let consoles = session.consoles();
    assert_eq!(consoles.iter().filter(|c| c.primary).count(), 1);
    let name = consoles[0].name.clone();
    assert!(session.open_console("no-such-console").is_err());
    let console = session.open_console(&name).expect("first open");
    assert!(
        session.open_console(&name).is_err(),
        "a console opened twice"
    );
    let (mut console, output) = read_some(console);
    assert!(!output.is_empty());
    console.write_all(b"hello\r").expect("write to the console");
    session.stop(Stop::Force).unwrap();
}

pub fn controls_and_media(h: &dyn Harness) {
    let fixture = fixture(h, "controls", true);
    let session = launch(h, &fixture, "controls");
    let offered = h.engine().info().capabilities.controls;
    for control in [
        Control::Reset,
        Control::PowerButton,
        Control::Nmi,
        Control::Pause,
        Control::Resume,
    ] {
        assert_eq!(
            offered.contains(&control),
            h.expected_controls().contains(&control),
            "{control:?}"
        );
    }
    let mut sent = Vec::new();
    for control in [
        Control::Reset,
        Control::Nmi,
        Control::Pause,
        Control::Resume,
    ] {
        // A control the engine does not offer is refused, never ignored.
        if !offered.contains(&control) {
            assert!(
                session.control(control).is_err(),
                "{control:?} is not offered but was accepted"
            );
            continue;
        }
        session
            .control(control)
            .unwrap_or_else(|e| panic!("{control:?}: {e}"));
        sent.push(control);
        match control {
            Control::Pause => assert_eq!(session.status().state, RunState::Paused),
            Control::Resume => assert_eq!(session.status().state, RunState::Running),
            _ => {}
        }
    }

    let iso = fixture.dir.drives().join("other.iso");
    fs::write(&iso, vec![0u8; 2048]).unwrap();
    session.media("cd", Some(&iso)).expect("insert media");
    session.media("cd", None).expect("eject media");
    assert!(
        session.media("disk", Some(&iso)).is_err(),
        "media went into a hard disk"
    );
    assert!(session.media("no-such-drive", None).is_err());

    // The power button is the guest's own shutdown, so the session ends.
    if offered.contains(&Control::PowerButton) {
        session.control(Control::PowerButton).expect("power button");
        sent.push(Control::PowerButton);
        wait_until("the shutdown", || exited(session.as_ref()).is_some());
        assert_eq!(exited(session.as_ref()).unwrap().0, Some(0));
    } else {
        assert!(session.control(Control::PowerButton).is_err());
        session.stop(Stop::Force).unwrap();
    }

    if h.observes_requests() {
        assert_eq!(h.controls_seen(&fixture.dir), sent);
        assert_eq!(
            h.media_seen(&fixture.dir),
            vec![
                ("cd".to_string(), Some(iso.clone())),
                ("cd".to_string(), None)
            ]
        );
    }
}

pub fn screendump_to_a_path(h: &dyn Harness) {
    let fixture = fixture(h, "screendump", true);
    let session = launch(h, &fixture, "screendump");
    let path = fixture.dir.path().join("shot.ppm");
    session.screendump(0, &path).expect("screendump");
    assert!(fs::metadata(&path).unwrap().len() > 0);
    session.stop(Stop::Force).unwrap();
}

pub fn exit_is_reported(h: &dyn Harness) {
    let fixture = fixture(h, "crash", true);
    let (wake, woken) = counter();
    let session = h
        .launch_crashing(plan_of(h, &fixture, "crash", false), wake)
        .expect("launch");
    wait_until("the crash", || exited(session.as_ref()).is_some());
    let (code, error) = exited(session.as_ref()).unwrap();
    if h.crashes_by_signal() {
        assert_eq!(code, None);
    } else {
        assert!(matches!(code, Some(code) if code != 0), "{code:?}");
    }
    assert!(
        error.as_deref().is_some_and(|error| !error.is_empty()),
        "no last error: {error:?}"
    );
    wait_until("the exit wake", || woken.load(Ordering::SeqCst) >= 1);
    assert!(session.control(Control::Reset).is_err());
}
