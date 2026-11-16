//! Guest audio: a run asks QEMU which backends it has and passes the host's
//! preferred one; without one the guest is silent and the log says why.

use crate::support::{self, create, fake_argv, has_option, ok, Running};
use std::fs;
use std::path::Path;

fn preferred() -> &'static str {
    if cfg!(target_os = "macos") {
        "coreaudio"
    } else if cfg!(windows) {
        "wasapi"
    } else {
        "pipewire"
    }
}

/// Start a machine with `fake` in ORIGAMI_FAKE and the extra run arguments,
/// and return its fake QEMU's argv and the runner's log.
fn run(root: &Path, fake: &str, extra: &[&str]) -> (Vec<String>, String) {
    let machine = create(root, "machine", "origin200-1", &[]);
    let _guard = Running::new(root, &machine);
    let output = support::origami(root)
        .env("ORIGAMI_FAKE", fake)
        .args(["run", machine.to_str().unwrap(), "--background"])
        .args(["--display", "none"])
        .args(extra)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let log = fs::read_to_string(machine.join("logs/runner.log")).unwrap_or_default();
    (fake_argv(&machine), log)
}

#[test]
fn audio_is_on_by_default_with_the_hosts_preferred_backend() {
    let root = tempfile::tempdir().unwrap();
    let (argv, log) = run(root.path(), "lifetime=60", &[]);
    assert!(
        has_option(&argv, "-audio", &format!("driver={}", preferred())),
        "{argv:?}"
    );
    assert!(!log.contains("guest audio is off"), "{log}");
}

#[test]
fn a_qemu_without_a_host_backend_runs_silent_and_says_so() {
    let root = tempfile::tempdir().unwrap();
    let (argv, log) = run(root.path(), "lifetime=60,audio-drivers=none+alsa", &[]);
    assert!(has_option(&argv, "-audio", "none"), "{argv:?}");
    assert!(log.contains("guest audio is off"), "{log}");
}

#[test]
fn no_audio_turns_it_off_without_a_note() {
    let root = tempfile::tempdir().unwrap();
    let (argv, log) = run(root.path(), "lifetime=60", &["--no-audio"]);
    assert!(has_option(&argv, "-audio", "none"), "{argv:?}");
    assert!(!log.contains("guest audio is off"), "{log}");
}

#[test]
fn show_command_leaves_the_host_backend_to_the_run() {
    let root = tempfile::tempdir().unwrap();
    let machine = create(root.path(), "machine", "origin200-1", &[]);
    let line = ok(root.path(), &["show-command", machine.to_str().unwrap()]);
    assert!(!line.contains("-audio"), "{line}");
}
