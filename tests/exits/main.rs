//! Each way a machine ends, as the apps' core sees it through the background
//! runner and as `origami status` reports it, against the fake QEMU. The fake
//! outlives its last QMP reply, as QEMU does while it folds a CHD disk.

#[path = "../support/mod.rs"]
mod support;

use origami_core::facade::{
    Config, Core, Event, ExitReason, Machine, MachineState, Observer, StopKind,
};
use origami_qemu::{RunnerEngine, Runtime};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::Command;
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

/// The guest powers itself off: QEMU quits with nobody on the host asking.
fn guest_powers_off(_: &Machine, dir: &Path) {
    let record = origami_machine::control::read(dir).unwrap();
    let stream = std::net::TcpStream::connect(("127.0.0.1", record.qmp_port)).unwrap();
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    for command in ["qmp_capabilities", "system_powerdown"] {
        writeln!(&stream, r#"{{"execute": "{command}"}}"#).unwrap();
        line.clear();
        reader.read_line(&mut line).unwrap();
    }
}

fn stop(machine: &Machine, _: &Path) {
    machine.stop(StopKind::Clean).unwrap().wait(None);
}

fn force(machine: &Machine, _: &Path) {
    machine.stop(StopKind::Force).unwrap().wait(None);
}

/// The emulator dies under the runner.
fn killed(_: &Machine, dir: &Path) {
    let pid = origami_machine::control::read(dir)
        .unwrap()
        .qemu_pid
        .to_string();
    let killed = if cfg!(windows) {
        Command::new("taskkill").args(["/F", "/PID", &pid]).status()
    } else {
        Command::new("kill").args(["-9", &pid]).status()
    };
    assert!(killed.unwrap().success());
}

type Ending = fn(&Machine, &Path);

/// The change notifications an app waits on.
struct Notified(Mutex<mpsc::Sender<Event>>);

impl Observer for Notified {
    fn on_event(&self, event: Event) {
        let _ = self.0.lock().unwrap().send(event);
    }
}

/// When the runner lets go of its record, which it does as it exits.
fn runner_exit(dir: &Path) -> std::thread::JoinHandle<Instant> {
    let record = origami_machine::control::path(dir);
    std::thread::spawn(move || {
        while record.exists() {
            std::thread::sleep(Duration::from_millis(5));
        }
        Instant::now()
    })
}

/// Wait on notifications alone, as an app does, until the view reads Exited.
fn notified_exit(machine: &Machine, events: &mpsc::Receiver<Event>) -> Instant {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        let event = events
            .recv_timeout(left)
            .expect("no notification of the exit");
        if event.machine.as_deref() == Some(machine.key())
            && machine.view().state == MachineState::Exited
        {
            return Instant::now();
        }
    }
}

#[test]
fn each_ending_reads_the_same_in_the_core_and_in_status() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    // One test, so the environment the runner inherits is set once.
    std::env::set_var("ORIGAMI_RUNTIME_DIR", support::fake_runtime(root));
    std::env::set_var("ORIGAMI_CACHE_DIR", root.join("cache"));
    // Long enough for the display thread to find QMP silent before QEMU exits.
    std::env::set_var("ORIGAMI_FAKE", "fold=6000");
    let engine = RunnerEngine::new(Runtime::locate().unwrap(), support::ORIGAMI.into());
    let (tx, events) = mpsc::channel();
    let observer = Arc::new(Notified(Mutex::new(tx)));
    let core = Core::new(Config::new(root), vec![Arc::new(engine)], Some(observer)).unwrap();
    let rows: [(&str, Ending, ExitReason, &str); 4] = [
        (
            "guest",
            guest_powers_off,
            ExitReason::GuestPoweredOff,
            "stopped (the guest powered off)",
        ),
        ("stop", stop, ExitReason::HostStopped, "stopped"),
        ("force", force, ExitReason::HostStopped, "stopped"),
        (
            "killed",
            killed,
            ExitReason::Crashed,
            "stopped unexpectedly (crashed",
        ),
    ];
    for (name, end, reason, status) in rows {
        let dir = support::create(root, name, "origin200-1", &[]);
        let _guard = support::Running::new(root, &dir);
        let machine = core.start_machine(dir.to_str().unwrap()).unwrap();
        let exited = runner_exit(&dir);
        while events.try_recv().is_ok() {}
        end(&machine, &dir);
        if name == "guest" {
            let shown = notified_exit(&machine, &events);
            let late = shown.saturating_duration_since(exited.join().unwrap());
            assert!(late < Duration::from_millis(500), "shown {late:?} late");
        }
        // Every reading the apps could take, not only the last.
        let mut seen = vec![];
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let view = machine.view();
            if view.state == MachineState::Exited {
                if seen.last() != Some(&view.exit_reason) {
                    seen.push(view.exit_reason);
                }
                if !support::run(root, &["status", dir.to_str().unwrap()])
                    .stdout
                    .starts_with(b"running")
                {
                    break;
                }
            }
            assert!(Instant::now() < deadline, "{name}: never ended");
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(seen, [Some(reason)], "{name}");
        let said = support::ok(root, &["status", dir.to_str().unwrap()]);
        assert!(said.starts_with(status), "{name}: {said}");
    }
}
