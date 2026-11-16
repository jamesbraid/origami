//! The runner as the session host: it is the one client of QEMU's console,
//! serves the control socket and the legacy console port, and writes the
//! run record. The fake QEMU serves one console client at a time, as QEMU's
//! chardev does, so a test that two clients share the console is a test of
//! the runner.

use crate::support::{self, create, fake_argv, fake_log, ok, wait_until, KillOnDrop, Running};
use origami_machine::control;
use origami_protocol::{Client, ConsoleData, ErrorKind, Request};
use serde_json::Value;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::mpsc;
use std::time::Duration;

const START: &[&str] = &["--background", "--display", "none"];
const TIMEOUT: Duration = Duration::from_secs(10);

fn started(root: &Path, extra: &[&str]) -> (PathBuf, Running) {
    let machine = create(root, "machine", "origin200-1", &[]);
    let guard = Running::new(root, &machine);
    let mut args = vec!["run", machine.to_str().unwrap()];
    args.extend(START);
    args.extend(extra);
    ok(root, &args);
    (machine, guard)
}

fn record(machine: &Path) -> control::Record {
    control::read(machine).unwrap()
}

fn client(machine: &Path) -> Client {
    let mut client = Client::connect(record(machine).control_port, TIMEOUT).unwrap();
    client.ping().unwrap();
    client
}

/// Read from `stream` until `text` has appeared, or fail.
fn read_until(stream: &mut impl Read, text: &str) -> String {
    let mut seen = Vec::new();
    let mut chunk = [0u8; 4096];
    let deadline = std::time::Instant::now() + TIMEOUT;
    while !String::from_utf8_lossy(&seen).contains(text) {
        assert!(
            std::time::Instant::now() < deadline,
            "{text:?} did not appear in {:?}",
            String::from_utf8_lossy(&seen)
        );
        match stream.read(&mut chunk) {
            Ok(0) => panic!(
                "closed before {text:?}: {:?}",
                String::from_utf8_lossy(&seen)
            ),
            Ok(n) => seen.extend_from_slice(&chunk[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(e) => panic!("{e}"),
        }
    }
    String::from_utf8_lossy(&seen).into_owned()
}

/// A line-reading thread over a child's output, so a test can wait for it.
fn lines_of(mut stream: impl Read + Send + 'static) -> mpsc::Receiver<String> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut chunk = [0u8; 1024];
        while let Ok(n) = stream.read(&mut chunk) {
            if n == 0
                || tx
                    .send(String::from_utf8_lossy(&chunk[..n]).into_owned())
                    .is_err()
            {
                break;
            }
        }
    });
    rx
}

fn wait_for_text(rx: &mpsc::Receiver<String>, text: &str) -> String {
    let mut seen = String::new();
    let deadline = std::time::Instant::now() + TIMEOUT;
    while !seen.contains(text) {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        match rx.recv_timeout(left) {
            Ok(more) => seen.push_str(&more),
            Err(_) => panic!("{text:?} did not appear in {seen:?}"),
        }
    }
    seen
}

#[test]
fn the_run_record_names_every_port_and_who_started_the_machine() {
    let root = tempfile::tempdir().unwrap();
    let (machine, _guard) = started(root.path(), &[]);
    let text = std::fs::read_to_string(machine.join("state/run.json")).unwrap();
    let json: Value = serde_json::from_str(&text).unwrap();
    for key in [
        "pid",
        "name",
        "qmp_port",
        "console_port",
        "control_port",
        "engine_console",
        "qemu_pid",
        "started_by",
    ] {
        assert!(json.get(key).is_some(), "{key} missing from {text}");
    }
    assert_eq!(json["started_by"], "cli");
    let r = record(&machine);
    let mut ports = [r.qmp_port, r.console_port, r.control_port, r.engine_console];
    assert!(ports.iter().all(|port| *port != 0));
    ports.sort();
    assert!(ports.windows(2).all(|pair| pair[0] != pair[1]), "{ports:?}");
    assert_ne!(r.pid, r.qemu_pid);
    // QEMU, not the runner, listens on the QMP port and the console chardev.
    let argv = fake_argv(&machine);
    assert!(argv.contains(&format!("tcp:127.0.0.1:{},server=on,wait=off", r.qmp_port)));
    assert!(argv
        .iter()
        .any(|arg| arg.contains(&format!("port={},server=on", r.engine_console))));
    // An older origami's reader sees the fields it knew.
    let old: Value = serde_json::from_str(&text).unwrap();
    assert!(old["console_port"].is_u64() && old["qmp_port"].is_u64());
}

#[test]
fn a_second_console_client_does_not_hang() {
    let root = tempfile::tempdir().unwrap();
    let (machine, _guard) = started(root.path(), &[]);
    let r = record(&machine);
    let mut first = support::origami(root.path())
        .args(["console", machine.to_str().unwrap()])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map(KillOnDrop)
        .unwrap();
    let out = lines_of(first.0.stdout.take().unwrap());
    // The first client is in, so QEMU's chardev is taken: a direct
    // connection now would wait in the backlog and hear nothing.
    wait_for_text(&out, "console login: ");
    let mut direct = TcpStream::connect(("127.0.0.1", r.engine_console)).unwrap();
    direct
        .set_read_timeout(Some(Duration::from_millis(500)))
        .unwrap();
    let mut buf = [0u8; 64];
    assert!(
        direct.read(&mut buf).is_err(),
        "the fake served two clients"
    );
    drop(direct);
    // Through the runner a second client works, and reaches the guest.
    let mut second = origami_qemu::connect_console(&machine).unwrap();
    let mut seen = Vec::new();
    second
        .command("root", b"# ", TIMEOUT, &mut seen)
        .expect("the second client got no answer");
    // The first client sees the same answer.
    wait_for_text(&out, "# ");
}

#[cfg(unix)]
#[test]
fn stop_falls_back_to_qmp_when_the_runner_is_killed() {
    let root = tempfile::tempdir().unwrap();
    let (machine, _guard) = started(root.path(), &[]);
    let r = record(&machine);
    // SAFETY: SIGKILL to the runner this test started.
    unsafe { libc::kill(r.pid as libc::pid_t, libc::SIGKILL) };
    wait_until("the runner to die", TIMEOUT, || {
        !control::is_locked(&machine).unwrap()
    });
    let path = machine.to_str().unwrap();
    assert_eq!(
        ok(root.path(), &["status", path]).trim(),
        format!("runner gone, QEMU pid {} still running", r.qemu_pid)
    );
    assert_eq!(ok(root.path(), &["stop", path]).trim(), "stopped");
    wait_until("QEMU to exit", TIMEOUT, || {
        fake_log(&machine).contains(&"exit".to_string())
    });
    assert!(!machine.join("state/run.json").exists());
}

#[cfg(unix)]
#[test]
fn a_runnerless_console_reconnects_to_qemu_with_a_warning() {
    let root = tempfile::tempdir().unwrap();
    let (machine, _guard) = started(root.path(), &[]);
    let r = record(&machine);
    // SAFETY: SIGKILL to the runner this test started.
    unsafe { libc::kill(r.pid as libc::pid_t, libc::SIGKILL) };
    wait_until("the runner to die", TIMEOUT, || {
        !control::is_locked(&machine).unwrap()
    });
    let mut console = support::origami(root.path())
        .args(["console", machine.to_str().unwrap()])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map(KillOnDrop)
        .unwrap();
    let out = lines_of(console.0.stdout.take().unwrap());
    let err = lines_of(console.0.stderr.take().unwrap());
    wait_for_text(&err, "the runner for this machine is gone");
    wait_for_text(&out, "console login: ");
    // Stopping still works, over QMP.
    let path = machine.to_str().unwrap();
    drop(console);
    assert_eq!(ok(root.path(), &["stop", path]).trim(), "stopped");
}

#[test]
fn a_second_console_beside_a_running_install_does_not_hang() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    let machine = create(root, "machine", "origin200-1", &[]);
    let path = machine.to_str().unwrap();
    ok(root, &["install-init", path, "--profile", "minimal"]);
    // The recorded install, held back long enough to attach a second client.
    let script = root.join("slow-install.txt");
    std::fs::write(
        &script,
        format!(
            "wait 2500\n{}",
            include_str!("../fixtures/guests/install-6.5.30-minimal.txt")
        ),
    )
    .unwrap();
    let output = support::origami(root)
        .env("ORIGAMI_FAKE", format!("guest-file={}", script.display()))
        .args(["run", path])
        .args(START)
        .output()
        .unwrap();
    assert!(output.status.success());
    let _guard = Running::new(root, &machine);
    let mut install = support::origami(root)
        .args(["install-apply", path])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map(KillOnDrop)
        .unwrap();
    let install_out = lines_of(install.0.stdout.take().unwrap());
    let install_err = lines_of(install.0.stderr.take().unwrap());
    std::thread::sleep(Duration::from_millis(500));
    let mut console = support::origami(root)
        .args(["console", path])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map(KillOnDrop)
        .unwrap();
    let watching = lines_of(console.0.stdout.take().unwrap());
    // The second client sees what the guest says to the install.
    wait_for_text(&watching, "Inst 4.1 Main Menu");
    wait_for_text(&install_out, "Installed IRIX reached serial login");
    drop(install_err);
    assert!(install.0.wait().unwrap().success());
}

#[test]
fn the_control_socket_serves_every_verb_against_the_fake() {
    let root = tempfile::tempdir().unwrap();
    let (machine, _guard) = started(root.path(), &[]);
    let mut c = client(&machine);

    let ping = c.ping().unwrap();
    assert_eq!(ping.protocol, 1);
    assert_eq!(ping.engine, "qemu");
    assert_eq!(ping.verbs.len(), 20);

    let status = c.call_data(&Request::Status {}).unwrap();
    assert_eq!(status["state"], "running");
    assert_eq!(status["pid"], record(&machine).pid);

    let found = c
        .call_data_timeout(
            &Request::WaitSerial {
                pattern: "console login: ".into(),
                timeout_ms: 10_000,
                since: None,
            },
            TIMEOUT,
        )
        .unwrap();
    let after = found["offset"].as_u64().unwrap();
    c.call_data(&Request::SerialSend {
        data: "root".into(),
    })
    .unwrap();
    let read = c
        .call_data_timeout(
            &Request::WaitSerial {
                pattern: "# ".into(),
                timeout_ms: 10_000,
                since: Some(after),
            },
            TIMEOUT,
        )
        .unwrap();
    assert!(read["offset"].as_u64().unwrap() > after);
    let text = c
        .call_data(&Request::SerialRead {
            since: Some(after),
            max: None,
        })
        .unwrap();
    assert!(text["data"].as_str().unwrap().contains("# "), "{text}");

    let shot = root.path().join("shot.ppm");
    c.call_data(&Request::Screenshot {
        head: 0,
        path: shot.display().to_string(),
    })
    .unwrap();
    assert!(shot.exists());

    for request in [Request::Pause {}, Request::Resume {}, Request::Nmi {}] {
        c.call_data(&request).unwrap();
    }
    let reset = c.call_data(&Request::Reset {}).unwrap();
    assert!(reset["boot_offset"].as_u64().is_some());
    let log = fake_log(&machine);
    for command in ["qmp stop", "qmp inject-nmi", "qmp system_reset"] {
        assert!(log.contains(&command.to_string()), "{command}: {log:?}");
    }

    // A machine with no CD-ROM says so rather than guessing a drive.
    let eject = c.call(&Request::CdromEject { drive: None }).unwrap();
    assert!(!eject.ok);
    assert_eq!(eject.kind, Some(ErrorKind::Unsupported));

    // `cont` only means something for a machine started paused.
    let cont = c.call(&Request::Cont {}).unwrap();
    assert!(!cont.ok);

    // The fake powers off like a guest that shuts down when asked.
    c.call_data(&Request::PowerButton {}).unwrap();
    wait_until("the machine to stop", TIMEOUT, || {
        !machine.join("state/run.json").exists()
    });
}

#[test]
fn a_held_machine_waits_for_cont_and_the_install_reader_starts_at_its_boot() {
    let root = tempfile::tempdir().unwrap();
    let (machine, _guard) = started(root.path(), &["--hold"]);
    assert!(fake_argv(&machine).contains(&"-S".to_string()));
    let mut c = client(&machine);
    assert_eq!(
        c.call_data(&Request::Status {}).unwrap()["state"],
        "prelaunch"
    );
    // The installer takes the lease first, then lets the machine go.
    let (data, mut stream) = c.console("", true, None).expect("console with a lease");
    let data: ConsoleData = serde_json::from_value(data).unwrap();
    assert!(data.leased);
    let mut control = client(&machine);
    let cont = control.call_data(&Request::Cont {}).unwrap();
    let boot_offset = cont["boot_offset"].as_u64().unwrap();
    assert!(boot_offset >= data.start);
    assert_eq!(
        control.call_data(&Request::Status {}).unwrap()["state"],
        "running"
    );
    // The lease holder reads the boot from the offset `cont` returned.
    let (data, mut from_boot) = client(&machine)
        .console("", false, Some(boot_offset))
        .unwrap();
    let data: ConsoleData = serde_json::from_value(data).unwrap();
    assert_eq!(data.start, boot_offset);
    from_boot
        .set_read_timeout(Some(Duration::from_millis(200)))
        .unwrap();
    read_until(&mut from_boot, "console login: ");
    read_until(&mut stream, "console login: ");
    // Another connection's typing is dropped while the lease is held, and
    // the lease holder's is not.
    stream.write_all(b"root\n").unwrap();
    read_until(&mut stream, "# ");
}

#[test]
fn a_viewer_is_told_on_stderr_that_an_installer_holds_the_console() {
    let root = tempfile::tempdir().unwrap();
    let (machine, _guard) = started(root.path(), &[]);
    let mut viewer = support::origami(root.path())
        .args(["console", machine.to_str().unwrap()])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map(KillOnDrop)
        .unwrap();
    let err = lines_of(viewer.0.stderr.take().unwrap());
    let out = lines_of(viewer.0.stdout.take().unwrap());
    wait_for_text(&out, "console login: ");
    let mut holder = client(&machine);
    holder
        .call_data(&Request::Lease {
            line: String::new(),
        })
        .unwrap();
    viewer
        .0
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"root\n")
        .unwrap();
    let notice = wait_for_text(&err, "holds the console");
    assert!(notice.starts_with("origami: "), "{notice}");
    // The legacy port gets the same rule with no notice, and never hangs.
    let mut legacy = origami_qemu::connect_console(&machine).unwrap();
    legacy.send("root\n").unwrap();
}
