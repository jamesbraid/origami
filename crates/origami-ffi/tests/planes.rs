//! The control plane and the C frame plane against the fake engine.
#![cfg(all(unix, feature = "fake"))]

mod common;

use common::*;
use origami_ffi::*;
use std::ffi::{c_void, CString};
use std::ptr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

#[derive(Default)]
struct Recorder {
    events: Mutex<Vec<OrigamiEvent>>,
    changed: Condvar,
}

impl OrigamiObserver for Recorder {
    fn on_event(&self, event: OrigamiEvent) {
        self.events.lock().unwrap().push(event);
        self.changed.notify_all();
    }
}

fn readable(fd: isize) -> bool {
    let mut p = libc::pollfd {
        fd: fd as i32,
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: one valid pollfd.
    unsafe { libc::poll(&mut p, 1, 0) > 0 }
}

fn take(
    m: *const origami_machine,
    pixels: &mut [u8],
    runs: &mut [origami_run],
) -> (origami_status, origami_frame) {
    let mut out = origami_frame {
        size: std::mem::size_of::<origami_frame>() as u32,
        kind: 9,
        seq: 0,
        width: 0,
        height: 0,
        resized: 0,
        run_count: 0,
        bytes_used: 0,
    };
    // SAFETY: buffers are writable for their lengths.
    let status = unsafe {
        origami_frame_take(
            m,
            0,
            pixels.as_mut_ptr(),
            pixels.len(),
            runs.as_mut_ptr(),
            runs.len(),
            &mut out,
        )
    };
    (status, out)
}

#[test]
fn the_control_plane_lists_starts_and_reports_an_event() {
    let recorder = Arc::new(Recorder::default());
    let lab = Lab::new(Some(recorder.clone()));
    let list = lab.core.list_machines().unwrap();
    assert_eq!((list.len(), list[0].name.as_str()), (1, "m"));
    assert_eq!(lab.core.engines()[0].id, "fake");
    let machine = lab.core.start_machine(lab.key.clone()).unwrap();
    assert_eq!(machine.view().state, OrigamiMachineState::Running);
    let guard = recorder.events.lock().unwrap();
    let (guard, timeout) = recorder
        .changed
        .wait_timeout_while(guard, Duration::from_secs(10), |e| {
            !e.iter().any(|e| {
                e.machine.as_deref() == Some(&lab.key) && e.kinds.contains(&OrigamiEventKind::State)
            })
        })
        .unwrap();
    drop(guard);
    assert!(!timeout.timed_out());
    // Errors arrive as the kind's variant.
    assert!(matches!(
        lab.core.open_machine("nope".into()),
        Err(CoreError::NotFound { .. })
    ));
    assert!(matches!(machine.start(), Err(CoreError::Busy { .. })));
    lab.core.shutdown();
}

#[test]
fn heads_take_wake_handle_and_callback_follow_the_published_frames() {
    let lab = Lab::new(None);
    let m = lab.c_machine();
    let mut head = origami_head_info {
        width: 0,
        height: 0,
        seq: 0,
        active: 0,
        reserved: 0,
    };
    let mut count = 0;
    // SAFETY: valid pointers.
    unsafe { assert_eq!(origami_frame_heads(m, &mut head, 1, &mut count), ORIGAMI_OK) };
    assert_eq!(
        (count, head.width, head.height, head.active),
        (1, 64, 48, 1)
    );

    let mut fd = 0isize;
    // SAFETY: valid pointer.
    unsafe { assert_eq!(origami_frame_wake_handle(m, &mut fd), ORIGAMI_OK) };
    let mut again = 0isize;
    // SAFETY: valid pointer.
    unsafe { origami_frame_wake_handle(m, &mut again) };
    assert_eq!(fd, again);
    let calls = Arc::new(AtomicU32::new(0));
    extern "C" fn bump(user: *mut c_void) {
        // SAFETY: points at an AtomicU32 that outlives the callback.
        unsafe { &*(user as *const AtomicU32) }.fetch_add(1, Ordering::SeqCst);
    }
    // SAFETY: the counter outlives the callback, which is removed below.
    unsafe {
        assert_eq!(
            origami_frame_set_wake(m, Some(bump), Arc::as_ptr(&calls) as *mut c_void),
            ORIGAMI_OK
        )
    };

    // Signalled once on being set up, since a frame may already be waiting;
    // then quiet while nothing is published.
    assert!(readable(fd));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    // SAFETY: valid handle.
    unsafe { origami_frame_wake_clear(m) };
    assert!(!readable(fd));

    lab.advance();
    assert!(readable(fd));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let mut pixels = vec![0u8; 64 * 48 * 4];
    let mut runs = [origami_run { y0: 0, y1: 0 }; 48];
    // SAFETY: valid handle.
    unsafe { origami_frame_wake_clear(m) };
    assert!(!readable(fd));
    let (status, frame) = take(m, &mut pixels, &mut runs);
    assert_eq!(status, ORIGAMI_OK);
    assert_eq!(
        (frame.kind, frame.width, frame.height, frame.run_count),
        (ORIGAMI_FRAME_ROWS, 64, 48, 1)
    );
    assert_eq!(
        (runs[0].y0, runs[0].y1, frame.bytes_used),
        (0, 48, 64 * 48 * 4)
    );
    // Pixel (x=3, y=2) at the frame's sequence number is [seq, y, x, 0].
    let at = (2 * 64 + 3) * 4;
    assert_eq!(pixels[at..at + 4], [frame.seq as u8, 2, 3, 0]);
    let (status, frame) = take(m, &mut pixels, &mut runs);
    assert_eq!((status, frame.kind), (ORIGAMI_OK, ORIGAMI_FRAME_NONE));

    // No callback after it is removed.
    // SAFETY: valid handle.
    unsafe { assert_eq!(origami_frame_set_wake(m, None, ptr::null_mut()), ORIGAMI_OK) };
    lab.advance();
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert!(readable(fd));
    // SAFETY: released once.
    unsafe { origami_machine_release(m) };
}

#[test]
fn the_first_wake_may_clear_the_handle_without_deadlocking() {
    let lab = Lab::new(None);
    let m = lab.c_machine();
    let mut fd = 0isize;
    // SAFETY: valid pointer.
    unsafe { assert_eq!(origami_frame_wake_handle(m, &mut fd), ORIGAMI_OK) };
    extern "C" fn clear(user: *mut c_void) {
        // SAFETY: `user` is the machine handle, alive for the test.
        unsafe { origami_frame_wake_clear(user as *const origami_machine) };
    }
    // SAFETY: the machine outlives the callback, which is removed below.
    unsafe {
        assert_eq!(
            origami_frame_set_wake(m, Some(clear), m as *mut c_void),
            ORIGAMI_OK
        );
        assert_eq!(origami_frame_set_wake(m, None, ptr::null_mut()), ORIGAMI_OK);
        origami_machine_release(m);
    }
}

#[test]
fn a_buffer_that_is_too_small_loses_nothing() {
    let lab = Lab::new(None);
    let m = lab.c_machine();
    let mut small = vec![0u8; 100];
    let mut runs = [origami_run { y0: 0, y1: 0 }; 48];
    let (status, frame) = take(m, &mut small, &mut runs);
    assert_eq!(status, ORIGAMI_ERR_BUFFER_TOO_SMALL);
    assert_eq!(frame.bytes_used, 64 * 48 * 4);
    let mut pixels = vec![0u8; frame.bytes_used as usize];
    let (status, frame) = take(m, &mut pixels, &mut runs);
    assert_eq!(
        (status, frame.kind, frame.run_count),
        (ORIGAMI_OK, ORIGAMI_FRAME_ROWS, 1)
    );
    // SAFETY: released once.
    unsafe { origami_machine_release(m) };
}

#[test]
fn input_and_release_all_reach_the_engine_and_bad_arguments_are_refused() {
    let lab = Lab::new(None);
    let m = lab.c_machine();
    let key = |code: origami_key, down| origami_input_event {
        kind: ORIGAMI_INPUT_KEY,
        code: code as u32,
        down,
        head: 0,
        x: 0,
        y: 0,
    };
    let events = [key(origami_key::ControlLeft, 1), key(origami_key::C, 1)];
    // SAFETY: valid pointers.
    unsafe {
        assert_eq!(
            origami_input_send(m, events.as_ptr(), events.len()),
            ORIGAMI_OK
        );
        let mut released = 0;
        assert_eq!(origami_input_release_all(m, &mut released), ORIGAMI_OK);
        assert_eq!(released, 2);
        let bad = origami_input_event {
            kind: 77,
            ..events[0]
        };
        assert_eq!(origami_input_send(m, &bad, 1), ORIGAMI_ERR_INVALID_ARGUMENT);
        assert!(last_error().contains("unknown input kind"));
        assert_eq!(
            origami_input_send(ptr::null(), events.as_ptr(), 1),
            ORIGAMI_ERR_NULL_ARGUMENT
        );
        let ghost = CString::new("ghost").unwrap();
        let mut none = ptr::null();
        assert_eq!(
            origami_machine_open(lab.c_core, ghost.as_ptr(), &mut none),
            ORIGAMI_ERR_NOT_FOUND
        );
        origami_machine_release(m);
    }
}

#[test]
fn the_fake_engine_reports_the_input_that_reached_it() {
    let lab = Lab::new(None);
    assert!(lab.core.fake_input(lab.key.clone()).is_err());
    let machine = lab.core.start_machine(lab.key.clone()).unwrap();
    machine.send_key(OrigamiKey::C, true).unwrap();
    assert_eq!(
        lab.core.fake_input(lab.key.clone()).unwrap(),
        ["Key { key: C, down: true }"]
    );
}

#[test]
fn the_control_plane_hands_a_machine_to_the_c_plane() {
    let lab = Lab::new(None);
    let machine = lab.core.start_machine(lab.key.clone()).unwrap();
    let handle = machine.frame_plane_handle() as usize as *const origami_machine;
    let mut count = 0;
    // SAFETY: the handle is a live retained machine.
    unsafe {
        assert_eq!(
            origami_frame_heads(handle, ptr::null_mut(), 0, &mut count),
            ORIGAMI_OK
        );
        assert_eq!(count, 1);
        origami_machine_release(handle);
    }
}

#[test]
fn views_report_what_they_cannot_do_instead_of_crashing() {
    let lab = Lab::new(None);
    let m = lab.c_machine();
    let surface = origami_surface {
        size: std::mem::size_of::<origami_surface>() as u32,
        kind: ORIGAMI_SURFACE_WIN32_SWAPCHAIN_PANEL,
        window: std::ptr::dangling_mut::<c_void>(),
        display: ptr::null_mut(),
        window_id: 0,
        screen: 0,
    };
    let mut view = ptr::null_mut();
    // SAFETY: valid pointers.
    let status = unsafe { origami_view_attach(m, 0, &surface, 64, 48, 1.0, &mut view) };
    assert_eq!(status, ORIGAMI_ERR_UNSUPPORTED);
    assert!(view.is_null());
    // SAFETY: valid arguments.
    unsafe {
        assert_eq!(
            origami_view_present(ptr::null_mut(), ptr::null_mut()),
            ORIGAMI_ERR_NULL_ARGUMENT
        );
        origami_view_detach(ptr::null_mut());
        origami_machine_release(m);
    }
}

#[test]
fn a_create_may_name_an_engine() {
    let lab = Lab::new(None);
    let prom = lab.root.path().join("prom.bin");
    let create = |name: &str, engine: Option<&str>| {
        lab.core
            .create_machine(OrigamiCreateRequest {
                name: name.into(),
                offering: "fake.1".into(),
                memory_per_node_mib: None,
                graphics: None,
                boot_prom: Some(prom.to_str().unwrap().into()),
                io_prom: None,
                engine: engine.map(Into::into),
                system_disk: None,
                identity: None,
            })
            .unwrap()
            .wait(Some(10_000))
    };
    assert_eq!(create("a", None), OrigamiOperationState::Succeeded);
    assert_eq!(create("b", Some("fake")), OrigamiOperationState::Succeeded);
    assert!(matches!(
        create("c", Some("nope")),
        OrigamiOperationState::Failed {
            kind: OrigamiErrorKind::NotFound,
            ..
        }
    ));
}

#[test]
fn a_create_with_an_unknown_ready_disk_fails_and_leaves_no_machine() {
    let lab = Lab::new(None);
    let prom = lab.root.path().join("prom.bin");
    let state = lab
        .core
        .create_machine(OrigamiCreateRequest {
            name: "ready".into(),
            offering: "fake.1".into(),
            memory_per_node_mib: None,
            graphics: None,
            boot_prom: Some(prom.to_str().unwrap().into()),
            io_prom: None,
            engine: None,
            system_disk: Some(OrigamiSystemDisk::Ready {
                name: "irix/none.chd".into(),
            }),
            identity: None,
        })
        .unwrap()
        .wait(Some(10_000));
    assert!(
        matches!(state, OrigamiOperationState::Failed { .. }),
        "{state:?}"
    );
    assert!(!lab.root.path().join("ready").exists());
}
