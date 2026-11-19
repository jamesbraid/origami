//! An iris Indy booted from the published IRIX 5.3 disk, seen through the C
//! frame plane and compared with iris's own screendump. Ignored by default;
//! the variables and the command are in dev/development.md.
#![cfg(all(
    target_os = "linux",
    feature = "iris",
    feature = "render",
    not(feature = "fake")
))]
#![allow(clippy::unnecessary_cast)]

mod boot_rig;

use boot_rig::*;
use origami_ffi::*;
use origami_protocol::{Client, Request};
use std::ffi::CString;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::ptr;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

/// iris's own screendump, through the runner's control socket.
fn iris_dump(port: u16, to: &Path) -> Shot {
    let mut client = Client::connect(port, Duration::from_secs(10)).expect("the runner");
    client
        .call_data_timeout(
            &Request::Screenshot {
                head: 0,
                path: to.display().to_string(),
            },
            Duration::from_secs(40),
        )
        .expect("iris screendump");
    ppm(&std::fs::read(to).unwrap())
}

/// Present on every wake for `time`.
fn pump(rig: &mut Rig, time: Duration) {
    let t = Instant::now();
    while t.elapsed() < time {
        if rig.wait(50) {
            rig.present();
        }
    }
}

/// The pixels where the window differs from iris's screendump taken right
/// after it, the fewest of many tries: the caret can blink between the two.
fn same_as_iris(rig: &mut Rig, dump: &dyn Fn(&Path) -> Shot, work: &Path, name: &str) -> usize {
    let mut fewest = usize::MAX;
    for _ in 0..30 {
        pump(rig, Duration::from_millis(150));
        rig.flush();
        let (_, _, got) = shot(rig.window, &work.join(format!("{name}-window.ppm")));
        let (_, _, want) = dump(&work.join(format!("{name}.ppm")));
        fewest = fewest.min(differing(&got, &want).len());
        if fewest == 0 {
            break;
        }
    }
    fewest
}

fn colours(rgb: &[u8]) -> usize {
    rgb.chunks(3)
        .collect::<std::collections::HashSet<_>>()
        .len()
}

#[test]
#[ignore = "starts a machine; see the header"]
fn an_iris_indy_boots_into_the_window_without_the_cpu_touching_a_pixel() {
    use origami_core::facade::{Config, Core, CreateRequest, OperationState};
    let bin = var("ORIGAMI_BIN");
    let work = var("ORIGAMI_GUI_TEST_DIR").join(format!("iris-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    let root = work.join("library");
    std::fs::create_dir_all(&root).unwrap();

    // The machine is made through the core; the C plane below opens it.
    let engine = origami_iris::IrisEngine::new(&bin);
    let core = Core::new(Config::new(&root), vec![std::sync::Arc::new(engine)], None).unwrap();
    let made = core
        .create_machine(CreateRequest {
            name: "indy".into(),
            offering: "indy.1".into(),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(made.wait(None), OperationState::Succeeded);
    drop(core);
    let machine_dir = root.join("indy").canonicalize().unwrap();

    let source = var("ORIGAMI_TEST_DISK");
    let hash = {
        let out = Command::new("sha256sum").arg(&source).output().unwrap();
        String::from_utf8_lossy(&out.stdout)
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .to_string()
    };
    eprintln!("MEASURE disk sha256 {hash}");
    if let Ok(want) = std::env::var("ORIGAMI_TEST_DISK_SHA256") {
        assert_eq!(hash, want, "the disk is not the one named");
    }
    let disk = work.join("indy.chd");
    std::fs::copy(&source, &disk).unwrap();
    let attached = Command::new(&bin)
        .args([
            "drive-attach",
            "--type",
            "disk",
            "--target",
            "1",
            "--overlay",
        ])
        .arg(&machine_dir)
        .arg(&disk)
        .output()
        .unwrap();
    assert!(
        attached.status.success(),
        "{}",
        String::from_utf8_lossy(&attached.stderr)
    );
    struct Stop(PathBuf, PathBuf);
    impl Drop for Stop {
        fn drop(&mut self) {
            let _ = Command::new(&self.0)
                .args(["stop", "--force"])
                .arg(&self.1)
                .status();
        }
    }
    let _stop = Stop(bin.clone(), machine_dir.clone());

    let root_c = CString::new(root.to_str().unwrap()).unwrap();
    let key_c = CString::new(machine_dir.to_str().unwrap()).unwrap();
    let mut core = ptr::null_mut();
    let mut machine = ptr::null();
    // SAFETY: valid strings and out pointers.
    unsafe {
        ok(origami_core_new(root_c.as_ptr(), &mut core), "core_new");
        ok(
            origami_machine_open(core, key_c.as_ptr(), &mut machine),
            "open",
        );
        ok(origami_machine_start(machine), "start");
        ok(
            origami_frame_set_wake(machine, Some(wake), ptr::null_mut()),
            "set_wake",
        );
    }
    let mut fd = 0isize;
    // SAFETY: valid machine and out pointer.
    unsafe { ok(origami_frame_wake_handle(machine, &mut fd), "wake_handle") };
    let control_port = ::origami_machine::control::read(&machine_dir)
        .expect("a running machine")
        .control_port;
    let dump = |to: &Path| iris_dump(control_port, to);

    // The head has no size until the PROM sets a mode.
    let started = Instant::now();
    let head = |machine| {
        let mut heads = [origami_head_info {
            width: 0,
            height: 0,
            seq: 0,
            active: 0,
            reserved: 0,
        }; 1];
        let mut n = 0;
        // SAFETY: valid buffer for one head.
        unsafe {
            ok(
                origami_frame_heads(machine, heads.as_mut_ptr(), 1, &mut n),
                "heads",
            )
        };
        (n > 0 && heads[0].active != 0).then_some((heads[0].width, heads[0].height))
    };
    let (w, h) = loop {
        if let Some(size) = head(machine) {
            break size;
        }
        assert!(started.elapsed() < Duration::from_secs(300), "no picture");
        std::thread::sleep(Duration::from_millis(100));
    };
    eprintln!("MEASURE picture {w}x{h} after {:?}", started.elapsed());

    // SAFETY: Xlib calls on a display we open and keep.
    let (dpy, window, screen) = unsafe {
        let dpy = XOpenDisplay(ptr::null());
        assert!(!dpy.is_null(), "no X display; set DISPLAY");
        let screen = XDefaultScreen(dpy);
        let window = XCreateSimpleWindow(dpy, XRootWindow(dpy, screen), 0, 0, w, h, 0, 0, 0);
        XMapWindow(dpy, window);
        XSync(dpy, 0);
        (dpy, window, screen)
    };
    let surface = origami_surface {
        size: std::mem::size_of::<origami_surface>() as u32,
        kind: ORIGAMI_SURFACE_XLIB_WINDOW,
        window: ptr::null_mut(),
        display: dpy,
        window_id: window as u64,
        screen,
    };
    let mut view = ptr::null_mut();
    // SAFETY: the window outlives the view (detached below).
    unsafe {
        ok(
            origami_view_attach(machine, 0, &surface, w, h, 1.0, &mut view),
            "attach",
        )
    };
    let mut rig = Rig {
        machine,
        view,
        window,
        fd: fd as i32,
        presents: 0,
        wall_ms: Vec::new(),
        cpu_ms: Vec::new(),
        ours_us: Vec::new(),
        ours_last_ns: 0,
        last_present: Instant::now(),
    };

    // IRIX boots (the first boot reconfigures once). The login caret blinks,
    // so the picture is never still: pump for a time and compare then.
    let boot = Instant::now();
    let mut next_note = Duration::from_secs(60);
    let desktop = loop {
        pump(&mut rig, Duration::from_secs(5));
        let n = colours(&dump(&work.join("boot.ppm")).2);
        if boot.elapsed() > next_note {
            eprintln!("still booting after {:?}: {n} colours", boot.elapsed());
            next_note += Duration::from_secs(60);
        }
        if n > 40 && boot.elapsed() > Duration::from_secs(120) {
            break n;
        }
        assert!(boot.elapsed() < Duration::from_secs(1500), "no desktop");
    };
    eprintln!(
        "MEASURE the IRIX picture appeared after {:?} ({desktop} colours)",
        boot.elapsed()
    );
    let differ = same_as_iris(&mut rig, &dump, &work, "boot");
    eprintln!(
        "MEASURE window vs iris screendump: {differ} of {} pixels differ",
        w * h
    );
    assert_eq!(differ, 0, "the window is not iris's picture");

    // Keys reach the guest: typing changes the picture, and the window still
    // equals iris's.
    let before = dump(&work.join("before-keys.ppm")).2;
    for code in [
        origami_key::R,
        origami_key::O,
        origami_key::O,
        origami_key::T,
    ] {
        rig.send(&[key(code as u32, true), key(code as u32, false)]);
        std::thread::sleep(Duration::from_millis(150));
    }
    pump(&mut rig, Duration::from_secs(3));
    let after = dump(&work.join("after-keys.ppm")).2;
    let changed = differing(&before, &after).len();
    let differ = same_as_iris(&mut rig, &dump, &work, "keys");
    eprintln!(
        "MEASURE after typing: {changed} pixels differ from before; window vs iris: {differ}"
    );
    assert!(changed > 0, "typing did not reach the guest");
    assert_eq!(differ, 0, "the window is not iris's picture after typing");

    // Capture: the head is a relative pointer, so only motion reaches the guest.
    let (before, cursor0) = (dump(&work.join("before-move.ppm")).2, rig.cursor());
    for _ in 0..12 {
        rig.send(&[move_by(10, 6)]);
        std::thread::sleep(Duration::from_millis(30));
    }
    pump(&mut rig, Duration::from_secs(2));
    let cursor1 = rig.cursor();
    let moved = differing(&before, &dump(&work.join("after-move.ppm")).2).len();
    let differ = same_as_iris(&mut rig, &dump, &work, "moved");
    eprintln!(
        "MEASURE capture: cursor on {} {},{} -> {},{}; {moved} pixels changed; window vs iris: {differ}",
        cursor1.on, cursor0.x, cursor0.y, cursor1.x, cursor1.y
    );
    assert_eq!(differ, 0, "the window is not iris's picture after moving");
    assert!(
        moved > 0 || (cursor1.on != 0 && cursor1.x > cursor0.x),
        "the pointer did not move"
    );

    // Idle: how often a still desktop wakes the viewer.
    pump(&mut rig, Duration::from_secs(4));
    let (wakes0, presents0, cpu0) = (WAKES.load(Ordering::Relaxed), rig.presents, process_cpu());
    let t = Instant::now();
    while t.elapsed() < Duration::from_secs(10) {
        if rig.wait(200) {
            rig.present();
        }
    }
    let wakes = WAKES.load(Ordering::Relaxed) - wakes0;
    let cpu = process_cpu() - cpu0;
    eprintln!(
        "MEASURE idle 10 s: {wakes} wakes, {} presents, {:.1} ms process CPU ({:.2}% of one core)",
        rig.presents - presents0,
        cpu * 1e3,
        cpu * 10.0
    );

    let s = rig.stats();
    let mut ours = rig.ours_us.clone();
    eprintln!(
        "MEASURE our CPU per presented frame: p50 {:.0} us p95 {:.0} us over {} presents; frames {} lines {} (imported {}) CPU-copied picture bytes {}",
        percentile(&mut ours, 0.5),
        percentile(&mut ours, 0.95),
        ours.len(),
        s.frames,
        s.lines,
        s.imported_lines,
        s.copied_bytes
    );
    // SAFETY: valid handles, released once.
    unsafe {
        origami_view_detach(rig.view);
        origami_machine_release(machine);
        origami_core_release(core);
    }
    assert_eq!(s.copied_bytes, 0, "a picture byte was copied by the CPU");
    assert!(s.imported_lines > 0);
}
