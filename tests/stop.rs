//! `origami stop` returns when the machine has exited, not when it was asked to.

use origami::control::{self, Record};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::time::{Duration, Instant};

const NAME: &str = "sgi-stop-test";

/// A QMP endpoint that answers like QEMU and, once told to quit, keeps the
/// machine lock for `fold` more before letting go, as a QEMU does while it
/// folds a CHD disk's changes into the image.
fn fake_machine(dir: &std::path::Path, fold: Duration) -> std::thread::JoinHandle<Instant> {
    let lock = control::lock_for_edit(dir, "test").unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let record = Record {
        pid: std::process::id(),
        qmp_port: listener.local_addr().unwrap().port(),
        console_port: 0,
        name: NAME.into(),
    };
    control::write(dir, &record).unwrap();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        writeln!(
            stream,
            r#"{{"QMP": {{"version": {{"qemu": {{"major": 0, "minor": 0, "micro": 0}}, "package": ""}}, "capabilities": []}}}}"#
        )
        .unwrap();
        for line in BufReader::new(stream.try_clone().unwrap()).lines() {
            let line = line.unwrap();
            if line.contains("query-name") {
                writeln!(stream, r#"{{"return": {{"name": "{NAME}"}}}}"#).unwrap();
            } else if line.contains("quit") {
                writeln!(stream, r#"{{"return": {{}}}}"#).unwrap();
                std::thread::sleep(fold);
                drop(lock);
                return Instant::now();
            } else {
                writeln!(stream, r#"{{"return": {{}}}}"#).unwrap();
            }
        }
        panic!("the client left without asking to quit");
    })
}

#[test]
fn stop_waits_for_a_slow_exit() {
    let root = tempfile::tempdir().unwrap();
    let fold = Duration::from_millis(2500);
    let machine = fake_machine(root.path(), fold);
    let started = Instant::now();
    control::stop(root.path()).unwrap();
    let returned = Instant::now();
    let released = machine.join().unwrap();
    assert!(
        returned >= released,
        "stop returned {:?} before the machine let go",
        released - returned
    );
    assert!(started.elapsed() >= fold, "stop returned during the fold");
    assert!(!control::is_locked(root.path()).unwrap());
}

#[test]
fn stop_of_a_machine_that_is_not_running_is_an_error() {
    let root = tempfile::tempdir().unwrap();
    assert!(control::stop(root.path()).is_err());
}

/// A stop that was interrupted leaves QEMU folding with its QMP endpoint no
/// longer answering. Stopping again must keep waiting, not report an error.
#[test]
fn a_second_stop_waits_while_qemu_no_longer_answers() {
    let root = tempfile::tempdir().unwrap();
    let lock = control::lock_for_edit(root.path(), "test").unwrap();
    let silent = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = silent.local_addr().unwrap().port();
    drop(silent);
    control::write(
        root.path(),
        &Record {
            pid: std::process::id(),
            qmp_port: port,
            console_port: 0,
            name: NAME.into(),
        },
    )
    .unwrap();
    let fold = Duration::from_millis(1500);
    let release = std::thread::spawn(move || {
        std::thread::sleep(fold);
        drop(lock);
    });
    let started = Instant::now();
    control::stop(root.path()).unwrap();
    assert!(started.elapsed() >= fold, "stop returned during the fold");
    release.join().unwrap();
}

#[test]
fn stop_reports_a_dead_machine_whose_lock_is_free() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("state")).unwrap();
    let silent = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = silent.local_addr().unwrap().port();
    drop(silent);
    control::write(
        root.path(),
        &Record {
            pid: std::process::id(),
            qmp_port: port,
            console_port: 0,
            name: NAME.into(),
        },
    )
    .unwrap();
    assert!(control::stop(root.path()).is_err());
}
