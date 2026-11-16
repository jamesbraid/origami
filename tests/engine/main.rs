//! The QEMU adapter against the engine contract suite, with the fake QEMU
//! (`tests/bin/fake.rs`) standing in for the emulator.

#[path = "../support/mod.rs"]
mod support;

use origami_engine::contract::{self, Harness};
use origami_engine::{Control, DisplayRequest, Engine, LaunchPlan, Session, Wake, Wiring};
use origami_machine::control::free_port;
use origami_machine::dir::MachineDir;
use origami_machine::stores::FirmwareSet;
use origami_qemu::{QemuEngine, Runtime};
use std::path::{Path, PathBuf};
use std::sync::Arc;

struct QemuHarness {
    engine: QemuEngine,
    root: tempfile::TempDir,
    offering: String,
}

impl QemuHarness {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        std::env::set_var("ORIGAMI_RUNTIME_DIR", support::fake_runtime(root.path()));
        let engine = QemuEngine::new(Runtime::locate().unwrap());
        let catalogue = engine.catalogue().unwrap();
        let offering = origami_machine::preset(&catalogue, "origin200-1")
            .unwrap()
            .id
            .clone();
        Self {
            engine,
            root,
            offering,
        }
    }

    /// The lines the fake QEMU logged for its last run in `dir`.
    fn qmp_lines(&self, dir: &MachineDir) -> Vec<String> {
        let log = support::fake_log(dir.path());
        let start = log
            .iter()
            .rposition(|l| l.starts_with("argv "))
            .unwrap_or(0);
        log[start..].to_vec()
    }
}

impl Harness for QemuHarness {
    fn engine(&self) -> &dyn Engine {
        &self.engine
    }

    fn root(&self) -> &Path {
        self.root.path()
    }

    fn firmware(&self) -> FirmwareSet {
        FirmwareSet {
            boot_prom: support::synthetic_prom(self.root()),
            io_prom: None,
        }
    }

    fn offering(&self) -> String {
        self.offering.clone()
    }

    fn wiring(&self, name: &str) -> Wiring {
        Wiring {
            display: DisplayRequest::None,
            console_port: free_port().unwrap(),
            control_port: free_port().unwrap(),
            name: format!("sgi-contract-{}-{name}", std::process::id()),
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
        // The fake reads this when it starts; the suite runs in one test, so
        // nothing else launches meanwhile.
        std::env::set_var("ORIGAMI_FAKE", "crash-after=1");
        let session = self.engine.launch(plan, wake);
        std::env::remove_var("ORIGAMI_FAKE");
        session
    }

    fn controls_seen(&self, dir: &MachineDir) -> Vec<Control> {
        self.qmp_lines(dir)
            .iter()
            .filter_map(|line| match line.strip_prefix("qmp ")? {
                "system_reset" => Some(Control::Reset),
                "inject-nmi" => Some(Control::Nmi),
                "stop" => Some(Control::Pause),
                "cont" => Some(Control::Resume),
                "system_powerdown" => Some(Control::PowerButton),
                _ => None,
            })
            .collect()
    }

    fn media_seen(&self, dir: &MachineDir) -> Vec<(String, Option<PathBuf>)> {
        // The fixture's first drive is `disk` (drive0), its second `cd`.
        self.qmp_lines(dir)
            .iter()
            .filter_map(|line| {
                let (command, arguments) = line.strip_prefix("qmp-args ")?.split_once(' ')?;
                let arguments: serde_json::Value = serde_json::from_str(arguments).ok()?;
                let drive = match arguments["device"].as_str()? {
                    "drive0" => "disk",
                    "drive1" => "cd",
                    other => other,
                };
                let image = match command {
                    "blockdev-change-medium" => Some(arguments["filename"].as_str()?.into()),
                    _ => None,
                };
                Some((drive.to_string(), image))
            })
            .collect()
    }
}

#[test]
fn the_qemu_adapter_meets_the_contract() {
    contract::run(&QemuHarness::new());
}
