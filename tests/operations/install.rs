//! The install operations against a runner over the fake runtime: the
//! installer holds the console lease, a cancelled install lets go of it,
//! and the media server is a child the operation owns.

use crate::{engine, root, support};
use origami_core::facade::{
    Config, Core, ErrorKind, InstallFailure, InstallStart, Operation, OperationState,
};
use origami_machine::control;
use origami_protocol::{Client, ConsoleData, Event, Request};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::mpsc;
use std::time::Duration;
use support::{create, fake_argv, ok, wait_until, KillOnDrop, Running};

const TIMEOUT: Duration = Duration::from_secs(15);

fn core() -> Core {
    core_with(None)
}

/// A core whose install server is the program at `instigator`.
fn core_with(instigator: Option<PathBuf>) -> Core {
    let mut config = Config::new(root());
    config.instigator = instigator;
    Core::new(config, vec![std::sync::Arc::new(engine())], None).unwrap()
}

/// A configured Origin 200 under a fresh folder, not yet running.
fn machine(name: &str) -> PathBuf {
    let dir = root().join(name);
    fs::create_dir_all(&dir).unwrap();
    let machine = create(&dir, "machine", "origin200-1", &[]);
    ok(
        &dir,
        &[
            "install-init",
            machine.to_str().unwrap(),
            "--profile",
            "minimal",
        ],
    );
    machine
}

/// Start the machine in a background runner whose guest plays `guest`: a
/// built-in name, or a script's text.
fn run(machine: &Path, guest: &str, extra: &[&str]) -> Running {
    let dir = machine.parent().unwrap();
    let setting = if guest.contains('\n') {
        let path = dir.join("guest.txt");
        fs::write(&path, guest).unwrap();
        format!("guest-file={}", path.display())
    } else {
        format!("guest={guest}")
    };
    let output = support::origami(dir)
        .env("ORIGAMI_FAKE", setting)
        .args([
            "run",
            machine.to_str().unwrap(),
            "--background",
            "--display",
            "none",
        ])
        .args(extra)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    Running::new(dir, machine)
}

fn client(machine: &Path) -> Client {
    let mut client =
        Client::connect(control::read(machine).unwrap().control_port, TIMEOUT).unwrap();
    client.ping().unwrap();
    client
}

fn lease() -> Request {
    Request::Lease {
        line: String::new(),
    }
}

/// An installer that has typed its way to a command no guest answers.
const STALLS: &str = "outln Inst 4.1 Main Menu\nin \\n\nout \\r\\nInst>\\x20\nin admin source\nin \\n\nout Reading product descriptions ..\\r\\n\nin never-sent\n";

/// The recorded install, started late enough for a viewer to join before
/// it speaks and paused long enough mid-way to type at it.
fn slow_install() -> String {
    let script = include_str!("../fixtures/guests/install-6.5.30-minimal.txt");
    format!(
        "wait 2500\n{}",
        script.replace("in admin source", "wait 3000\nin admin source")
    )
}

/// Wait until the install has the console: it reports its first line only
/// once connected, and polling the lease would take it.
fn wait_for_lease_held(install: &Operation) {
    wait_until("the installer to take the lease", TIMEOUT, || {
        !install.progress().is_empty()
    });
}

#[test]
fn an_install_holds_the_lease_and_a_viewer_beside_it_is_read_only() {
    let machine = machine("lease");
    let _running = run(&machine, &slow_install(), &[]);
    let key = machine.to_str().unwrap();
    let core = core();
    let install = core.install_apply(key, InstallStart::Attached).unwrap();
    wait_for_lease_held(&install);

    let mut viewer = support::origami(machine.parent().unwrap())
        .args(["console", key])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map(KillOnDrop)
        .unwrap();
    let (out_tx, out) = mpsc::channel();
    let (err_tx, err) = mpsc::channel();
    let mut stdout = viewer.0.stdout.take().unwrap();
    let mut stderr = viewer.0.stderr.take().unwrap();
    std::thread::spawn(move || pump(&mut stdout, &out_tx));
    std::thread::spawn(move || pump(&mut stderr, &err_tx));
    // The viewer is not hung: it sees the installer's conversation.
    wait_text(&out, "Inst 4.1 Main Menu");
    viewer.0.stdin.as_mut().unwrap().write_all(b"x\n").unwrap();
    wait_text(&err, "holds the console");

    assert_eq!(install.wait(Some(TIMEOUT)), OperationState::Succeeded);
    assert!(
        install
            .progress()
            .contains("Installed IRIX reached serial login"),
        "{}",
        install.progress()
    );
    // Finishing gives the lease back.
    wait_until("the lease to be free", TIMEOUT, || {
        client(&machine).call_data(&lease()).is_ok()
    });
}

/// The recorded install behind an `Inst>` prompt that is already showing.
fn at_prompt(then: &str) -> String {
    let script = include_str!("../fixtures/guests/install-6.5.30-minimal.txt");
    let after_menu = script.split_once("in \\n\n").unwrap().1;
    format!("out Inst>\\x20\nin \\n\n{after_menu}{then}")
}

#[test]
fn an_automated_install_logs_in_and_halts_the_guest() {
    let machine = machine("halt");
    let core = core();
    let script = at_prompt(
        "in root\\n\nout TERM = (vt100)\\x20\nin \\n\nout #\\x20\nin halt -p\\n\npoweroff\n",
    );
    let _running = run(&machine, &script, &[]);
    let install = core.install_apply_from(machine.to_str().unwrap()).unwrap();
    // The guest only powers off when it was sent root and halt -p.
    assert_eq!(install.wait(Some(TIMEOUT)), OperationState::Succeeded);
    assert_eq!(install.install_failure(), None);
}

#[test]
fn an_automated_install_that_stops_at_a_conflict_leaves_the_guest_running() {
    let machine = machine("conflict");
    let core = core();
    let script = "out Inst>\\x20\nin \\n\nin admin source\nin \\n\nout ERROR: Conflicts must be resolved.\\r\\n\nout Inst>\\x20\n";
    let _running = run(&machine, script, &[]);
    let install = core.install_apply_from(machine.to_str().unwrap()).unwrap();
    let OperationState::Failed(error) = install.wait(Some(TIMEOUT)) else {
        panic!("the install should fail")
    };
    assert!(
        error.message.contains("unresolved installation conflicts"),
        "{error}"
    );
    assert_eq!(install.install_failure(), Some(InstallFailure::Conflict));
    let status = client(&machine).call_data(&Request::Status {}).unwrap();
    assert_eq!(status["state"], "running");
}

#[test]
fn cancelling_while_the_machine_powers_off_is_a_cancel_not_a_success() {
    let machine = machine("halt-cancel");
    let core = core();
    // The guest takes halt -p and never powers off.
    let script =
        at_prompt("in root\\n\nout TERM = (vt100)\\x20\nin \\n\nout #\\x20\nin halt -p\\n\n");
    let _running = run(&machine, &script, &[]);
    let install = core.install_apply_from(machine.to_str().unwrap()).unwrap();
    wait_until("halt to be typed", TIMEOUT, || {
        support::fake_log(&machine).contains(&"script finished".to_string())
    });
    assert!(install.cancel());
    assert_eq!(install.wait(Some(TIMEOUT)), OperationState::Cancelled);
    assert_eq!(install.install_failure(), Some(InstallFailure::Shutdown));
}

fn pump(from: &mut impl std::io::Read, to: &mpsc::Sender<String>) {
    let mut chunk = [0u8; 1024];
    while let Ok(n) = from.read(&mut chunk) {
        if n == 0
            || to
                .send(String::from_utf8_lossy(&chunk[..n]).into_owned())
                .is_err()
        {
            break;
        }
    }
}

fn wait_text(rx: &mpsc::Receiver<String>, text: &str) {
    let mut seen = String::new();
    let deadline = std::time::Instant::now() + TIMEOUT;
    while !seen.contains(text) {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        seen.push_str(
            &rx.recv_timeout(left)
                .unwrap_or_else(|_| panic!("{text:?} not in {seen:?}")),
        );
    }
}

#[test]
fn cancelling_an_install_releases_the_lease_and_leaves_the_machine_running() {
    let machine = machine("cancel");
    let _running = run(&machine, STALLS, &[]);
    let record = control::read(&machine).unwrap();
    let core = core();
    let install = core
        .install_apply(machine.to_str().unwrap(), InstallStart::Attached)
        .unwrap();
    wait_for_lease_held(&install);
    wait_until("the install to reach the guest", TIMEOUT, || {
        install.progress().contains("Reading product descriptions")
    });
    assert!(install.cancel());
    assert_eq!(install.wait(Some(TIMEOUT)), OperationState::Cancelled);
    wait_until("the lease to be free", TIMEOUT, || {
        client(&machine).call_data(&lease()).is_ok()
    });
    // The runner and QEMU are the ones it was.
    let status = client(&machine).call_data(&Request::Status {}).unwrap();
    assert_eq!(status["state"], "running");
    assert_eq!(control::read(&machine).unwrap().pid, record.pid);
    assert_eq!(status["qemu_pid"], record.qemu_pid);
}

#[test]
fn break_lease_takes_the_console_from_a_running_install() {
    let machine = machine("break");
    let _running = run(&machine, STALLS, &[]);
    let core = core();
    let install = core
        .install_apply(machine.to_str().unwrap(), InstallStart::Attached)
        .unwrap();
    wait_for_lease_held(&install);
    let mut other = client(&machine);
    assert!(other
        .call_data(&Request::BreakLease {
            line: String::new()
        })
        .unwrap()["broken"]
        .is_u64());
    other
        .call_data(&lease())
        .expect("the lease is free once broken");
    // Cancelling the install lets go of nothing it no longer holds.
    install.cancel();
    assert_eq!(install.wait(Some(TIMEOUT)), OperationState::Cancelled);
    assert!(
        client(&machine).call_data(&lease()).is_err(),
        "the new holder lost its lease"
    );
}

#[test]
fn a_held_machine_is_continued_by_the_install_and_read_from_its_boot() {
    let machine = machine("held");
    let _running = run(&machine, "install-6.5.30-minimal", &["--hold"]);
    assert!(fake_argv(&machine).contains(&"-S".to_string()));
    // The runner is up and paused; the starter has nothing left to do.
    let start = InstallStart::Held(Box::new(|_| Ok(())));
    let core = core();
    let install = core
        .install_apply(machine.to_str().unwrap(), start)
        .unwrap();
    assert_eq!(install.wait(Some(TIMEOUT)), OperationState::Succeeded);
    let status = client(&machine).call_data(&Request::Status {}).unwrap();
    assert_eq!(status["state"], "running");
    assert!(status["boot_offset"].as_u64().is_some());
}

#[test]
fn a_viewer_raw_stream_reports_who_holds_the_lease() {
    let machine = machine("notice");
    let _running = run(&machine, STALLS, &[]);
    let core = core();
    let install = core
        .install_apply(machine.to_str().unwrap(), InstallStart::Attached)
        .unwrap();
    wait_for_lease_held(&install);
    let mut watcher = client(&machine);
    watcher.call_data(&Request::Status {}).unwrap();
    let (data, mut viewer) = client(&machine).console("", false, None).unwrap();
    let data: ConsoleData = serde_json::from_value(data).unwrap();
    assert!(!data.leased);
    viewer.write_all(b"typed\n").unwrap();
    let notice = loop {
        match watcher.next_event(TIMEOUT).unwrap() {
            Some(Event::Notice { conn, text, .. }) if conn == data.conn => break text,
            Some(_) => {}
            None => panic!("no notice"),
        }
    };
    assert!(notice.contains("holds the console"), "{notice}");
    install.cancel();
    install.wait(Some(TIMEOUT));
}

#[test]
fn the_install_refuses_a_console_another_client_leases() {
    let machine = machine("refused");
    let _running = run(&machine, STALLS, &[]);
    let mut holder = client(&machine);
    holder.call_data(&lease()).unwrap();
    let core = core();
    let install = core
        .install_apply(machine.to_str().unwrap(), InstallStart::Attached)
        .unwrap();
    let OperationState::Failed(error) = install.wait(Some(TIMEOUT)) else {
        panic!("the install ran without the lease");
    };
    assert!(
        error.message.contains("another client holds the console"),
        "{error}"
    );
    holder
        .call_data(&lease())
        .expect("the holder kept its lease");
}

#[test]
fn an_install_needs_a_background_run() {
    let machine = machine("stopped");
    let core = core();
    let install = core
        .install_apply(machine.to_str().unwrap(), InstallStart::Attached)
        .unwrap();
    let OperationState::Failed(error) = install.wait(Some(TIMEOUT)) else {
        panic!("no machine, no install");
    };
    assert!(
        error.message.contains("require a background run"),
        "{error}"
    );
    assert_eq!(error.kind, ErrorKind::Invalid);
}

/// Whether process `pid` is gone; a zombie nobody reaped counts. macOS has no
/// /proc, which would read as gone for every process.
#[cfg(all(unix, not(target_os = "linux")))]
fn gone(pid: u32) -> bool {
    let out = std::process::Command::new("ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
        .unwrap();
    let stat = String::from_utf8_lossy(&out.stdout);
    stat.trim().is_empty() || stat.trim_start().starts_with('Z')
}

/// Whether process `pid` is gone; a zombie nobody reaped counts.
#[cfg(target_os = "linux")]
fn gone(pid: u32) -> bool {
    fs::read_to_string(format!("/proc/{pid}/stat")).map_or(true, |stat| {
        stat.rsplit(')')
            .next()
            .is_some_and(|rest| rest.trim_start().starts_with('Z'))
    })
}

/// An Instigator that starts a grandchild, as a real one starts its helpers.
#[cfg(unix)]
fn forking_instigator(dir: &Path) -> (PathBuf, PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    let pids = dir.join("grandchild.pid");
    let script = dir.join("instigator.sh");
    fs::write(
        &script,
        format!(
            "#!/bin/sh\nsleep 300 &\necho $! > {}\nwait\n",
            pids.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    (script, pids)
}

#[cfg(unix)]
fn grandchild(pids: &Path) -> u32 {
    wait_until("the server to start its helper", TIMEOUT, || {
        fs::read_to_string(pids).is_ok_and(|text| text.trim().parse::<u32>().is_ok())
    });
    fs::read_to_string(pids).unwrap().trim().parse().unwrap()
}

#[cfg(unix)]
#[test]
fn cancelling_the_media_server_ends_its_whole_process_group() {
    let machine = machine("serve-cancel");
    let (script, pids) = forking_instigator(machine.parent().unwrap());
    let core = core_with(Some(script));
    let serve = core.install_serve(machine.to_str().unwrap()).unwrap();
    let helper = grandchild(&pids);
    assert!(!gone(helper));
    assert!(serve.cancel());
    assert_eq!(serve.wait(Some(TIMEOUT)), OperationState::Cancelled);
    wait_until("the helper to die", TIMEOUT, || gone(helper));
}

#[cfg(unix)]
#[test]
fn dropping_the_core_ends_the_media_server() {
    let machine = machine("serve-drop");
    let (script, pids) = forking_instigator(machine.parent().unwrap());
    let core = core_with(Some(script));
    let serve = core.install_serve(machine.to_str().unwrap()).unwrap();
    let helper = grandchild(&pids);
    drop(core);
    assert_eq!(serve.state(), OperationState::Cancelled);
    wait_until("the helper to die", TIMEOUT, || gone(helper));
}

#[test]
fn the_media_server_and_the_check_report_how_instigator_ended() {
    let machine = machine("serve-ends");
    let dir = machine.parent().unwrap();
    let runtime = support::fake_runtime(dir);
    let instigator = runtime.join(if cfg!(windows) {
        "instigator.exe"
    } else {
        "instigator"
    });
    // `check` against the fake passes; the check operation says so.
    let core = core_with(Some(instigator));
    let check = core
        .install_check(machine.to_str().unwrap(), false)
        .unwrap();
    assert_eq!(check.wait(Some(TIMEOUT)), OperationState::Succeeded);
    assert!(
        check.progress().contains("Checking"),
        "{}",
        check.progress()
    );
    // A server that exits with an error is a failed operation.
    std::env::set_var("ORIGAMI_FAKE", "serve-fails");
    let serve = core.install_serve(machine.to_str().unwrap()).unwrap();
    let state = serve.wait(Some(TIMEOUT));
    std::env::remove_var("ORIGAMI_FAKE");
    assert!(
        serve.progress().ends_with("serving 6.5.30, Minimal"),
        "{}",
        serve.progress()
    );
    let OperationState::Failed(error) = state else {
        panic!("{state:?}")
    };
    assert!(error.message.contains("Instigator exited with"), "{error}");
}

#[cfg(unix)]
#[test]
fn a_second_server_and_a_setup_under_a_live_server_are_refused() {
    let machine = machine("serve-twice");
    let (script, _pids) = forking_instigator(machine.parent().unwrap());
    let core = core_with(Some(script));
    let key = machine.to_str().unwrap();
    let serve = core.install_serve(key).unwrap();
    let error = core.install_serve(key).err().unwrap();
    assert_eq!(error.kind, ErrorKind::Busy);
    let setup = origami_core::facade::InstallSetup {
        method: None,
        release: "6.5.30".into(),
        profile: "minimal".into(),
        media_root: None,
        addons: vec![],
    };
    assert_eq!(
        core.install_prepare(key, &setup).unwrap_err().kind,
        ErrorKind::Busy
    );
    serve.cancel();
    assert_eq!(serve.wait(Some(TIMEOUT)), OperationState::Cancelled);
}
