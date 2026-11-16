use super::*;
use origami_engine::fake::{FakeEngine, FakeSession};
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

const WAIT: Duration = Duration::from_secs(10);

#[derive(Default)]
struct Recorder {
    events: Mutex<Vec<Event>>,
    changed: Condvar,
}

impl Observer for Recorder {
    fn on_event(&self, event: Event) {
        self.events.lock().unwrap().push(event);
        self.changed.notify_all();
    }
}

impl Recorder {
    /// Wait until some event satisfies `want`.
    fn wait_for(&self, want: impl Fn(&Event) -> bool) -> bool {
        let guard = self.events.lock().unwrap();
        let (guard, result) = self
            .changed
            .wait_timeout_while(guard, WAIT, |events| !events.iter().any(&want))
            .unwrap();
        drop(guard);
        !result.timed_out()
    }
}

struct Fixture {
    root: tempfile::TempDir,
    engine: Arc<FakeEngine>,
    core: Core,
}

fn fixture_with(observer: Option<Arc<dyn Observer>>, ring: usize) -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let engine = Arc::new(FakeEngine::new());
    let mut config = Config::new(root.path());
    config.console_ring_bytes = ring;
    let core = Core::new(config, vec![engine.clone()], observer).unwrap();
    Fixture { root, engine, core }
}

fn fixture() -> Fixture {
    fixture_with(None, 1 << 20)
}

impl Fixture {
    fn create(&self, name: &str, offering: &str) -> String {
        let prom = self.root.path().join("prom.bin");
        std::fs::write(&prom, [0u8; 16]).unwrap();
        let op = self
            .core
            .create_machine(CreateRequest {
                name: name.into(),
                offering: offering.into(),
                boot_prom: Some(prom),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(op.wait(Some(WAIT)), OperationState::Succeeded);
        op.result_machine().unwrap()
    }

    fn session(&self, key: &str) -> Arc<FakeSession> {
        self.engine.session_in(Path::new(key)).unwrap()
    }

    fn started(&self, name: &str) -> (Machine, Arc<FakeSession>) {
        let key = self.create(name, "fake.1");
        let machine = self.core.start_machine(&key).unwrap();
        let session = self.session(&key);
        (machine, session)
    }
}

#[test]
fn the_library_lists_created_machines_and_the_catalogue_lists_offerings() {
    let f = fixture();
    assert!(f.core.list_machines().unwrap().is_empty());
    let offerings = f.core.catalogue().unwrap();
    assert_eq!(
        offerings.iter().map(|o| o.id.as_str()).collect::<Vec<_>>(),
        ["fake.1", "fake.2", "fakeview.1"]
    );
    assert_eq!(offerings[0].engine, "fake");
    assert_eq!(offerings[0].memory_accepted_mib, [64, 128, 256]);
    let a = f.create("b-machine", "fake.2");
    f.create("a-machine", "fake.1");
    let list = f.core.list_machines().unwrap();
    assert_eq!(
        list.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(),
        ["a-machine", "b-machine"]
    );
    assert_eq!(list[1].key, a);
    assert_eq!(
        (list[1].nodes, list[1].state, list[1].problem.clone()),
        (2, MachineState::NotRunning, None)
    );
    // The same machine opened twice is one machine.
    let first = f.core.open_machine("a-machine").unwrap();
    let second = f.core.open_machine(&list[0].key).unwrap();
    assert!(Arc::ptr_eq(&first.inner, &second.inner));
}

#[test]
fn a_started_machine_runs_has_a_head_and_reports_state_changes_as_events() {
    let recorder = Arc::new(Recorder::default());
    let f = fixture_with(Some(recorder.clone()), 1 << 20);
    let key = f.create("m", "fake.1");
    let machine = f.core.open_machine(&key).unwrap();
    assert_eq!(machine.view().state, MachineState::NotRunning);
    let op = machine.start().unwrap();
    assert_eq!(op.wait(Some(WAIT)), OperationState::Succeeded);
    assert_eq!(op.result_machine().as_deref(), Some(key.as_str()));
    let view = machine.view();
    assert_eq!(view.state, MachineState::Running);
    assert_eq!(view.engine, "fake");
    assert_eq!(
        (view.heads.len(), view.heads[0].width, view.heads[0].height),
        (1, 64, 48)
    );
    assert!(recorder
        .wait_for(|e| e.machine.as_deref() == Some(&key) && e.kinds.contains(&EventKind::State)));
    assert!(recorder.wait_for(
        |e| e.machine.as_deref() == Some(&key) && e.kinds.contains(&EventKind::Operation)
    ));
    assert!(recorder.wait_for(|e| e.machine.is_none() && e.kinds.contains(&EventKind::Operation)));
}

#[test]
fn a_burst_of_frames_reaches_the_observer_once_per_machine() {
    // Hold the observer on its first event while the engine publishes 500
    // frames; when released it must see the burst as one event, not 500.
    struct Gate {
        calls: AtomicU32,
        entered: Sender<()>,
        release: Mutex<std::sync::mpsc::Receiver<()>>,
    }
    impl Observer for Gate {
        fn on_event(&self, event: Event) {
            if event.machine.is_some() && self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                self.entered.send(()).unwrap();
                self.release.lock().unwrap().recv().unwrap();
            }
        }
    }
    let (entered, entered_rx) = channel();
    let (release_tx, release) = channel();
    let gate = Arc::new(Gate {
        calls: AtomicU32::new(0),
        entered,
        release: Mutex::new(release),
    });
    let f = fixture_with(Some(gate.clone()), 1 << 20);
    let (_machine, session) = f.started("m");
    entered_rx.recv_timeout(WAIT).unwrap();
    // Let the start operation finish raising its own events first.
    std::thread::sleep(Duration::from_millis(200));
    let before = gate.calls.load(Ordering::SeqCst);
    for _ in 0..500 {
        session.advance_frame();
    }
    release_tx.send(()).unwrap();
    // One more delivery carries the whole burst, then it goes quiet.
    std::thread::sleep(Duration::from_millis(300));
    let after = gate.calls.load(Ordering::SeqCst);
    assert!(
        after - before <= 1,
        "{} events for one burst",
        after - before
    );
}

#[test]
fn the_observer_can_call_back_into_the_core_without_deadlock() {
    struct Reentrant {
        core: Mutex<Option<Core>>,
        done: Sender<usize>,
    }
    impl Observer for Reentrant {
        fn on_event(&self, event: Event) {
            let Some(key) = event.machine else { return };
            let core = self.core.lock().unwrap().clone().unwrap();
            let listed = core.list_machines().unwrap().len();
            let machine = core.open_machine(&key).unwrap();
            let _ = machine.view();
            let _ = machine.heads();
            let _ = self.done.send(listed);
        }
    }
    let (done, done_rx) = channel();
    let observer = Arc::new(Reentrant {
        core: Mutex::new(None),
        done,
    });
    let f = fixture_with(Some(observer.clone()), 1 << 20);
    *observer.core.lock().unwrap() = Some(f.core.clone());
    let (_machine, session) = f.started("m");
    session.advance_frame();
    assert_eq!(done_rx.recv_timeout(WAIT).unwrap(), 1);
    f.core.shutdown();
}

#[test]
fn errors_carry_a_kind_a_program_can_branch_on() {
    let f = fixture();
    let kind = |r: CoreResult<_>| r.map(|_: ()| ()).unwrap_err().kind;
    assert_eq!(
        kind(f.core.open_machine("nope").map(|_| ())),
        ErrorKind::NotFound
    );
    let key = f.create("m", "fake.1");
    let machine = f.core.open_machine(&key).unwrap();
    assert_eq!(
        kind(machine.stop(StopKind::Clean).map(|_| ())),
        ErrorKind::NotRunning
    );
    assert_eq!(kind(machine.send_key(Key::A, true)), ErrorKind::NotRunning);
    assert_eq!(kind(machine.console_send(b"x")), ErrorKind::NotRunning);
    assert_eq!(
        kind(machine.take_frame(0, &mut vec![]).map(|_| ())),
        ErrorKind::NotRunning
    );
    machine.start().unwrap().wait(Some(WAIT));
    assert_eq!(kind(machine.start().map(|_| ())), ErrorKind::Busy);
    let bad = f.core.create_machine(CreateRequest {
        name: "../x".into(),
        offering: "fake.1".into(),
        ..Default::default()
    });
    assert_eq!(kind(bad.map(|_| ())), ErrorKind::Invalid);
    let op = f
        .core
        .create_machine(CreateRequest {
            name: "n".into(),
            offering: "no.such".into(),
            ..Default::default()
        })
        .unwrap();
    match op.wait(Some(WAIT)) {
        OperationState::Failed(error) => assert_eq!(error.kind, ErrorKind::NotFound),
        other => panic!("{other:?}"),
    }
    // Unreadable machine files are listed with a problem, not an error.
    std::fs::create_dir(f.root.path().join("broken")).unwrap();
    std::fs::write(f.root.path().join("broken/machine.toml"), "not toml [").unwrap();
    let broken = f
        .core
        .list_machines()
        .unwrap()
        .into_iter()
        .find(|m| m.name == "broken")
        .unwrap();
    assert!(broken.problem.is_some());
}

#[test]
fn a_clean_stop_and_a_forced_stop_end_the_machine_differently() {
    let f = fixture();
    let (clean, _) = f.started("clean");
    let op = clean.stop(StopKind::Clean).unwrap();
    assert!(!op.cancel(), "a clean stop never gives up");
    assert_eq!(op.wait(Some(WAIT)), OperationState::Succeeded);
    let view = clean.view();
    assert_eq!(
        (view.state, view.exit_code),
        (MachineState::Exited, Some(0))
    );

    let (forced, _) = f.started("forced");
    assert_eq!(
        forced.stop(StopKind::Force).unwrap().wait(Some(WAIT)),
        OperationState::Succeeded
    );
    let view = forced.view();
    assert_eq!((view.state, view.exit_code), (MachineState::Exited, None));

    // An exited machine starts again.
    clean.start().unwrap().wait(Some(WAIT));
    assert_eq!(clean.view().state, MachineState::Running);
}

#[test]
fn a_crash_is_seen_with_the_last_error() {
    let f = fixture();
    let (machine, session) = f.started("m");
    session.crash(3, "bus error");
    let view = machine.view();
    assert_eq!(view.state, MachineState::Exited);
    assert_eq!(
        (view.exit_code, view.last_error.as_deref()),
        (Some(3), Some("bus error"))
    );
}

#[test]
fn release_all_keys_lifts_what_is_held_and_reaches_the_engine() {
    let f = fixture();
    let (machine, session) = f.started("m");
    machine.send_key(Key::ShiftLeft, true).unwrap();
    machine.send_key(Key::A, true).unwrap();
    machine.send_key(Key::A, false).unwrap();
    machine
        .send_input(&[InputEvent::Button {
            button: PointerButton::Left,
            down: true,
        }])
        .unwrap();
    assert_eq!(machine.release_all_keys().unwrap(), 2);
    let input = session.input();
    assert_eq!(
        input[4..],
        [
            InputEvent::Key {
                key: Key::ShiftLeft,
                down: false
            },
            InputEvent::Button {
                button: PointerButton::Left,
                down: false
            },
        ]
    );
    assert_eq!(machine.release_all_keys().unwrap(), 0);
    assert_eq!(session.input().len(), 6);
}

#[test]
fn the_console_keeps_a_ring_that_several_readers_can_follow() {
    let f = fixture_with(None, 32);
    let (machine, session) = f.started("m");
    let wait_total = |n: u64| {
        for _ in 0..500 {
            if machine.view().console.total >= n {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("console total stayed at {}", machine.view().console.total);
    };
    wait_total(19);
    let chunk = machine.console_read(0, 1024);
    assert_eq!(chunk.bytes, b"fake console\r\nlogin: ");
    assert_eq!((chunk.next, chunk.dropped), (21, 0));
    machine.console_send(b"root\r\n").unwrap();
    wait_total(27);
    assert_eq!(machine.console_read(chunk.next, 1024).bytes, b"root\r\n");
    // A reader that fell behind the 32-byte ring is told how much it lost.
    session.say(&"x".repeat(40));
    wait_total(67);
    let late = machine.console_read(0, 1024);
    assert_eq!((late.bytes.len(), late.dropped, late.next), (32, 35, 67));
    assert!(machine.view().console.connected);
    machine.stop(StopKind::Clean).unwrap().wait(Some(WAIT));
    for _ in 0..500 {
        if !machine.view().console.connected {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(!machine.view().console.connected);
    assert_eq!(
        machine.console_send(b"x").unwrap_err().kind,
        ErrorKind::NotRunning
    );
}

#[test]
fn frame_hooks_run_on_a_new_frame_and_not_otherwise() {
    let f = fixture();
    let (machine, session) = f.started("m");
    let count = Arc::new(AtomicU32::new(0));
    let counter = count.clone();
    let id = machine.add_frame_hook(Arc::new(move || {
        counter.fetch_add(1, Ordering::SeqCst);
    }));
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(count.load(Ordering::SeqCst), 0);
    session.advance_frame();
    session.advance_frame();
    assert_eq!(count.load(Ordering::SeqCst), 2);
    let mut pixels = Vec::new();
    let frame = machine.take_frame(0, &mut pixels).unwrap();
    assert!(matches!(frame, Some(Frame::Rows(_))));
    assert_eq!(pixels.len(), 64 * 48 * 4);
    assert!(machine.take_frame(0, &mut pixels).unwrap().is_none());
    machine.remove_frame_hook(id);
    session.advance_frame();
    assert_eq!(count.load(Ordering::SeqCst), 2);
}

#[test]
fn controls_and_media_reach_the_engine_and_a_missing_drive_is_refused() {
    let f = fixture();
    let (machine, session) = f.started("m");
    machine.control(Control::Pause).unwrap();
    assert_eq!(machine.view().state, MachineState::Paused);
    machine.control(Control::Resume).unwrap();
    assert_eq!(session.controls(), [Control::Pause, Control::Resume]);
    assert_eq!(
        machine.change_media("cdrom0", None).unwrap_err().kind,
        ErrorKind::Engine
    );
    let shot = f.root.path().join("shot.ppm");
    machine.screendump(0, &shot).unwrap();
    assert!(std::fs::read(&shot)
        .unwrap()
        .starts_with(b"P6\n64 48\n255\n"));
}

#[test]
fn the_command_table_names_each_command_and_when_it_applies() {
    let f = fixture();
    let commands = f.core.commands();
    let ids: Vec<&str> = commands.iter().map(|c| c.id.as_str()).collect();
    for id in [
        "machine.new",
        "machine.start",
        "machine.stop",
        "machine.reset",
        "machine.pause",
        "machine.resume",
        "machine.release-keys",
        "machine.screenshot",
        "app.quit",
    ] {
        assert!(ids.contains(&id), "{id}");
    }
    let by = |id: &str| commands.iter().find(|c| c.id == id).unwrap();
    assert!(by("machine.new").enabled_when.holds(None));
    assert!(!by("machine.start").enabled_when.holds(None));
    assert!(by("machine.start")
        .enabled_when
        .holds(Some(MachineState::Exited)));
    assert!(!by("machine.start")
        .enabled_when
        .holds(Some(MachineState::Running)));
    assert!(by("machine.resume")
        .enabled_when
        .holds(Some(MachineState::Paused)));
    assert!(!by("machine.pause")
        .enabled_when
        .holds(Some(MachineState::Paused)));
    assert!(commands.iter().all(|c| c.shortcuts.len() == 3));
    assert_eq!(by("app.quit").shortcuts[0].accelerator, "Cmd+Q");
}
