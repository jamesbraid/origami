//! Starting, watching and stopping a machine: the background runner and the
//! fake QEMU it supervises, including the ways QEMU misbehaves.

use crate::support::{self, create, fail, fake_argv, fake_log, ok, run, wait_until, Running};
use std::fs;
use std::time::{Duration, Instant};

const START: &[&str] = &["--background", "--display", "none"];

fn start(root: &std::path::Path, machine: &std::path::Path) -> String {
    let mut args = vec!["run", machine.to_str().unwrap()];
    args.extend(START);
    ok(root, &args)
}

#[test]
fn a_machine_runs_in_the_background_and_stops() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    let machine = create(root, "machine", "origin200-1", &[]);
    let path = machine.to_str().unwrap();
    assert_eq!(ok(root, &["status", path]).trim(), "stopped");

    let _guard = Running::new(root, &machine);
    let started = start(root, &machine);
    let pid = started
        .trim()
        .strip_prefix("running: pid ")
        .unwrap_or_else(|| panic!("unexpected output {started:?}"));
    assert_eq!(
        ok(root, &["status", path]).trim(),
        format!("running: pid {pid}")
    );

    // The runner gave QEMU the control endpoints the product relies on.
    let argv = fake_argv(&machine);
    assert!(
        argv.iter().any(|arg| arg.starts_with("tcp:127.0.0.1:")),
        "{argv:?}"
    );
    assert!(support::has_option(&argv, "-display", "none"), "{argv:?}");
    assert!(
        argv.iter()
            .any(|arg| arg.starts_with("socket,id=origami-console,host=127.0.0.1,port=")),
        "{argv:?}"
    );

    assert_eq!(ok(root, &["stop", path]).trim(), "stopped");
    assert_eq!(ok(root, &["status", path]).trim(), "stopped");
    assert!(fake_log(&machine).contains(&"exit".to_string()));
    assert!(!machine.join("state/run.json").exists());
}

#[test]
fn a_running_machine_cannot_be_started_again() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    let machine = create(root, "machine", "origin200-1", &[]);
    let _guard = Running::new(root, &machine);
    start(root, &machine);
    let path = machine.to_str().unwrap();
    let error = fail(root, &["run", path, "--background", "--display", "none"]);
    assert!(error.contains("already running"), "{error}");
    // Nor can it be edited under QEMU.
    let error = fail(root, &["network-set", path, "--mode", "none"]);
    assert!(error.contains("stop the machine"), "{error}");
}

/// QEMU takes a quit and keeps the machine for a while before it exits, as
/// it does while it folds a CHD disk's changes into the image. `stop` has to
/// outwait it, and the machine is not stopped until it has.
#[test]
fn stop_waits_for_qemu_to_finish_folding() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    let machine = create(root, "machine", "origin200-1", &[]);
    let path = machine.to_str().unwrap();
    let _guard = Running::new(root, &machine);
    let output = support::origami(root)
        .env("ORIGAMI_FAKE", "fold=2500")
        .args(["run", path, "--background", "--display", "none"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let asked = Instant::now();
    let output = support::origami(root)
        .args(["stop", path])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(
        asked.elapsed() >= Duration::from_millis(2500),
        "{:?}",
        asked.elapsed()
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("waiting for QEMU to exit"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    // It is stopped now, not stopping.
    assert_eq!(ok(root, &["status", path]).trim(), "stopped");
    assert!(fake_log(&machine).contains(&"exit".to_string()));
}

#[test]
fn qemu_that_exits_before_it_is_ready_is_reported_with_its_message() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    let machine = create(root, "machine", "origin200-1", &[]);
    let path = machine.to_str().unwrap();
    let output = support::origami(root)
        .env("ORIGAMI_FAKE", "exit-early")
        .args(["run", path, "--background", "--display", "none"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("machine failed to start"), "{error}");
    assert!(
        error.contains("QEMU exited before QMP was ready"),
        "{error}"
    );
    assert!(
        error.contains("full log:") && error.contains("runner.log"),
        "{error}"
    );
    assert_eq!(ok(root, &["status", path]).trim(), "stopped");
    // The failure left nothing behind that blocks the next start.
    start(root, &machine);
    let _guard = Running::new(root, &machine);
}

#[test]
fn qemu_that_never_opens_qmp_is_killed_and_reported() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    let machine = create(root, "machine", "origin200-1", &[]);
    let path = machine.to_str().unwrap();
    let output = support::origami(root)
        .env("ORIGAMI_FAKE", "no-qmp")
        .args(["run", path, "--background", "--display", "none"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(
        error.contains("did not open its QMP control endpoint"),
        "{error}"
    );
    assert!(!machine.join("state/run.json").exists());
    // The supervisor let go of the machine.
    assert_eq!(ok(root, &["status", path]).trim(), "stopped");
}

#[test]
fn qemu_that_accepts_qmp_but_never_answers_is_not_a_started_machine() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    let machine = create(root, "machine", "origin200-1", &[]);
    let path = machine.to_str().unwrap();
    let output = support::origami(root)
        // The fake gives up soon after the product does.
        .env("ORIGAMI_FAKE", "hang,lifetime=40")
        .args(["run", path, "--background", "--display", "none"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(
        error.contains("did not open its QMP control endpoint"),
        "{error}"
    );
    assert_eq!(ok(root, &["status", path]).trim(), "stopped");
}

#[test]
fn a_machine_whose_qemu_crashes_is_stopped_and_can_start_again() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    let machine = create(root, "machine", "origin200-1", &[]);
    let path = machine.to_str().unwrap();
    let output = support::origami(root)
        .env("ORIGAMI_FAKE", "crash-after=3")
        .args(["run", path, "--background", "--display", "none"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(ok(root, &["status", path]).starts_with("running"));
    wait_until(
        "the crashed machine to read as stopped",
        Duration::from_secs(20),
        || ok(root, &["status", path]).trim() == "stopped",
    );
    assert!(!machine.join("state/run.json").exists());
    let _guard = Running::new(root, &machine);
    start(root, &machine);
}

#[test]
fn a_run_record_left_by_a_dead_machine_is_not_a_running_machine() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    let machine = create(root, "machine", "origin200-1", &[]);
    let path = machine.to_str().unwrap();
    // A port nothing listens on.
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    fs::create_dir_all(machine.join("state")).unwrap();
    fs::write(
        machine.join("state/run.json"),
        format!(
            r#"{{"pid": 2147483000, "qmp_port": {port}, "console_port": {port}, "name": "gone"}}"#
        ),
    )
    .unwrap();
    assert_eq!(ok(root, &["status", path]).trim(), "stopped");
    let error = fail(root, &["stop", path]);
    assert!(!error.is_empty());
    // The stale record does not stop a start from replacing it.
    let _guard = Running::new(root, &machine);
    start(root, &machine);
    assert!(ok(root, &["status", path]).starts_with("running"));
}

#[test]
fn the_console_reaches_the_guest() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    let machine = create(root, "machine", "origin200-1", &[]);
    let _guard = Running::new(root, &machine);
    start(root, &machine);
    let mut console = origami::console::Console::connect(&machine).unwrap();
    let mut seen = Vec::new();
    let timeout = Duration::from_secs(10);
    console
        .wait_for(b"console login: ", timeout, &mut seen)
        .unwrap();
    console.command("root", b"# ", timeout, &mut seen).unwrap();
    let uname = console
        .command("uname -m", b"# ", timeout, &mut seen)
        .unwrap();
    assert!(uname.contains("IP27"), "{uname}");
    let hinv = console.command("hinv", b"# ", timeout, &mut seen).unwrap();
    assert!(hinv.contains("Main memory size"), "{hinv}");
    assert!(String::from_utf8_lossy(&seen).contains("The system is coming up."));
}

#[test]
fn the_machine_list_and_version_come_from_the_runtime() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    let machines = ok(root, &["machines"]);
    assert!(machines.contains("origin200-1"), "{machines}");
    let version = ok(root, &["--version"]);
    assert!(version.contains("origami"), "{version}");
    let version = String::from_utf8_lossy(&run(root, &["version"]).stdout).into_owned();
    assert!(version.contains("qemu 0.0.0 (origami fake)"), "{version}");
    assert!(
        version.contains("instigator 0.0.0 (origami fake)"),
        "{version}"
    );
}

#[test]
fn show_command_points_qemu_at_its_data_directory() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    let machine = create(root, "machine", "origin200-1", &[]);
    let shown = ok(root, &["show-command", machine.to_str().unwrap()]);
    assert!(shown.contains(" -L "), "{shown}");
    assert!(shown.contains("share/origami/qemu"), "{shown}");
}
