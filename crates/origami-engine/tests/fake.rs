//! `FakeEngine` against the contract suite, and the parts of it that tests
//! of other crates rely on.

use origami_engine::contract::{self, Harness};
use origami_engine::fake::FakeEngine;
use origami_engine::{
    Control, DisplayEndpoint, DisplayRequest, Engine, Frame, LaunchPlan, RunState, Session, Wake,
    Wiring,
};
use origami_machine::dir::MachineDir;
use origami_machine::stores::FirmwareSet;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

struct FakeHarness {
    engine: FakeEngine,
    root: tempfile::TempDir,
}

impl FakeHarness {
    fn new() -> Self {
        Self {
            engine: FakeEngine::new(),
            root: tempfile::tempdir().unwrap(),
        }
    }
}

impl Harness for FakeHarness {
    fn engine(&self) -> &dyn Engine {
        &self.engine
    }

    fn root(&self) -> &Path {
        self.root.path()
    }

    fn firmware(&self) -> FirmwareSet {
        let boot_prom = self.root().join("prom.bin");
        std::fs::write(&boot_prom, [0u8; 16]).unwrap();
        FirmwareSet {
            boot_prom,
            io_prom: None,
        }
    }

    fn offering(&self) -> String {
        "fake.1".into()
    }

    fn wiring(&self, name: &str) -> Wiring {
        Wiring {
            display: DisplayRequest::Shared { gpu: None },
            console_port: 0,
            control_port: 0,
            name: name.into(),
            paused: false,
            shares: None,
            once: vec![],
        }
    }

    fn launch_crashing(
        &self,
        plan: LaunchPlan,
        wake: Wake,
    ) -> origami_engine::Result<Arc<dyn Session>> {
        let dir = plan.dir().to_path_buf();
        let session = self.engine.launch(plan, wake)?;
        self.engine
            .session_in(&dir)
            .unwrap()
            .crash(3, "the fake guest crashed");
        Ok(session)
    }

    fn controls_seen(&self, dir: &MachineDir) -> Vec<Control> {
        self.engine.session_in(dir.path()).unwrap().controls()
    }

    fn media_seen(&self, dir: &MachineDir) -> Vec<(String, Option<PathBuf>)> {
        self.engine.session_in(dir.path()).unwrap().media_changes()
    }
}

#[test]
fn the_fake_engine_meets_the_contract() {
    contract::run(&FakeHarness::new());
}

#[test]
fn the_fake_pictures_change_when_told_to() {
    let h = FakeHarness::new();
    let dir = h.root().join("m");
    std::fs::create_dir_all(&dir).unwrap();
    let wiring = h.wiring("frames");
    let offering = &h.engine.catalogue().unwrap().offerings[2];
    let machine = origami_machine::model::Machine::from_offering(
        offering,
        None,
        None,
        vec![],
        Default::default(),
    );
    let mut machine = machine;
    machine.engine.id = "fake".into();
    let plan = h
        .engine
        .plan(&machine, &MachineDir::new(&dir), &wiring)
        .unwrap();
    let (wake, woken) = {
        let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = count.clone();
        let wake: Wake = Arc::new(move |_| {
            seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        });
        (wake, count)
    };
    let session = h.engine.launch(plan, wake).unwrap();
    let DisplayEndpoint::Frames(source) = session.display() else {
        panic!("the fake session has in-process frames");
    };
    assert_eq!(source.heads().len(), 2);
    let mut pixels = Vec::new();
    let Some(Frame::Rows(first)) = source.take_frame(0, &mut pixels) else {
        panic!("a new head has a frame");
    };
    assert!(first.resized);
    assert_eq!(pixels.len(), (first.width * first.height * 4) as usize);
    assert!(source.take_frame(0, &mut pixels).is_none());
    let fake = h.engine.session_in(&dir).unwrap();
    fake.advance_frame();
    assert_eq!(woken.load(std::sync::atomic::Ordering::SeqCst), 1);
    let Some(Frame::Rows(next)) = source.take_frame(0, &mut pixels) else {
        panic!("an advanced head has a frame");
    };
    assert!(next.seq > first.seq && !next.resized);
}

#[test]
fn the_fake_console_prints_a_banner_then_echoes() {
    let h = FakeHarness::new();
    let dir = h.root().join("m");
    std::fs::create_dir_all(&dir).unwrap();
    let offering = &h.engine.catalogue().unwrap().offerings[0];
    let mut machine = origami_machine::model::Machine::from_offering(
        offering,
        None,
        None,
        vec![],
        Default::default(),
    );
    machine.engine.id = "fake".into();
    let plan = h
        .engine
        .plan(&machine, &MachineDir::new(&dir), &h.wiring("console"))
        .unwrap();
    let session = h.engine.launch(plan, Arc::new(|_| {})).unwrap();
    let mut console = session.open_console("console").unwrap();
    let mut banner = [0u8; 64];
    let count = console.read(&mut banner).unwrap();
    assert!(banner[..count].starts_with(b"fake console"));
    console.write_all(b"root\r").unwrap();
    let mut echo = [0u8; 16];
    let count = console.read(&mut echo).unwrap();
    assert_eq!(&echo[..count], b"root\r");
    h.engine.session_in(&dir).unwrap().crash(1, "gone");
    assert_eq!(console.read(&mut echo).unwrap(), 0);
    assert!(matches!(
        session.status().state,
        RunState::Exited { code: Some(1), .. }
    ));
}
