//! The wake path allocates nothing per frame, and taking a frame costs far
//! less than the plan's 0.5 ms of CPU per frame. A counting global allocator
//! makes this its own test binary.
#![cfg(all(unix, feature = "fake"))]

use origami_core::facade::{Config, Core, CreateRequest, OperationState};
use origami_engine::fake::FakeEngine;
use origami_ffi::*;
use std::alloc::{GlobalAlloc, Layout, System};
use std::ffi::c_void;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

struct Counting;
static ALLOCS: AtomicU64 = AtomicU64::new(0);

// SAFETY: forwards to the system allocator.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        // SAFETY: forwarded.
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: forwarded.
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new: usize) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        // SAFETY: forwarded.
        unsafe { System.realloc(ptr, layout, new) }
    }
}

#[global_allocator]
static A: Counting = Counting;

extern "C" fn ignore(_: *mut c_void) {}

#[test]
fn publishing_a_frame_allocates_nothing_and_taking_one_is_cheap() {
    const PUBLISHED: u64 = 20_000;
    const TAKEN: u32 = 2_000;
    let root = tempfile::tempdir().unwrap();
    let engine = Arc::new(FakeEngine::new());
    let core = Core::new(Config::new(root.path()), vec![engine.clone()], None).unwrap();
    let prom = root.path().join("prom.bin");
    std::fs::write(&prom, [0u8; 16]).unwrap();
    let op = core
        .create_machine(CreateRequest {
            name: "m".into(),
            offering: "fake.1".into(),
            boot_prom: Some(prom),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(op.wait(None), OperationState::Succeeded);
    let machine = core.start_machine(&op.result_machine().unwrap()).unwrap();
    let session = engine
        .session_in(std::path::Path::new(machine.key()))
        .unwrap();
    let m = retain_machine(&machine);

    let mut fd = 0isize;
    // SAFETY: valid arguments; the callback does nothing.
    unsafe {
        assert_eq!(origami_frame_wake_handle(m, &mut fd), ORIGAMI_OK);
        assert_eq!(
            origami_frame_set_wake(m, Some(ignore), std::ptr::null_mut()),
            ORIGAMI_OK
        );
    }
    session.advance_frame(); // first use: lazy statics and thread locals allocate here
                             // Let the console thread take the banner and the start operation finish,
                             // so only the wake path runs while allocations are counted.
    while machine.view().console.total == 0 {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    std::thread::sleep(std::time::Duration::from_millis(200));

    let before = ALLOCS.load(Ordering::Relaxed);
    for _ in 0..PUBLISHED {
        session.advance_frame();
    }
    let allocated = ALLOCS.load(Ordering::Relaxed) - before;
    assert_eq!(
        allocated, 0,
        "the wake path allocated {allocated} times for {PUBLISHED} frames"
    );

    let mut pixels = vec![0u8; 64 * 48 * 4];
    let mut runs = [origami_run { y0: 0, y1: 0 }; 48];
    let mut out = origami_frame {
        size: std::mem::size_of::<origami_frame>() as u32,
        kind: 0,
        seq: 0,
        width: 0,
        height: 0,
        resized: 0,
        run_count: 0,
        bytes_used: 0,
    };
    // The fake engine fills the picture pixel by pixel, which dominates a
    // debug build, so measure the plane's cost over the facade's own take.
    let mut direct = Vec::new();
    let (mut engine_time, mut plane_time) = (std::time::Duration::ZERO, std::time::Duration::ZERO);
    let before = ALLOCS.load(Ordering::Relaxed);
    for _ in 0..TAKEN {
        session.advance_frame();
        let start = Instant::now();
        assert!(machine.take_frame(0, &mut direct).unwrap().is_some());
        engine_time += start.elapsed();
        session.advance_frame();
        let start = Instant::now();
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
        plane_time += start.elapsed();
        assert_eq!((status, out.kind), (ORIGAMI_OK, ORIGAMI_FRAME_ROWS));
    }
    // The fake engine allocates the run list of each frame; the plane adds none.
    let per_take = (ALLOCS.load(Ordering::Relaxed) - before) / u64::from(TAKEN) / 2;
    assert!(per_take <= 1, "{per_take} allocations per take_frame");
    let overhead = plane_time.saturating_sub(engine_time) / TAKEN;
    assert!(
        overhead.as_micros() < 50,
        "origami_frame_take adds {overhead:?} per frame over the facade (plan budget: 500us per frame in all)"
    );
    // SAFETY: released once.
    unsafe { origami_machine_release(m) };
}
