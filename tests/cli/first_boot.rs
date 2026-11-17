//! A headless machine's console setting waits for its PROM's first boot and
//! is applied by whichever command starts the machine next.

use crate::support::{self, ok, wait_until, KillOnDrop, Running};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

/// An Octane PROM's NVRAM after its first boot: console `g`.
const OCTANE_NVRAM: &[u8] = include_bytes!("../fixtures/nvram/octane-ip30prom-4.17.raw");

/// A headless Octane2 whose PROM has run, so its console setting is pending.
fn headless(root: &Path) -> PathBuf {
    let machine = support::create(root, "machine", "octane2-vpro", &["--graphics", "none"]);
    assert!(fs::read_to_string(machine.join("machine.toml"))
        .unwrap()
        .contains("[firstboot.nvram]"));
    fs::write(machine.join("state/nvram0.raw"), OCTANE_NVRAM).unwrap();
    machine
}

fn applied(machine: &Path) -> bool {
    !fs::read_to_string(machine.join("machine.toml"))
        .unwrap()
        .contains("firstboot")
}

fn console_after_stop(root: &Path, machine: &Path) -> String {
    ok(root, &["stop", machine.to_str().unwrap()]);
    wait_until("the machine to stop", Duration::from_secs(10), || {
        !origami_machine::control::is_locked(machine).unwrap()
    });
    ok(
        root,
        &["nvram", machine.to_str().unwrap(), "get", "console"],
    )
    .trim()
    .to_owned()
}

#[test]
fn run_applies_the_first_boot_console_before_it_starts_the_machine() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    let machine = headless(root);
    let path = machine.to_str().unwrap();
    let _guard = Running::new(root, &machine);
    let output = ok(root, &["run", path, "--background", "--display", "none"]);
    assert!(output.contains("console=d"), "{output}");
    assert!(applied(&machine));
    assert_eq!(console_after_stop(root, &machine), "console=d");
}

#[test]
fn the_runner_applies_it_too_when_started_directly() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    let machine = headless(root);
    let _guard = Running::new(root, &machine);
    let _runner = KillOnDrop(
        support::origami(root)
            .env("ORIGAMI_FAKE", "lifetime=60")
            .args(["_serve", machine.to_str().unwrap(), "--display", "none"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    wait_until(
        "the runner to record itself",
        Duration::from_secs(120),
        || machine.join("state/run.json").exists(),
    );
    assert!(applied(&machine));
    assert_eq!(console_after_stop(root, &machine), "console=d");
}
