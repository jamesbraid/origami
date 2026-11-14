//! The install driver on scripted guests: the product connects to the
//! console of a running machine and walks inst through an install.

use crate::support::{self, create, fail, fake_log, ok, wait_until, Running};
use std::fs;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

/// An Origin 200 with install media configured, running a guest that plays
/// `guest`, a built-in script name or the path of one.
fn installing(root: &Path, guest: &str) -> (std::path::PathBuf, Running) {
    let machine = create(root, "machine", "origin200-1", &[]);
    let path = machine.to_str().unwrap();
    ok(root, &["install-init", path, "--profile", "minimal"]);
    let setting = if Path::new(guest).is_absolute() {
        format!("guest-file={guest}")
    } else {
        format!("guest={guest}")
    };
    let output = support::origami(root)
        .env("ORIGAMI_FAKE", setting)
        .args(["run", path, "--background", "--display", "none"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let guard = Running::new(root, &machine);
    (machine, guard)
}

#[test]
fn install_apply_runs_a_recorded_install_to_the_login_prompt() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    let (machine, _guard) = installing(root, "install-6.5.30-minimal");
    let output = ok(root, &["install-apply", machine.to_str().unwrap()]);
    assert!(output.contains("Load the package script"), "{output}");
    assert!(
        output.contains("instigator: post-install commands finished"),
        "{output}"
    );
    assert!(output.contains("base console login: "), "{output}");
    assert!(
        output.contains("Installed IRIX reached serial login"),
        "{output}"
    );
    let log = fake_log(&machine);
    assert!(log.contains(&"script finished".to_string()), "{log:?}");
}

#[test]
fn install_apply_stops_at_unresolved_conflicts() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    let script = root.join("conflicts.txt");
    fs::write(
        &script,
        "out Inst>\\x20\nin \\n\nin admin source\nin \\n\nout ERROR: Conflicts must be resolved.\\r\\n\nout Inst>\\x20\n",
    )
    .unwrap();
    let (machine, _guard) = installing(root, script.to_str().unwrap());
    let error = fail(root, &["install-apply", machine.to_str().unwrap()]);
    assert!(
        error.contains("unresolved installation conflicts"),
        "{error}"
    );
}

#[test]
fn install_apply_needs_a_running_machine() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    let machine = create(root, "machine", "origin200-1", &[]);
    let path = machine.to_str().unwrap();
    ok(root, &["install-init", path, "--profile", "minimal"]);
    let error = fail(root, &["install-apply", path]);
    assert!(error.contains("require a background run"), "{error}");
}

#[test]
fn install_check_runs_the_bundled_instigator() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    let machine = create(root, "machine", "origin200-1", &[]);
    let path = machine.to_str().unwrap();
    ok(root, &["install-init", path, "--profile", "minimal"]);
    assert!(ok(root, &["install-check", path]).contains("install media validated"));
    let output = support::origami(root)
        .env("ORIGAMI_FAKE", "check-fails")
        .args(["install-check", path])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("Instigator rejected install media"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// `install-serve` is what the guest's private network connects to: the
/// machine's QEMU dials the port Instigator listens on.
#[test]
fn the_guest_network_reaches_install_serve() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    let machine = create(root, "machine", "origin200-1", &[]);
    let path = machine.to_str().unwrap();
    ok(root, &["install-init", path, "--profile", "minimal"]);
    let _serve = support::KillOnDrop(
        support::origami(root)
            .env("ORIGAMI_FAKE", "lifetime=60")
            .args(["install-serve", path])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let logs = machine.join("install");
    let served = || {
        fs::read_dir(&logs)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("instigator-")
            })
            .any(|entry| {
                fs::read_to_string(entry.path().join("server.log"))
                    .is_ok_and(|log| log.contains("serving"))
            })
    };
    wait_until("Instigator to listen", Duration::from_secs(20), served);
    let _guard = Running::new(root, &machine);
    ok(root, &["run", path, "--background", "--display", "none"]);
    wait_until(
        "QEMU's network to reach Instigator",
        Duration::from_secs(20),
        || fake_log(&machine).contains(&"netdev net0 connected".to_string()),
    );
    let log = fs::read_dir(&logs)
        .unwrap()
        .flatten()
        .find_map(|entry| fs::read_to_string(entry.path().join("server.log")).ok())
        .unwrap();
    assert!(log.contains("peer connected"), "{log}");
}
