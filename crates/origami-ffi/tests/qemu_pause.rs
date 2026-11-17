//! Pause and Resume on a real QEMU Octane2 through the core, including a
//! second core attaching to the paused machine.
//!
//! Ignored by default: it starts a machine. Needs ORIGAMI_BIN (the `origami`
//! executable, with ORIGAMI_RUNTIME_DIR for QEMU) and ORIGAMI_GUI_TEST_DIR.
#![cfg(all(unix, feature = "qemu", not(feature = "fake")))]

use origami_ffi::{OrigamiControl, OrigamiCore, OrigamiMachine, OrigamiMachineState};
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

fn var(name: &str) -> PathBuf {
    std::env::var_os(name)
        .map(PathBuf::from)
        .unwrap_or_else(|| panic!("{name} is not set; see the header"))
}

fn wait_for(machine: &OrigamiMachine, want: OrigamiMachineState) {
    let started = Instant::now();
    while machine.view().state != want {
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "wanted {want:?}, still {:?}: {}",
            machine.view().state,
            machine.view().reason
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
#[ignore = "starts a machine; see the header"]
fn pause_and_resume_follow_qemu_and_survive_a_reconnect() {
    let bin = var("ORIGAMI_BIN");
    let work = var("ORIGAMI_GUI_TEST_DIR").join(format!("pause-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).unwrap();
    let made = Command::new(&bin)
        .arg("create")
        .arg(work.join("octane2"))
        .args(["--preset", "octane2-vpro"])
        .output()
        .unwrap();
    assert!(
        made.status.success(),
        "{}",
        String::from_utf8_lossy(&made.stderr)
    );
    let dir = work.join("octane2").canonicalize().unwrap();
    struct Stop(PathBuf, PathBuf);
    impl Drop for Stop {
        fn drop(&mut self) {
            let _ = Command::new(&self.0)
                .args(["stop", "--force"])
                .arg(&self.1)
                .status();
        }
    }
    let _stop = Stop(bin, dir.clone());
    let key = dir.to_string_lossy().into_owned();
    let root = work.to_string_lossy().into_owned();

    let core = OrigamiCore::new(root.clone(), None).unwrap();
    let machine = core.start_machine(key.clone()).unwrap();
    wait_for(&machine, OrigamiMachineState::Running);

    machine.control(OrigamiControl::Pause).unwrap();
    wait_for(&machine, OrigamiMachineState::Paused);

    // A new app attaching to the paused runner must see it paused.
    let again = OrigamiCore::new(root, None).unwrap();
    let reopened = again.start_machine(key).unwrap();
    wait_for(&reopened, OrigamiMachineState::Paused);

    reopened.control(OrigamiControl::Resume).unwrap();
    wait_for(&reopened, OrigamiMachineState::Running);
}
