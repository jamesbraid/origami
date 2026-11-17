use super::*;
use crate::Change;
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
    fixture_tuned(observer, |config| config.console_ring_bytes = ring)
}

fn fixture_tuned(observer: Option<Arc<dyn Observer>>, tune: impl FnOnce(&mut Config)) -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let engine = Arc::new(FakeEngine::new());
    let mut config = Config::new(root.path());
    tune(&mut config);
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
        ["fake.1", "fake.2", "fakeview.1", "fakeblind.1"]
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
fn a_created_machine_gets_the_identity_the_cli_gives() {
    let f = fixture();
    let key = f.create("i", "fake.1");
    let file = origami_machine::read_machine(Path::new(&key)).unwrap();
    assert!(file.identity.is_some_and(|i| i.emu_id.is_some()));
}

#[test]
fn a_running_machine_with_no_head_says_why_it_shows_nothing() {
    let f = fixture();
    let key = f.create("b", "fakeblind.1");
    let machine = f.core.start_machine(&key).unwrap();
    let view = machine.view();
    assert!(view.heads.is_empty());
    assert!(
        view.reason.contains("the fake display has no head"),
        "{}",
        view.reason
    );
    let (shown, _) = f.started("s");
    assert_eq!(shown.view().reason, "The machine is running.");
}

#[test]
fn a_graphics_product_the_machine_cannot_take_fails_the_create() {
    let f = fixture();
    let op = f
        .core
        .create_machine(CreateRequest {
            name: "g".into(),
            offering: "fake.1".into(),
            graphics: Some("v12".into()),
            ..Default::default()
        })
        .unwrap();
    assert!(matches!(op.wait(Some(WAIT)), OperationState::Failed(_)));
    assert!(!f.core.root().join("g").exists());
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
fn a_power_off_the_guest_ignores_fails_in_bounded_time_and_force_quit_ends_it() {
    let f = fixture_tuned(None, |config| {
        config.stop_patience = Duration::from_millis(100)
    });
    let (machine, session) = f.started("stuck");
    session.ignore_clean_stop();
    let started = std::time::Instant::now();
    let op = machine.stop(StopKind::Clean).unwrap();
    assert_eq!(machine.view().state, MachineState::Stopping);
    let error = match op.wait(Some(WAIT)) {
        OperationState::Failed(error) => error,
        other => panic!("{other:?}"),
    };
    assert!(
        started.elapsed() < WAIT,
        "the stop waited past its patience"
    );
    assert_eq!(error.kind, ErrorKind::Busy);
    assert!(error.message.contains("did not power off"), "{error}");
    // Still up, so Power Off and Force Quit are offered again.
    assert_eq!(machine.view().state, MachineState::Running);
    assert_eq!(
        machine.stop(StopKind::Force).unwrap().wait(Some(WAIT)),
        OperationState::Succeeded
    );
    assert_eq!(machine.view().state, MachineState::Exited);
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
fn hiding_a_machine_reaches_its_frame_source() {
    let f = fixture();
    let (machine, session) = f.started("m");
    assert!(session.frames_visible());
    machine.set_visible(false).unwrap();
    assert!(!session.frames_visible());
    machine.set_visible(true).unwrap();
    assert!(session.frames_visible());
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
        "machine.force-quit",
        "machine.eject-cd",
        "window.console",
        "window.diagnostics",
        "machine.settings",
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
    // A starting machine has no session to stop; the Start is cancelled instead.
    assert!(!by("machine.stop")
        .enabled_when
        .holds(Some(MachineState::Starting)));
    assert!(by("machine.stop")
        .enabled_when
        .holds(Some(MachineState::Running)));
    assert_eq!(by("app.quit").shortcuts[0].accelerator, "Cmd+Q");
}

#[test]
fn the_command_shortcuts_match_the_design() {
    let f = fixture();
    let commands = f.core.commands();
    let keys = |id: &str| -> Vec<&str> {
        let c = commands.iter().find(|c| c.id == id).unwrap();
        c.shortcuts.iter().map(|s| s.accelerator.as_str()).collect()
    };
    assert_eq!(
        keys("machine.start"),
        ["Cmd+R", "Ctrl+Shift+R", "Ctrl+Shift+R"]
    );
    assert_eq!(
        keys("machine.stop"),
        ["Alt+Cmd+P", "Ctrl+Shift+P", "Ctrl+Shift+P"]
    );
    assert_eq!(
        commands
            .iter()
            .find(|c| c.id == "machine.stop")
            .unwrap()
            .title,
        "Power Off"
    );
    assert!(keys("machine.reset").is_empty());
    assert!(keys("machine.force-quit").is_empty());
    assert_eq!(keys("window.console")[1], "Ctrl+Shift+O");
    // Screenshot would collide with Power Off, so it is Mac-only.
    assert_eq!(keys("machine.screenshot"), ["Cmd+Shift+3"]);
}

#[test]
fn a_start_applies_the_pending_first_boot_settings_first() {
    let f = fixture();
    let key = f.create("m", "fake.1");
    let file = Path::new(&key).join("machine.toml");
    let mut text = std::fs::read_to_string(&file)
        .unwrap()
        .replace("id = \"qemu\"", "id = \"fake\"");
    text.push_str("\n[firstboot.nvram]\nconsole = \"d\"\n");
    std::fs::write(&file, text).unwrap();
    // The fake topology has no NVRAM layout, so the settings cannot apply;
    // that is a warning, and the machine still starts.
    let machine = f.core.open_machine(&key).unwrap();
    let state = machine.start().unwrap().wait(Some(WAIT));
    assert!(matches!(state, OperationState::Succeeded), "{state:?}");
    assert_ne!(machine.view().state, MachineState::NotRunning);
}

#[test]
fn settings_read_what_the_machine_file_holds() {
    let f = fixture();
    let machine = f.core.open_machine(&f.create("s", "fake.2")).unwrap();
    let s = machine.settings().unwrap();
    assert_eq!(
        (s.processor.as_str(), s.processor_part.as_str()),
        ("fake-cpu", "FAKE1")
    );
    assert_eq!((s.cpus_per_node, s.cpus_per_node_allowed), (1, vec![1]));
    assert_eq!(
        (s.memory_per_node_mib, s.memory_allowed_mib),
        (128, vec![64, 128, 256])
    );
    assert_eq!(s.network_mode, "user");
    assert!(s.forwards.is_empty() && s.shares.is_empty() && s.drives.is_empty());
}

#[test]
fn an_edit_takes_hold_on_a_stopped_machine_and_is_refused_on_a_running_one() {
    let f = fixture();
    let key = f.create("e", "fake.1");
    let machine = f.core.open_machine(&key).unwrap();
    machine.apply_change(&Change::SetMemory(256)).unwrap();
    assert_eq!(machine.settings().unwrap().memory_per_node_mib, 256);
    // The next start launches the edited file, not the one opened.
    machine.start().unwrap().wait(Some(WAIT));
    assert_eq!(machine.view().state, MachineState::Running);
    let error = machine.apply_change(&Change::SetMemory(64)).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Busy);
    assert_eq!(machine.settings().unwrap().memory_per_node_mib, 256);
}

#[test]
fn a_create_names_its_engine_or_takes_the_first_that_offers_the_offering() {
    let f = fixture();
    let prom = f.root.path().join("prom.bin");
    std::fs::write(&prom, [0u8; 16]).unwrap();
    let create = |name: &str, offering: &str, engine: Option<&str>| {
        let op = f
            .core
            .create_machine(CreateRequest {
                name: name.into(),
                offering: offering.into(),
                engine: engine.map(Into::into),
                boot_prom: Some(prom.clone()),
                ..Default::default()
            })
            .unwrap();
        op.wait(Some(WAIT))
    };
    assert_eq!(create("a", "fake.1", None), OperationState::Succeeded);
    assert_eq!(
        create("b", "fake.1", Some("fake")),
        OperationState::Succeeded
    );
    let failed = |state| match state {
        OperationState::Failed(error) => (error.kind, error.detail),
        other => panic!("{other:?}"),
    };
    let unknown = failed(create("c", "fake.1", Some("nope")));
    assert_eq!(unknown, (ErrorKind::NotFound, Some("engine".into())));
    let wrong = failed(create("d", "no.such", Some("fake")));
    assert_eq!(wrong, (ErrorKind::NotFound, Some("offering".into())));
    assert!(!f.core.root().join("c").exists());
}

#[test]
fn an_edit_in_progress_refuses_a_start_and_another_edit() {
    let f = fixture();
    let machine = f.core.open_machine(&f.create("e", "fake.1")).unwrap();
    // One edit's validations, the last of which is the reload.
    let before = f.engine.validations();
    machine.apply_change(&Change::SetMemory(256)).unwrap();
    let reload = f.engine.validations() - before;
    let target = f.engine.validations() + reload;
    let (entered, entered_rx) = channel();
    let (release, release_rx) = channel::<()>();
    let (entered, release_rx) = (Mutex::new(entered), Mutex::new(release_rx));
    f.engine.set_validate_hook(Arc::new(move |call| {
        if call == target {
            entered.lock().unwrap().send(()).unwrap();
            release_rx.lock().unwrap().recv().unwrap();
        }
        None
    }));
    let editor = machine.clone();
    let edit = std::thread::spawn(move || editor.apply_change(&Change::SetMemory(64)));
    entered_rx.recv_timeout(WAIT).unwrap();
    assert_eq!(machine.start().err().unwrap().kind, ErrorKind::Busy);
    let second = machine.apply_change(&Change::SetMemory(128)).unwrap_err();
    assert_eq!(second.kind, ErrorKind::Busy);
    release.send(()).unwrap();
    edit.join().unwrap().unwrap();
    assert_eq!(machine.settings().unwrap().memory_per_node_mib, 64);
}

#[test]
fn a_reload_that_fails_after_the_write_says_the_change_was_saved() {
    let f = fixture();
    let machine = f.core.open_machine(&f.create("r", "fake.1")).unwrap();
    let before = f.engine.validations();
    machine.apply_change(&Change::SetMemory(256)).unwrap();
    let target = f.engine.validations() + (f.engine.validations() - before);
    f.engine.set_validate_hook(Arc::new(move |call| {
        (call == target).then(|| "the engine gave up".to_string())
    }));
    let error = machine.apply_change(&Change::SetMemory(64)).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Internal);
    assert!(error.message.contains("saved"), "{error}");
    let file = origami_machine::read_machine(Path::new(machine.key())).unwrap();
    assert_eq!(file.machine.memory_per_node, "64MiB");
    // The edit lock is free again and a later edit works.
    machine.apply_change(&Change::SetMemory(128)).unwrap();
}

#[test]
fn a_start_racing_an_edit_launches_the_edited_file_or_is_refused() {
    let f = fixture();
    for round in 0..20 {
        let machine = f
            .core
            .open_machine(&f.create(&format!("r{round}"), "fake.1"))
            .unwrap();
        let editor = machine.clone();
        let edit = std::thread::spawn(move || editor.apply_change(&Change::SetMemory(256)));
        let started = machine.start();
        let edited = edit.join().unwrap();
        // Each is either done or told Busy; a start that wins is never edited under.
        for refused in [started.as_ref().err(), edited.as_ref().err()]
            .into_iter()
            .flatten()
        {
            assert_eq!(refused.kind, ErrorKind::Busy, "{refused}");
        }
        if let Ok(op) = started {
            assert_eq!(op.wait(Some(WAIT)), OperationState::Succeeded);
        }
        let want = if edited.is_ok() { 256 } else { 128 };
        assert_eq!(machine.settings().unwrap().memory_per_node_mib, want);
    }
}

#[test]
fn an_exit_says_why_the_machine_ended() {
    let f = fixture();
    let n = std::cell::Cell::new(0);
    let ended = |stop: Option<StopKind>, end: &dyn Fn(&FakeSession)| {
        n.set(n.get() + 1);
        let (machine, session) = f.started(&format!("m{}", n.get()));
        if let Some(how) = stop {
            machine.stop(how).unwrap().wait(Some(WAIT));
        }
        end(&session);
        let view = machine.view();
        (view.exit_reason, view.exit_detail)
    };
    let guest = ended(None, &|s| s.shut_down());
    assert_eq!(guest, (Some(ExitReason::GuestPoweredOff), None));
    let host = ended(Some(StopKind::Clean), &|_| {});
    assert_eq!(host.0, Some(ExitReason::HostStopped));
    assert_eq!(ended(Some(StopKind::Force), &|_| {}).0, host.0);
    let crash = ended(None, &|s| s.crash(3, "bus error"));
    assert_eq!(crash, (Some(ExitReason::Crashed), Some("bus error".into())));
    let killed = ended(None, &|s| s.kill(9));
    assert_eq!(killed, (Some(ExitReason::Crashed), Some("signal 9".into())));
    assert_eq!(
        ended(None, &|s| s.vanish()).0,
        Some(ExitReason::RunnerError)
    );
    let (machine, _) = f.started("running");
    assert_eq!(machine.view().exit_reason, None);
}

#[test]
fn a_stop_another_process_asked_for_is_still_a_host_stop() {
    let f = fixture();
    let ended = |name: &str, requested: Option<&str>, end: &dyn Fn(&FakeSession)| {
        let (machine, session) = f.started(name);
        let note = origami_machine::control::Exit {
            requested: requested.map(Into::into),
            ..Default::default()
        };
        origami_machine::control::write_exit(Path::new(machine.key()), &note).unwrap();
        end(&session);
        machine.view().exit_reason
    };
    assert_eq!(
        ended("a", Some("stop"), &|s| s.shut_down()),
        Some(ExitReason::HostStopped)
    );
    assert_eq!(
        ended("b", Some("force"), &|s| s.kill(9)),
        Some(ExitReason::HostStopped)
    );
    assert_eq!(ended("c", None, &|s| s.kill(9)), Some(ExitReason::Crashed));
}

#[test]
fn a_stop_that_failed_or_a_power_button_the_guest_ignored_does_not_claim_the_exit() {
    let f = fixture_tuned(None, |config| {
        config.stop_patience = Duration::from_millis(100)
    });
    let (machine, session) = f.started("stuck");
    session.ignore_clean_stop();
    let op = machine.stop(StopKind::Clean).unwrap();
    assert!(matches!(op.wait(Some(WAIT)), OperationState::Failed(_)));
    session.shut_down();
    assert_eq!(
        machine.view().exit_reason,
        Some(ExitReason::GuestPoweredOff)
    );
    let (machine, session) = f.started("button");
    machine.pressed_power_button_at(std::time::Instant::now() - Duration::from_secs(600));
    session.shut_down();
    assert_eq!(
        machine.view().exit_reason,
        Some(ExitReason::GuestPoweredOff)
    );
}
