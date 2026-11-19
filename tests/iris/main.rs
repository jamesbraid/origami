//! The iris engine against the engine contract suite, with the real emulator:
//! each launch starts this build's `origami _serve`, which needs the `iris`
//! feature.

use origami_engine::contract::{self, Harness};
use origami_engine::{Control, DisplayRequest, Engine, LaunchPlan, Session, Wake, Wiring};
use origami_iris::IrisEngine;
use origami_machine::control::free_port;
use origami_machine::dir::MachineDir;
use origami_machine::stores::FirmwareSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

struct IrisHarness {
    engine: IrisEngine,
    offering: &'static str,
    root: tempfile::TempDir,
}

impl Harness for IrisHarness {
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
        self.offering.into()
    }

    fn wiring(&self, name: &str) -> Wiring {
        Wiring {
            display: DisplayRequest::None,
            console_port: free_port().unwrap(),
            control_port: free_port().unwrap(),
            name: name.into(),
            paused: false,
            shares: None,
            // The suite reads the PROM's serial console.
            once: vec!["console=d".into()],
        }
    }

    // A fault in iris ends its runner; a signal is the one we can cause.
    fn launch_crashing(
        &self,
        plan: LaunchPlan,
        wake: Wake,
    ) -> origami_engine::Result<Arc<dyn Session>> {
        let session = self.engine.launch(plan, wake)?;
        let pid = session.status().pid.expect("a running runner has a pid");
        // SAFETY: kill(2) takes no pointers.
        assert_eq!(unsafe { libc::kill(pid as libc::pid_t, libc::SIGSEGV) }, 0);
        Ok(session)
    }

    fn controls_seen(&self, _: &MachineDir) -> Vec<Control> {
        unreachable!("the suite does not ask: observes_requests is false")
    }

    fn media_seen(&self, _: &MachineDir) -> Vec<(String, Option<PathBuf>)> {
        unreachable!("the suite does not ask: observes_requests is false")
    }

    fn expected_controls(&self) -> Vec<Control> {
        vec![Control::Reset, Control::Pause, Control::Resume]
    }

    fn crashes_by_signal(&self) -> bool {
        true
    }

    fn observes_requests(&self) -> bool {
        false
    }
}

fn contract_for(offering: &'static str) {
    contract::run(&IrisHarness {
        engine: IrisEngine::new(env!("CARGO_BIN_EXE_origami")),
        offering,
        root: tempfile::tempdir().unwrap(),
    });
}

#[test]
fn the_indy_meets_the_contract() {
    contract_for("indy.1");
}

#[test]
fn the_indigo2_meets_the_contract() {
    contract_for("indigo2.1");
}

#[test]
fn the_octane2_meets_the_contract() {
    contract_for("octane2.1.iris");
}

/// What a front end does: create a machine through the core, start it, watch
/// the PROM on its display, stop it.
#[test]
fn the_core_creates_starts_and_stops_an_iris_machine() {
    use origami_core::facade::{
        Config, Core, CreateRequest, MachineState, OperationState, StopKind,
    };

    let root = tempfile::tempdir().unwrap();
    let engine = IrisEngine::new(env!("CARGO_BIN_EXE_origami"));
    let core = Core::new(Config::new(root.path()), vec![Arc::new(engine)], None).unwrap();
    assert!(core.catalogue().unwrap().iter().any(|o| o.id == "indy.1"));
    let created = core
        .create_machine(CreateRequest {
            name: "indy".into(),
            offering: "indy.1".into(),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(created.wait(None), OperationState::Succeeded);

    let machine = core.start_machine("indy").unwrap();
    assert_eq!(machine.view().engine, "iris");
    assert_eq!(machine.view().state, MachineState::Running);
    #[cfg(target_os = "linux")]
    picture_arrives_over_display_protocol_4(root.path());

    assert_eq!(
        machine.stop(StopKind::Clean).unwrap().wait(None),
        OperationState::Succeeded
    );
    let view = machine.view();
    assert_eq!(view.state, MachineState::Exited);
    assert_eq!(view.exit_code, Some(0));
}

/// The runner serves the PROM's picture on the display protocol 4 socket in
/// the machine directory under `root`.
#[cfg(target_os = "linux")]
fn picture_arrives_over_display_protocol_4(root: &Path) {
    use origami_display::v4::session::{Event, HelloParams, Session};
    use std::time::Duration;

    fn find(dir: &Path) -> Option<PathBuf> {
        for entry in std::fs::read_dir(dir).ok()?.flatten() {
            let path = entry.path();
            if path.ends_with("state/display/v4.sock") {
                return Some(path);
            }
            if path.is_dir() {
                if let Some(found) = find(&path) {
                    return Some(found);
                }
            }
        }
        None
    }

    let socket = find(root).expect("the runner serves no display socket");
    let stream = std::os::unix::net::UnixStream::connect(socket).unwrap();
    let mut session = Session::connect(stream, &HelloParams::default()).unwrap();
    session.request(0, false).unwrap();
    // The PROM's console is its graphics head: its gradient is the first picture.
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    loop {
        if let Some(Event::Frame(frame)) = session.recv(Some(Duration::from_secs(1))).unwrap() {
            assert_eq!((frame.width, frame.height), (1282, 1024));
            let row = |y: usize| frame.pixels()[y * frame.stride as usize..][..16].to_vec();
            if row(0) != row(1000) {
                return;
            }
            drop(frame);
            session.request(0, false).unwrap();
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the PROM drew no gradient in 60 seconds"
        );
    }
}
