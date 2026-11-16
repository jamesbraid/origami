//! The operations against the fake engine: edits are all-or-nothing under
//! the edit lock, and what they say is data that survives a round trip.

use origami_core::{
    apply_change, apply_changes, open, validate, Change, DriveKind, DriveRequest, Kind, Report,
};
use origami_engine::fake::{FakeEngine, ID};
use origami_engine::{
    Catalogue, Engine, EngineInfo, EngineStores, LaunchPlan, Report as Findings, Session, Wake,
    Wiring,
};
use origami_machine::dir::MachineDir;
use origami_machine::model::{EngineChoice, Machine};
use origami_machine::stores::FirmwareSet;
use origami_machine::{control, MachineInit, Offering, PortForward, Result};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// The fake engine, which also says whether the machine's edit lock was
/// held each time it was asked to validate. A machine file does not name its
/// engine yet, so validation tells the fake the machine is its own.
#[derive(Default)]
struct Probe {
    fake: FakeEngine,
    locked: AtomicUsize,
    unlocked: AtomicUsize,
}

impl Engine for Probe {
    fn info(&self) -> EngineInfo {
        self.fake.info()
    }
    fn catalogue(&self) -> Result<Arc<Catalogue>> {
        self.fake.catalogue()
    }
    fn firmware(
        &self,
        o: &Offering,
        boot: Option<&Path>,
        io: Option<&Path>,
    ) -> Result<FirmwareSet> {
        self.fake.firmware(o, boot, io)
    }
    fn validate(&self, m: &Machine, dir: &MachineDir) -> Findings {
        let counter = if control::is_locked(dir.path()).unwrap() {
            &self.locked
        } else {
            &self.unlocked
        };
        counter.fetch_add(1, Ordering::SeqCst);
        let mut m = m.clone();
        m.engine = EngineChoice {
            id: ID.into(),
            private: Default::default(),
        };
        self.fake.validate(&m, dir)
    }
    fn prepare_stores(&self, o: &Offering, proms: &FirmwareSet, state: &Path) -> Result<()> {
        self.fake.prepare_stores(o, proms, state)
    }
    fn create_disk(&self, stem: &Path, size_mib: u64) -> Result<PathBuf> {
        self.fake.create_disk(stem, size_mib)
    }
    fn plan(&self, m: &Machine, dir: &MachineDir, w: &Wiring) -> Result<LaunchPlan> {
        self.fake.plan(m, dir, w)
    }
    fn launch(&self, plan: LaunchPlan, wake: Wake) -> Result<Arc<dyn Session>> {
        self.fake.launch(plan, wake)
    }
}

struct Fixture {
    _root: tempfile::TempDir,
    engine: Probe,
    dir: PathBuf,
    image: PathBuf,
}

fn fixture() -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let engine = Probe::default();
    let prom = root.path().join("prom.bin");
    fs::write(&prom, vec![0; 1024]).unwrap();
    let catalogue = engine.catalogue().unwrap();
    let offering = &catalogue.offerings[0];
    let dir = root.path().join("machine");
    origami_machine::create_configured(
        &dir,
        offering,
        &MachineInit {
            stores: &EngineStores(&engine),
            boot_prom: &prom,
            io_prom: None,
        },
        None,
        None,
        vec![],
        BTreeMap::new(),
    )
    .unwrap();
    let image = root.path().join("extra.img");
    fs::write(&image, vec![0; 512]).unwrap();
    Fixture {
        dir: canonical(&dir),
        image,
        engine,
        _root: root,
    }
}

fn canonical(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap()
}

/// Every file under `dir` with its bytes.
fn snapshot(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn walk(dir: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, out);
            } else {
                out.insert(path.clone(), fs::read(&path).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, &mut out);
    // The edit lock file is a marker, not machine state.
    out.retain(|path, _| !path.ends_with("state/machine.lock"));
    out
}

fn attach(image: &Path, target: u32) -> Change {
    Change::AttachDrive(DriveRequest {
        file: image.into(),
        kind: DriveKind::Disk,
        target,
        overlay: false,
    })
}

#[test]
fn a_failing_second_step_leaves_the_machine_byte_identical() {
    let f = fixture();
    let before = snapshot(&f.dir);
    // The first step is fine; the fake machine has no SCSI target 9.
    let error = apply_changes(
        &f.engine,
        &f.dir,
        &[attach(&f.image, 2), attach(&f.image, 9)],
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("unsupported SCSI target 9"),
        "{error}"
    );
    assert_eq!(snapshot(&f.dir), before);
    // The same first step alone is applied.
    apply_change(&f.engine, &f.dir, &attach(&f.image, 2)).unwrap();
    assert_ne!(snapshot(&f.dir), before);
    assert_eq!(open(&f.engine, &f.dir).unwrap().file.drive.len(), 1);
}

#[test]
fn a_disk_made_before_a_failing_step_is_removed() {
    let f = fixture();
    let before = snapshot(&f.dir);
    let error = apply_changes(
        &f.engine,
        &f.dir,
        &[
            Change::CreateDisk { size_mib: 1 },
            Change::DetachDrive("nope".into()),
        ],
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("expected one drive named nope"),
        "{error}"
    );
    assert!(!f.dir.join("drives/system.raw").exists());
    assert_eq!(snapshot(&f.dir), before);
}

#[test]
fn a_batch_is_one_write_and_reports_each_step() {
    let f = fixture();
    let report = apply_changes(
        &f.engine,
        &f.dir,
        &[
            Change::CreateDisk { size_mib: 1 },
            Change::AddForward(PortForward {
                name: "web".into(),
                protocol: "tcp".into(),
                host_port: 8080,
                guest_port: 80,
            }),
        ],
    )
    .unwrap();
    let lines: Vec<_> = report.of(Kind::Result).collect();
    assert!(lines[0].starts_with("created "), "{lines:?}");
    assert_eq!(lines[1], "forward added: web");
    let file = open(&f.engine, &f.dir).unwrap().file;
    assert_eq!(file.drive[0].name, "system");
    assert_eq!(file.network.forward[0].name, "web");
    assert!(!f.dir.join("machine.toml.tmp").exists());
}

#[test]
fn the_edit_lock_is_held_while_an_edit_validates_and_released_after() {
    let f = fixture();
    apply_change(&f.engine, &f.dir, &attach(&f.image, 2)).unwrap();
    assert!(f.engine.locked.load(Ordering::SeqCst) > 0);
    assert_eq!(f.engine.unlocked.load(Ordering::SeqCst), 0);
    assert!(!control::is_locked(&f.dir).unwrap());
}

#[test]
fn a_held_lock_refuses_the_edit_and_changes_nothing() {
    let f = fixture();
    let before = snapshot(&f.dir);
    let _running = control::lock_for_edit(&f.dir, "running").unwrap();
    let error = apply_change(&f.engine, &f.dir, &attach(&f.image, 2)).unwrap_err();
    assert_eq!(error.to_string(), "stop the machine before changing drives");
    assert_eq!(snapshot(&f.dir), before);
}

#[test]
fn validate_reports_the_verdict_as_a_result_line() {
    let f = fixture();
    let report = validate(&f.engine, &f.dir).unwrap();
    let result: Vec<_> = report.of(Kind::Result).collect();
    assert_eq!(result.len(), 1);
    assert!(result[0].starts_with("valid: fake (1 CPUs"), "{result:?}");
}

#[test]
fn a_report_and_a_change_survive_json() {
    let mut report = Report::default();
    report.result("created x");
    report.warning("careful");
    report.push(Kind::GuestNote, "memory changed");
    let text = serde_json::to_string(&report).unwrap();
    assert_eq!(serde_json::from_str::<Report>(&text).unwrap(), report);
    let printed: Vec<_> = report.rendered().map(|(_, text)| text).collect();
    assert_eq!(
        printed,
        [
            "created x",
            "warning: careful",
            "note: memory changed; the guest sees this at its next boot"
        ]
    );

    let change = Change::AddShare {
        name: "src".into(),
        host_dir: "/tmp".into(),
    };
    let text = serde_json::to_string(&change).unwrap();
    assert_eq!(serde_json::from_str::<Change>(&text).unwrap(), change);
}
