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

/// A second core over the library of an earlier fixture.
fn fixture_in(root: &Path) -> Fixture {
    let engine = Arc::new(FakeEngine::new());
    let core = Core::new(Config::new(root), vec![engine.clone()], None).unwrap();
    Fixture {
        root: tempfile::tempdir().unwrap(),
        engine,
        core,
    }
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
        [
            "fake.1",
            "fake.2",
            "fakeview.1",
            "fakeblind.1",
            "fakebig.1",
            "origin200.1"
        ]
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
fn a_frame_hook_runs_when_a_start_attaches_the_display() {
    // An engine may wake before the start has stored its display.
    let f = fixture();
    let key = f.create("m", "fake.1");
    let machine = f.core.open_machine(&key).unwrap();
    let count = Arc::new(AtomicU32::new(0));
    let counter = count.clone();
    machine.add_frame_hook(Arc::new(move || {
        counter.fetch_add(1, Ordering::SeqCst);
    }));
    let machine = f.core.start_machine(&key).unwrap();
    assert!(count.load(Ordering::SeqCst) >= 1);
    assert!(!machine.heads().is_empty());
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
    assert_eq!(s.network.mode, "user");
    assert!(s.forwards.is_empty() && s.shares.is_empty() && s.drives.is_empty());
}

#[test]
fn a_change_keeps_only_what_the_new_mode_uses() {
    use crate::NetworkMode::{Private, Tap, User, Vmnet};
    let f = fixture();
    let machine = f.core.open_machine(&f.create("b", "fake.1")).unwrap();
    let net = || machine.settings().unwrap().network;
    machine
        .apply_change(&Change::SetNetwork(crate::NetworkChange {
            mode: Private,
            endpoint: Some("/run/net.sock".into()),
            interface: None,
            share_address: None,
            subnet: None,
            mac: None,
        }))
        .unwrap();
    machine.set_network(Private, "", None, None).unwrap();
    assert_eq!(net().endpoint.as_deref(), Some("/run/net.sock"));
    // Another mode must not inherit it: a private endpoint goes to the network socket.
    machine.set_network(Vmnet, "/x/vmnet", None, None).unwrap();
    assert_eq!((net().device, net().warnings.len()), ("/x/vmnet".into(), 1));
    machine.set_network(Private, "", None, None).unwrap();
    assert_eq!(net().endpoint, None);
    machine
        .set_network(Tap, "tap0", Some("192.168.1.5/24"), None)
        .unwrap();
    machine
        .set_network(User, "tap0", Some("192.168.1.5/24"), None)
        .unwrap();
    let n = net();
    assert!(n.device.is_empty() && n.share_address.is_none());
    let error = machine
        .set_network(Tap, " ", None, None)
        .unwrap_err()
        .to_string();
    assert!(
        error.starts_with("Enter the ") && !error.contains("--"),
        "{error}"
    );
}

#[test]
fn moving_the_subnet_or_the_network_returns_the_lease_advice() {
    use crate::NetworkMode::{Tap, User};
    let f = fixture();
    let machine = f.core.open_machine(&f.create("a", "fake.1")).unwrap();
    let advice = |r: crate::Report| {
        r.lines
            .into_iter()
            .filter(|l| l.kind == crate::Kind::Advice)
            .collect::<Vec<_>>()
    };
    let moved = advice(
        machine
            .set_network(User, "", None, Some("10.42.0.0/24"))
            .unwrap(),
    );
    assert!(moved.len() == 1 && moved[0].text.contains("booted on 10.98.0.0/24"));
    assert_eq!(
        moved[0].copy_text(),
        Some("rm /var/adm/proclaim.lease_info* /etc/resolv.conf")
    );
    assert!(advice(
        machine
            .set_network(User, "", None, Some("10.42.0.0/24"))
            .unwrap()
    )
    .is_empty());
    assert_eq!(
        advice(machine.set_network(Tap, "tap0", None, None).unwrap()).len(),
        1
    );
    let error = machine
        .set_network(User, "", None, Some("8.8.8.0/24"))
        .unwrap_err();
    assert!(error.to_string().contains("private"), "{error}");
}

#[test]
fn a_new_machine_previews_its_id_and_address_and_refuses_ones_taken() {
    let f = fixture();
    let key = f.create("first", "fake.1");
    let held = origami_machine::read_machine(Path::new(&key))
        .unwrap()
        .identity
        .unwrap();
    let id = held.emu_id.unwrap();
    let preview = |id: &str, mac: Option<&str>| {
        f.core
            .preview_new_identity("fake.1", None, id, mac)
            .unwrap()
    };
    let next = preview("", None);
    assert!(
        next.emu_id != id
            && next.problem.is_none()
            && next.from_id
            && next.mac.starts_with("08:00:69:e1:")
    );
    assert!(preview(&id.to_string(), None)
        .problem
        .unwrap()
        .contains("already"));
    assert_eq!(
        preview("x", None).problem.as_deref(),
        Some("Choose a number from 1 to 2999.")
    );
    assert!(preview("3500", None).problem.unwrap().contains("reserved"));
    assert!(preview("77", Some(&held.mac))
        .problem
        .unwrap()
        .contains("collides"));
    assert!(preview("77", Some("nonsense")).problem.is_some());
    let own = preview(" 77 ", Some("08:00:69:12:34:56"));
    assert!(own.problem.is_none());
    assert_eq!((own.emu_id, own.mac.as_str()), (77, "08:00:69:12:34:56"));
    let op = f
        .core
        .create_machine(CreateRequest {
            name: "m".into(),
            offering: "fake.1".into(),
            boot_prom: Some(f.root.path().join("prom.bin")),
            emu_id: Some("41".into()),
            mac: Some("08:00:69:12:34:50".into()),
            ..Default::default()
        })
        .unwrap();
    op.wait(Some(WAIT));
    let made = origami_machine::read_machine(&f.root.path().join("m"))
        .unwrap()
        .identity
        .unwrap();
    assert_eq!(
        (made.emu_id, made.mac.as_str()),
        (Some(41), "08:00:69:12:34:50")
    );
}

#[test]
fn the_console_line_is_chosen_from_the_machines_lines_and_put_back() {
    let f = fixture();
    let machine = f.core.open_machine(&f.create("c", "fake.1")).unwrap();
    let s = machine.settings().unwrap();
    assert_eq!(s.console_port, "ttyS0");
    let lines: Vec<_> = s
        .console_ports
        .iter()
        .map(|p| (p.name.as_str(), p.is_default))
        .collect();
    assert_eq!(lines, [("ttyS0", true), ("ttyS1", false)]);
    assert_eq!(s.console_ports[0].label, "ttyS0 (default)");
    assert_eq!(s.console_ports[1].label, "ttyS1");
    machine
        .apply_change(&Change::SetConsole(Some("ttyS1".into())))
        .unwrap();
    assert_eq!(machine.settings().unwrap().console_port, "ttyS1");
    machine.apply_change(&Change::SetConsole(None)).unwrap();
    assert_eq!(machine.settings().unwrap().console_port, "ttyS0");
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
fn a_machine_that_exited_on_its_own_starts_again() {
    let f = fixture();
    let (machine, session) = f.started("m");
    session.crash(0, "");
    assert_eq!(machine.view().state, MachineState::Exited);
    assert_eq!(
        machine.start().unwrap().wait(Some(WAIT)),
        OperationState::Succeeded
    );
    assert_eq!(machine.view().state, MachineState::Running);
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

#[test]
fn a_machine_another_process_runs_is_listed_as_left_running() {
    let f = fixture();
    let key = f.create("m", "fake.1");
    let dir = Path::new(&key);
    let state = || f.core.list_machines().unwrap()[0].state;
    // An edit holds the lock too, but leaves no run record.
    let lock = origami_machine::control::lock_for_edit(dir, "t").unwrap();
    assert_eq!(state(), MachineState::NotRunning);
    origami_machine::control::write(dir, &Default::default()).unwrap();
    assert_eq!(state(), MachineState::LeftRunning);
    drop(lock);
    assert_eq!(state(), MachineState::NotRunning);
}

#[test]
fn an_edit_of_a_machine_another_process_runs_is_busy() {
    let f = fixture();
    let machine = f.core.open_machine(&f.create("l", "fake.1")).unwrap();
    let dir = Path::new(machine.key());
    let _lock = origami_machine::control::lock_for_edit(dir, "t").unwrap();
    origami_machine::control::write(dir, &Default::default()).unwrap();
    let error = machine.apply_change(&Change::SetMemory(256)).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Busy);
    assert_eq!(machine.settings().unwrap().memory_per_node_mib, 128);
}

#[test]
fn stopping_a_machine_that_has_ended_says_it_is_not_running() {
    let f = fixture();
    let (machine, session) = f.started("m");
    session.shut_down();
    for how in [StopKind::Clean, StopKind::Force] {
        assert_eq!(machine.stop(how).err().unwrap().kind, ErrorKind::NotRunning);
    }
}

#[test]
fn cpu_load_follows_the_engine_and_is_empty_when_the_machine_is_not_running() {
    let f = fixture();
    let key = f.create("m", "fake.1");
    let idle = f.core.open_machine(&key).unwrap();
    let report = idle.cpu_load();
    assert!(report.cpus.is_empty());
    assert_eq!(
        report.unavailable.as_deref(),
        Some("the machine is not running")
    );

    let machine = f.core.start_machine(&key).unwrap();
    // The first sample lands after two readings, one interval apart.
    let report = (0..40)
        .find_map(|_| {
            std::thread::sleep(Duration::from_millis(100));
            Some(machine.cpu_load()).filter(|r| !r.cpus.is_empty())
        })
        .expect("a sample within four seconds");
    assert_eq!(report.unavailable, None);
    let near = |got: f32, want: f32| (got - want).abs() < 5.0;
    let (a, b) = (report.cpus[0], report.cpus[1]);
    assert!(near(a.user_pct, 30.0) && near(a.sys_pct, 10.0), "{a:?}");
    assert!(near(b.user_pct, 60.0) && near(b.sys_pct, 5.0), "{b:?}");
    let view = machine.osview();
    assert_eq!(view.bars.len(), 2);
    assert_eq!(view.bars[1].title, "CPU 1 host time");
}

#[test]
fn offerings_and_machines_say_what_they_are_called_and_whether_they_have_a_display() {
    let f = fixture();
    let offerings = f.core.catalogue().unwrap();
    // The fake catalogue names no topology; an engine says for itself whether
    // an offering has a display, so one with heads does and a blind one not.
    assert_eq!(
        (
            offerings[0].topology_name.as_str(),
            offerings[0].has_display
        ),
        ("fake", true)
    );
    let blind = offerings.iter().find(|o| o.id == "fakeblind.1").unwrap();
    assert!(!blind.has_display);
    let key = f.create("m", "fake.1");
    let summary = |f: &Fixture| f.core.list_machines().unwrap().remove(0);
    assert_eq!(summary(&f).topology_name.as_deref(), Some("fake"));
    assert!(!summary(&f).has_display);
    assert!(!f.core.open_machine(&key).unwrap().view().has_display);
    // A machine file with a pipe has a display, whatever the engine says.
    let toml = Path::new(&key).join("machine.toml");
    let text = std::fs::read_to_string(&toml).unwrap();
    std::fs::write(
        &toml,
        text + "\n[[pipe]]\nslot = \"widget-13\"\nfamily = \"odyssey\"\nproduct = \"v12\"\n",
    )
    .unwrap();
    assert!(summary(&f).has_display);
    // The view of the same machine, opened by a fresh core, agrees.
    let again = fixture_in(f.root.path());
    assert!(again.core.open_machine(&key).unwrap().view().has_display);
    // A topology the name table knows reaches the summary by its real name.
    let text = std::fs::read_to_string(&toml).unwrap();
    std::fs::write(
        &toml,
        text.replace("topology = \"fake\"", "topology = \"origin200\""),
    )
    .unwrap();
    assert_eq!(summary(&f).topology_name.as_deref(), Some("Origin 200"));
}

#[test]
fn changing_the_root_relists_at_once_and_keeps_a_running_machine() {
    let recorder = Arc::new(Recorder::default());
    let f = fixture_with(Some(recorder.clone()), 1 << 20);
    let (machine, _session) = f.started("old");
    let other = tempfile::tempdir().unwrap();
    f.core.set_root(other.path());
    assert!(recorder.wait_for(|e| e.machine.is_none() && e.kinds.contains(&EventKind::Library)));
    assert_eq!(f.core.root(), other.path());
    assert!(f.core.list_machines().unwrap().is_empty());
    assert_eq!(machine.view().state, MachineState::Running);
    let prom = other.path().join("prom.bin");
    std::fs::write(&prom, [0u8; 16]).unwrap();
    let op = f
        .core
        .create_machine(CreateRequest {
            name: "new".into(),
            offering: "fake.1".into(),
            boot_prom: Some(prom),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(op.wait(Some(WAIT)), OperationState::Succeeded);
    let names: Vec<_> = f
        .core
        .list_machines()
        .unwrap()
        .into_iter()
        .map(|m| m.name)
        .collect();
    assert_eq!(names, ["new"]);
}

/// A format 4 Origin 200 in the library, from the repository's fixture.
fn old_machine(f: &Fixture) -> std::path::PathBuf {
    let dir = f.root.path().join("old");
    std::fs::create_dir(&dir).unwrap();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/machines/format4-origin200.toml");
    std::fs::copy(fixture, dir.join("machine.toml")).unwrap();
    dir
}

#[test]
fn an_older_machine_is_listed_as_needing_an_upgrade_and_will_not_open() {
    let f = fixture();
    let dir = old_machine(&f);
    let current = f.create("current", "fake.1");
    let list = f.core.list_machines().unwrap();
    assert_eq!(
        list.iter().map(|m| m.needs_upgrade).collect::<Vec<_>>(),
        [false, true]
    );
    assert_eq!(
        f.core.open_machine("old").err().unwrap().kind,
        ErrorKind::NeedsUpgrade
    );
    assert!(f.core.open_machine(&current).is_ok());
    // A format 1 file does not read at all; the listing still says why.
    std::fs::write(dir.join("machine.toml"), "format = 1\n").unwrap();
    let old = &f.core.list_machines().unwrap()[1];
    assert!(old.needs_upgrade && old.problem.is_some());
}

#[test]
fn upgrading_an_older_machine_reports_what_the_guest_sees_and_then_it_opens() {
    let f = fixture();
    let dir = old_machine(&f);
    let before = std::fs::read(dir.join("machine.toml")).unwrap();
    let report = f.core.upgrade_machine("old").unwrap();
    assert!(report.lines.iter().any(|l| l.text.starts_with("upgraded ")));
    assert!(f
        .core
        .list_machines()
        .unwrap()
        .iter()
        .all(|m| !m.needs_upgrade));
    assert!(f.core.open_machine("old").is_ok());
    // The format 4 file is kept, as the CLI keeps it.
    assert_eq!(
        std::fs::read(dir.join("machine.toml.format4")).unwrap(),
        before
    );
    let again = f.core.upgrade_machine("old").unwrap();
    assert!(again.lines[0].text.contains("needs no upgrade"));
}

#[test]
fn a_failed_upgrade_leaves_an_unwritten_file_alone_and_a_machine_that_runs_is_busy() {
    let f = fixture();
    let dir = old_machine(&f);
    let text = std::fs::read_to_string(dir.join("machine.toml")).unwrap();
    std::fs::write(
        dir.join("machine.toml"),
        text.replace("origin200", "nosuchbox"),
    )
    .unwrap();
    let before = std::fs::read(dir.join("machine.toml")).unwrap();
    assert!(f.core.upgrade_machine("old").is_err());
    assert_eq!(std::fs::read(dir.join("machine.toml")).unwrap(), before);
    let _lock = origami_machine::control::lock_for_edit(&dir, "t").unwrap();
    assert_eq!(
        f.core.upgrade_machine("old").unwrap_err().kind,
        ErrorKind::Busy
    );
}

#[test]
fn a_forced_stop_reads_as_ours_whatever_status_the_kill_leaves() {
    for (code, signal) in [(None, Some(9)), (Some(1), None)] {
        let (reason, _) = classify_exit(code, signal, None, Some(StopKind::Force));
        assert_eq!(reason, ExitReason::HostStopped);
    }
    let (reason, _) = classify_exit(Some(1), None, None, None);
    assert_eq!(reason, ExitReason::Crashed);
}

fn irix_disk() -> Vec<u8> {
    std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/machines/disk-irix-6.5.30.img"
    ))
    .unwrap()
}

impl Fixture {
    fn create_with(&self, name: &str, system_disk: SystemDisk) -> (String, OperationState) {
        let prom = self.root.path().join("prom.bin");
        std::fs::write(&prom, [0u8; 16]).unwrap();
        let op = self
            .core
            .create_machine(CreateRequest {
                name: name.into(),
                offering: "fake.1".into(),
                boot_prom: Some(prom),
                system_disk: Some(system_disk),
                ..Default::default()
            })
            .unwrap();
        let state = op.wait(Some(WAIT));
        (self.root.path().join(name).to_string_lossy().into(), state)
    }
}

#[test]
fn a_new_machine_gets_an_empty_disk_at_target_one() {
    let f = fixture();
    let (key, state) = f.create_with("empty", SystemDisk::Empty { gb: 1 });
    assert_eq!(state, OperationState::Succeeded);
    let s = f.core.open_machine(&key).unwrap().settings().unwrap();
    assert_eq!((s.drives.len(), s.drives[0].target), (1, 1));
    assert_eq!((s.disk_target, s.cd_target), (Some(2), Some(4)));
}

#[test]
fn an_image_is_used_in_place_or_copied_into_the_machines_folder() {
    let f = fixture();
    let image = f.root.path().join("irix.img");
    std::fs::write(&image, irix_disk()).unwrap();
    let place = |copy| SystemDisk::Image {
        file: image.clone(),
        copy,
        overlay: false,
    };
    let (key, state) = f.create_with("in-place", place(false));
    assert_eq!(state, OperationState::Succeeded);
    let drives = f
        .core
        .open_machine(&key)
        .unwrap()
        .settings()
        .unwrap()
        .drives;
    // Windows temp paths can come back in their short 8.3 form, so compare files, not text.
    let canon = |p: &str| std::fs::canonicalize(p).unwrap();
    assert_eq!(canon(&drives[0].image), canon(&image.to_string_lossy()));
    let (key, state) = f.create_with("copied", place(true));
    assert_eq!(state, OperationState::Succeeded);
    let drives = f
        .core
        .open_machine(&key)
        .unwrap()
        .settings()
        .unwrap()
        .drives;
    assert_eq!(drives[0].image, "drives/irix.img");
    assert_eq!(
        std::fs::read(f.root.path().join("copied/drives/irix.img")).unwrap(),
        irix_disk()
    );
}

#[test]
fn a_cancelled_copy_leaves_no_file_and_a_machine_with_no_disk() {
    let f = fixture();
    let dir = f.root.path().join("half");
    let (go, wait) = channel::<()>();
    let slot = super::event::Dispatcher::new(None).slot(None);
    let src = f.root.path().join("src.img");
    std::fs::write(&src, irix_disk()).unwrap();
    let to = dir.clone();
    let op = super::operation::spawn(1, "copy", None, true, slot, move |control| {
        wait.recv().unwrap();
        super::disks::copy_into(&src, &to, control).map(|_| None)
    });
    op.cancel();
    go.send(()).unwrap();
    assert_eq!(op.wait(Some(WAIT)), OperationState::Cancelled);
    assert_eq!(std::fs::read_dir(dir.join("drives")).unwrap().count(), 0);
}

#[test]
fn an_image_is_attached_with_progress_and_refused_while_running() {
    let f = fixture();
    let key = f.create("add", "fake.1");
    let machine = f.core.open_machine(&key).unwrap();
    let image = f.root.path().join("scratch.img");
    std::fs::write(&image, irix_disk()).unwrap();
    let request = crate::DriveRequest {
        file: image,
        kind: crate::DriveKind::Disk,
        target: 2,
        overlay: false,
    };
    let op = machine.import_drive(request.clone(), true).unwrap();
    assert_eq!(op.wait(Some(WAIT)), OperationState::Succeeded);
    assert_eq!(op.fraction(), Some(1.0));
    assert!(op.progress().contains("of"));
    let drives = machine.settings().unwrap().drives;
    assert_eq!(
        (drives[0].target, drives[0].image.as_str()),
        (2, "drives/scratch.img")
    );
    machine.start().unwrap().wait(Some(WAIT));
    assert_eq!(
        machine.import_drive(request, true).err().unwrap().kind,
        ErrorKind::Busy
    );
}

#[test]
fn the_cd_is_swapped_and_ejected_on_a_stopped_machine() {
    let f = fixture();
    let machine = f.core.open_machine(&f.create("cd", "fake.1")).unwrap();
    let (a, b) = (f.root.path().join("a.iso"), f.root.path().join("b.iso"));
    std::fs::write(&a, [1u8; 512]).unwrap();
    std::fs::write(&b, [2u8; 512]).unwrap();
    let cd = |image: &Path| crate::DriveRequest {
        file: image.into(),
        kind: crate::DriveKind::Cdrom,
        target: 4,
        overlay: false,
    };
    machine.apply_change(&Change::AttachDrive(cd(&a))).unwrap();
    machine.change_media("cdrom4", Some(&b)).unwrap();
    let drives = machine.settings().unwrap().drives;
    assert_eq!(drives.len(), 1);
    assert!(drives[0].image.ends_with("b.iso"));
    machine.change_media("cdrom4", None).unwrap();
    assert!(machine.settings().unwrap().drives.is_empty());
    assert_eq!(
        machine.change_media("cdrom4", None).unwrap_err().kind,
        ErrorKind::NotFound
    );
}

#[test]
fn a_disk_is_named_by_its_operating_system_and_read_again_when_it_changes() {
    let f = fixture();
    let image = f.root.path().join("irix.img");
    std::fs::write(&image, irix_disk()).unwrap();
    assert_eq!(f.core.disk_info(&image).unwrap(), "IRIX 6.5.30");
    std::fs::write(
        &image,
        include_bytes!("../../../../tests/fixtures/machines/disk-openbsd-6.9.img"),
    )
    .unwrap();
    assert_eq!(f.core.disk_info(&image).unwrap(), "OpenBSD 6.9");
    std::fs::write(&image, vec![0u8; 1 << 16]).unwrap();
    assert_eq!(f.core.disk_info(&image).unwrap(), "Unknown");
    assert_eq!(
        f.core
            .disk_info(&f.root.path().join("none"))
            .unwrap_err()
            .kind,
        ErrorKind::NotFound
    );
}

#[test]
fn the_library_row_learns_the_system_disk_in_the_background() {
    let f = fixture();
    let image = f.root.path().join("irix.img");
    std::fs::write(&image, irix_disk()).unwrap();
    let (key, _) = f.create_with(
        "row",
        SystemDisk::Image {
            file: image,
            copy: false,
            overlay: false,
        },
    );
    let machine = f.core.open_machine(&key).unwrap();
    let deadline = std::time::Instant::now() + WAIT;
    let label = loop {
        let row = f.core.list_machines().unwrap().remove(0);
        if row.system_disk.is_some() || std::time::Instant::now() > deadline {
            break row.system_disk;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(label.as_deref(), Some("IRIX 6.5.30"));
    assert_eq!(machine.drive_info("disk1").unwrap(), "IRIX 6.5.30");
}

#[test]
fn an_image_check_reports_size_room_and_a_machine_that_already_uses_it() {
    let f = fixture();
    let image = f.root.path().join("irix.img");
    std::fs::write(&image, irix_disk()).unwrap();
    let check = f.core.check_disk_image(&image, None).unwrap();
    assert_eq!(
        (check.bytes, check.is_chd, check.used_by.clone()),
        (irix_disk().len() as u64, false, None)
    );
    assert!(check.free_bytes > 0 && check.problem.is_none());
    f.create_with(
        "user",
        SystemDisk::Image {
            file: image.clone(),
            copy: false,
            overlay: false,
        },
    );
    assert_eq!(
        f.core
            .check_disk_image(&image, None)
            .unwrap()
            .used_by
            .as_deref(),
        Some("user")
    );
    let key = f.root.path().join("user").to_string_lossy().into_owned();
    assert_eq!(
        f.core.check_disk_image(&image, Some(&key)).unwrap().used_by,
        None
    );
    let chd = f.root.path().join("bad.chd");
    std::fs::write(&chd, b"not a chd").unwrap();
    assert!(f
        .core
        .check_disk_image(&chd, None)
        .unwrap()
        .problem
        .unwrap()
        .contains("not a CHD"));
}

#[test]
fn an_image_verdict_says_the_same_words_for_every_app() {
    let check = |used_by: Option<&str>, problem: Option<&str>| DiskImageCheck {
        bytes: 9_000_000_000,
        is_chd: false,
        free_bytes: 1_200_000_000,
        used_by: used_by.map(Into::into),
        problem: problem.map(Into::into),
    };
    let v = image_verdict(&check(None, None), true);
    assert!(!v.ready && v.warning);
    assert_eq!(
        v.note.unwrap(),
        "A copy needs 9.0 GB and 1.2 GB is free. Use the disk where it is."
    );
    let v = image_verdict(&check(None, None), false);
    assert!(
        v.ready
            && v.warning
            && v.note
                .unwrap()
                .starts_with("IRIX will write into this file")
    );
    let v = image_verdict(&check(Some("Fuel"), None), false);
    assert_eq!(
        v.note.unwrap(),
        "Fuel already uses this file. Two machines writing to one disk will corrupt it."
    );
    assert!(!image_verdict(&check(None, Some("not a CHD")), false).ready);
    let roomy = DiskImageCheck {
        free_bytes: u64::MAX,
        is_chd: true,
        ..check(None, None)
    };
    assert!(image_verdict(&roomy, true)
        .note
        .unwrap()
        .ends_with("keeps your original safe."));
    assert_eq!(image_verdict(&roomy, false).note, None);
}

#[test]
fn a_missing_system_disk_starts_no_read_and_raises_no_event() {
    let recorder = Arc::new(Recorder::default());
    let f = fixture_with(Some(recorder.clone()), 1 << 20);
    let image = f.root.path().join("gone.img");
    std::fs::write(&image, irix_disk()).unwrap();
    f.create_with(
        "row",
        SystemDisk::Image {
            file: image.clone(),
            copy: false,
            overlay: false,
        },
    );
    std::fs::remove_file(&image).unwrap();
    std::thread::sleep(Duration::from_millis(300));
    let before = recorder.events.lock().unwrap().len();
    assert_eq!(f.core.list_machines().unwrap()[0].system_disk, None);
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(recorder.events.lock().unwrap().len(), before);
}

#[test]
fn a_create_whose_disk_fails_leaves_no_machine_and_a_file_that_is_not_one_is_refused() {
    let f = fixture();
    let folder = f.root.path().join("not-a-file");
    std::fs::create_dir(&folder).unwrap();
    let (_, state) = f.create_with(
        "bad",
        SystemDisk::Image {
            file: folder.clone(),
            copy: true,
            overlay: false,
        },
    );
    assert!(matches!(state, OperationState::Failed(_)), "{state:?}");
    assert!(!f.root.path().join("bad").exists());
    assert!(f.core.check_disk_image(&folder, None).is_err());
}

#[test]
fn a_cached_ready_disk_is_copied_into_the_machine_and_the_cache_is_left_alone() {
    let cache = tempfile::tempdir().unwrap();
    // Nothing else in this test binary reads the cache directory.
    std::env::set_var("ORIGAMI_CACHE_DIR", cache.path());
    let image = irix_disk();
    let sha = origami_machine::sha256_hex(&image);
    let folder = cache.path().join("disks").join(&sha);
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("a.img"), &image).unwrap();
    std::fs::write(
        cache.path().join("disks/index.json"),
        format!(
            r#"{{"format":1,"disks":[{{"name":"irix/a.img","url":"https://origami-dist.irix.fans/irix/a.img",
            "sha256":"{sha}","size":{},"version":"6.5.30","board":"ip30","machines":["fake"]}}]}}"#,
            image.len()
        ),
    )
    .unwrap();
    let f = fixture();
    // A disk for another product is refused, and the refusal leaves no machine.
    let other = std::fs::read_to_string(cache.path().join("disks/index.json")).unwrap();
    std::fs::write(
        cache.path().join("disks/index.json"),
        other.replace(r#""fake""#, r#""indy""#),
    )
    .unwrap();
    let (_, state) = f.create_with(
        "wrong",
        SystemDisk::Ready {
            name: "irix/a.img".into(),
        },
    );
    assert!(matches!(state, OperationState::Failed(_)), "{state:?}");
    assert!(!f.root.path().join("wrong").exists());
    std::fs::write(cache.path().join("disks/index.json"), other).unwrap();
    let (key, state) = f.create_with(
        "ready",
        SystemDisk::Ready {
            name: "irix/a.img".into(),
        },
    );
    assert_eq!(state, OperationState::Succeeded);
    let s = f.core.open_machine(&key).unwrap().settings().unwrap();
    assert_eq!(s.drives.len(), 1);
    assert!(std::path::Path::new(&key).join("drives/a.img").is_file());
    assert_eq!(std::fs::read(folder.join("a.img")).unwrap(), image);
    let cached = f.core.cached_disks().unwrap();
    assert_eq!(cached.len(), 1);
    assert_eq!(cached[0].label, "IRIX 6.5.30 (a.img)");
    f.core.remove_cached_disk(&sha).unwrap();
    assert!(f.core.cached_disks().unwrap().is_empty());
}
