//! A real QEMU Octane2 VPro boot through the C frame plane, drawn into an X11
//! window on a real GPU and compared with QEMU's own screendump.
//!
//! Ignored by default: it starts a machine. It needs a Vulkan device, an X
//! server (`DISPLAY`, Xvfb is enough; the window is made as large as the
//! picture so the draw is 1:1), ImageMagick's `import`, and:
//!
//! - ORIGAMI_RUNTIME_DIR: the QEMU runtime tree. A QEMU with the `display-shm`
//!   device runs display protocol 4, an older one protocol 3; the test says
//!   which it found.
//! - ORIGAMI_BIN: the `origami` executable, which creates and runs machines
//! - ORIGAMI_GUI_TEST_DIR: a directory for the machine
//! - ORIGAMI_TEST_DISK (optional): an IRIX disk for the desktop phase, used
//!   only as the read-only backing file of an overlay made here.
//!   ORIGAMI_TEST_DISK_SHA256 names the hash it must have, checked first.
//!
//! `ORIGAMI_GUI_ZERO_COPY=0` runs the copying path. Run it in release mode
//! with `cargo test -p origami-ffi --release --test qemu_boot -- --ignored --nocapture`;
//! the numbers it prints start with `MEASURE`.
#![cfg(all(unix, feature = "qemu", feature = "render", not(feature = "fake")))]
#![allow(clippy::unnecessary_cast)]

mod boot_rig;

use boot_rig::*;
use origami_ffi::*;
use std::ffi::CString;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::ptr;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

/// Run QMP commands, in order, and return when the last is answered.
fn qmp(port: u16, commands: &[String]) {
    let stream = TcpStream::connect(("127.0.0.1", port)).expect("QMP");
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut out = stream;
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    let mut send = |cmd: &str| {
        writeln!(out, "{cmd}").unwrap();
        loop {
            line.clear();
            reader.read_line(&mut line).unwrap();
            if line.contains("\"return\"") {
                return;
            }
            assert!(!line.contains("\"error\""), "{cmd}: {line}");
        }
    };
    send(r#"{"execute":"qmp_capabilities"}"#);
    for c in commands {
        send(c);
    }
}

/// QEMU's own idea of the screen: a QMP `screendump`.
fn screendump(qmp_port: u16, to: &Path) -> Shot {
    qmp(
        qmp_port,
        &[format!(
            r#"{{"execute":"screendump","arguments":{{"filename":"{}","format":"ppm"}}}}"#,
            to.display()
        )],
    );
    let mut bytes = Vec::new();
    std::fs::File::open(to)
        .unwrap()
        .read_to_end(&mut bytes)
        .unwrap();
    ppm(&bytes)
}

/// The disk's SHA-256, by `sha256sum`.
fn sha256_of(path: &Path) -> String {
    let out = Command::new("sha256sum")
        .arg(path)
        .output()
        .expect("sha256sum");
    assert!(out.status.success(), "sha256sum failed");
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_string()
}

/// The display protocol the runtime's QEMU speaks: 4 when it has the
/// `display-shm` device.
fn protocol() -> u32 {
    let qemu = var("ORIGAMI_RUNTIME_DIR").join("qemu-system-mips64");
    let out = Command::new(qemu)
        .args(["-device", "help"])
        .output()
        .expect("qemu -device help");
    if String::from_utf8_lossy(&out.stdout).contains("\"display-shm\"") {
        4
    } else {
        3
    }
}

/// Where the red arrow of IRIX's pointer is in a screendump: the top left of
/// its red pixels. Used when the guest's cursor is in the picture itself.
fn find_arrow(rgb: &[u8], w: u32) -> Option<(i32, i32)> {
    let red = |p: &[u8]| p[0] > 200 && p[1] < 60 && p[2] < 60;
    let (mut best, mut found) = ((i32::MAX, i32::MAX), false);
    for (i, p) in rgb.chunks(3).enumerate() {
        if red(p) {
            let (x, y) = ((i as u32 % w) as i32, (i as u32 / w) as i32);
            if y < best.1 || (y == best.1 && x < best.0) {
                best = (x, y);
            }
            found = true;
        }
    }
    found.then_some(best)
}

/// An IRIX desktop on the Octane2: the pointer in both of its modes reaching
/// the guest, the cursor drawn, and the cost of a moving cursor.
fn irix_phase(rig: &mut Rig, qmp_port: u16, work: &Path) {
    let boot = Instant::now();
    let mut next_note = Duration::from_secs(60);
    // The login screen has thousands of colours; the PROM and the boot text
    // have a few hundred at most.
    loop {
        rig.settle(Duration::from_secs(5), Duration::from_secs(1500));
        let (_, _, want) = screendump(qmp_port, &work.join("boot.ppm"));
        let colours: std::collections::HashSet<&[u8]> = want.chunks(3).collect();
        if colours.len() > 1000 {
            break;
        }
        if boot.elapsed() > next_note {
            eprintln!(
                "still booting after {:?}: {} colours",
                boot.elapsed(),
                colours.len()
            );
            next_note += Duration::from_secs(60);
        }
        assert!(
            boot.elapsed() < Duration::from_secs(1500),
            "no desktop after 25 minutes"
        );
    }
    eprintln!(
        "MEASURE the IRIX desktop appeared after {:?}",
        boot.elapsed()
    );
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
            origami_frame_heads(rig.machine, heads.as_mut_ptr(), 1, &mut n),
            "heads",
        )
    };
    let w = heads[0].width;
    let c = rig.cursor();
    let baked = c.on == 0;
    eprintln!(
        "MEASURE frame-plane cursor at the desktop: present {} on {} {}x{} hot {},{} at {},{}; picture {}x{}",
        c.present, c.on, c.width, c.height, c.hot_x, c.hot_y, c.x, c.y, w, heads[0].height
    );

    // Follow: absolute positions reach the guest, and the window shows what
    // QEMU shows.
    for (i, (tx, ty)) in [(200, 200), (900, 700), (1100, 100), (300, 900)]
        .into_iter()
        .enumerate()
    {
        rig.send(&[move_to(tx, ty)]);
        let t = Instant::now();
        let at = loop {
            if rig.wait(100) {
                rig.present();
            }
            let (x, y) = if baked {
                let (_, _, want) = screendump(qmp_port, &work.join("probe.ppm"));
                find_arrow(&want, w).unwrap_or((-1000, -1000))
            } else {
                let c = rig.cursor();
                (c.x, c.y)
            };
            if (x - tx).abs() <= 24 && (y - ty).abs() <= 24 {
                break (x, y);
            }
            assert!(
                t.elapsed() < Duration::from_secs(60),
                "the cursor never reached {tx},{ty}; it is at {x},{y}"
            );
        };
        eprintln!(
            "MEASURE follow to {tx},{ty}: cursor at {},{} after {:?}",
            at.0,
            at.1,
            t.elapsed()
        );
        rig.settle(Duration::from_secs(2), Duration::from_secs(60));
        rig.flush();
        let (_, _, got) = shot(rig.window, &work.join(format!("follow{i}-window.ppm")));
        let (_, _, want) = screendump(qmp_port, &work.join(format!("follow{i}.ppm")));
        let diff = differing(&got, &want);
        let c = rig.cursor();
        let (x0, y0) = (c.x - c.hot_x, c.y - c.hot_y);
        let inside = diff.iter().all(|&i| {
            let (x, y) = ((i as u32 % w) as i32, (i as u32 / w) as i32);
            x >= x0 && x < x0 + c.width as i32 && y >= y0 && y < y0 + c.height as i32
        });
        eprintln!("MEASURE follow {i}: {} pixels differ from the screendump (all inside the cursor rectangle: {inside}; cursor on {})", diff.len(), c.on);
        if baked {
            assert!(diff.is_empty(), "the window is not QEMU's picture");
        } else {
            assert!(
                !diff.is_empty() && inside,
                "the cursor is not drawn where the guest has it"
            );
        }
    }

    // Capture: relative motion from the current place.
    let at = |rig: &mut Rig| {
        if baked {
            let (_, _, want) = screendump(qmp_port, &work.join("probe.ppm"));
            find_arrow(&want, w).unwrap()
        } else {
            let c = rig.cursor();
            (c.x, c.y)
        }
    };
    rig.settle(Duration::from_secs(2), Duration::from_secs(60));
    let c0 = at(rig);
    for _ in 0..10 {
        rig.send(&[move_by(8, 5)]);
        std::thread::sleep(Duration::from_millis(20));
    }
    rig.settle(Duration::from_secs(2), Duration::from_secs(60));
    let c1 = at(rig);
    eprintln!(
        "MEASURE capture: 10 moves of (+8,+5) took the cursor from {},{} to {},{}",
        c0.0, c0.1, c1.0, c1.1
    );
    assert!(
        c1.0 > c0.0 && c1.1 > c0.1,
        "relative motion went the wrong way"
    );

    // Idle cost with the cursor moving, then with it still.
    let (moving_from, moving_cpu_from) = (rig.ours_us.len(), rig.cpu_ms.len());
    let ticks0 = thread_ticks();
    let vs0 = rig.view_stats();
    let (wakes0, presents0, cpu0) = (WAKES.load(Ordering::Relaxed), rig.presents, process_cpu());
    let t = Instant::now();
    let mut sent = 0u32;
    while t.elapsed() < Duration::from_secs(10) {
        let d = if (sent / 30).is_multiple_of(2) { 2 } else { -2 };
        rig.send(&[move_by(d, d)]);
        sent += 1;
        if rig.wait(16) {
            rig.present();
        }
    }
    let cpu = process_cpu() - cpu0;
    eprintln!(
        "MEASURE cursor moving 10 s ({sent} moves sent): {} wakes, {} presents, {:.1} ms process CPU ({:.2}% of one core)",
        WAKES.load(Ordering::Relaxed) - wakes0,
        rig.presents - presents0,
        cpu * 1e3,
        cpu / 10.0 * 100.0
    );
    let vs1 = rig.view_stats();
    let n = (vs1.presents - vs0.presents).max(1) as f64;
    eprintln!(
        "MEASURE moving, per present: ours (take+draw) {:.0} us, the window system's present {:.0} us",
        (vs1.take_ns + vs1.draw_ns - vs0.take_ns - vs0.draw_ns) as f64 / n / 1e3,
        (vs1.present_ns - vs0.present_ns) as f64 / n / 1e3
    );
    for (name, t) in &thread_ticks() {
        let d = t - ticks0.get(name).copied().unwrap_or(0);
        if d > 0 {
            eprintln!("MEASURE moving thread {name}: {d} ticks (10 ms each)");
        }
    }
    let mut ours = rig.ours_us[moving_from..].to_vec();
    let mut cpu_ms = rig.cpu_ms[moving_cpu_from..].to_vec();
    eprintln!(
        "MEASURE moving, our CPU per presented frame: p50 {:.0} us p95 {:.0} us; present-call thread CPU p50 {:.3} ms p95 {:.3} ms; over {} presents",
        percentile(&mut ours, 0.5),
        percentile(&mut ours, 0.95),
        percentile(&mut cpu_ms, 0.5),
        percentile(&mut cpu_ms, 0.95),
        ours.len()
    );
    rig.settle(Duration::from_secs(3), Duration::from_secs(60));
    let (wakes0, presents0, cpu0) = (WAKES.load(Ordering::Relaxed), rig.presents, process_cpu());
    let t = Instant::now();
    while t.elapsed() < Duration::from_secs(10) {
        if rig.wait(200) {
            rig.present();
        }
    }
    let cpu = process_cpu() - cpu0;
    let wakes = WAKES.load(Ordering::Relaxed) - wakes0;
    eprintln!(
        "MEASURE idle after the cursor stopped, 10 s: {wakes} wakes, {} presents, {:.1} ms process CPU ({:.2}% of one core)",
        rig.presents - presents0,
        cpu * 1e3,
        cpu / 10.0 * 100.0
    );
    assert_eq!(wakes, 0, "the still desktop woke the viewer");
}

#[test]
#[ignore = "starts a machine; see the header"]
fn an_octane2_boot_reaches_the_window_without_the_cpu_touching_a_pixel() {
    let want_import = std::env::var("ORIGAMI_GUI_ZERO_COPY").map_or(true, |v| v != "0");
    eprintln!("MEASURE display protocol {}", protocol());
    let work = var("ORIGAMI_GUI_TEST_DIR").join(format!("ffi-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).unwrap();
    let machine_dir = work.join("octane2");
    let out = Command::new(var("ORIGAMI_BIN"))
        .arg("create")
        .arg(&machine_dir)
        .args(["--preset", "octane2-vpro"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    // ORIGAMI_TEST_DISK: an IRIX disk, used only as the backing file of an
    // overlay made here, so the original is never written.
    let disk = std::env::var_os("ORIGAMI_TEST_DISK").map(PathBuf::from);
    if let Some(disk) = &disk {
        let hash = sha256_of(disk);
        eprintln!("MEASURE disk sha256 {hash}");
        if let Ok(want) = std::env::var("ORIGAMI_TEST_DISK_SHA256") {
            assert_eq!(hash, want, "the disk is not the one named");
        }
        let overlay = work.join("overlay.qcow2");
        let rt = var("ORIGAMI_RUNTIME_DIR").join("qemu-img");
        let made = Command::new(rt)
            .args(["create", "-f", "qcow2", "-F", "qcow2", "-b"])
            .arg(disk)
            .arg(&overlay)
            .output()
            .unwrap();
        assert!(
            made.status.success(),
            "{}",
            String::from_utf8_lossy(&made.stderr)
        );
        let attached = Command::new(var("ORIGAMI_BIN"))
            .arg("drive-attach")
            .args(["--type", "disk", "--target", "1"])
            .arg(&machine_dir)
            .arg(&overlay)
            .output()
            .unwrap();
        assert!(
            attached.status.success(),
            "{}",
            String::from_utf8_lossy(&attached.stderr)
        );
    }
    let machine_dir = machine_dir.canonicalize().unwrap();
    // The runner outlives this process; end it however the test ends.
    struct Stop(PathBuf, PathBuf);
    impl Drop for Stop {
        fn drop(&mut self) {
            let _ = Command::new(&self.0)
                .args(["stop", "--force"])
                .arg(&self.1)
                .status();
        }
    }
    let _stop = Stop(var("ORIGAMI_BIN"), machine_dir.clone());

    let root = CString::new(work.to_str().unwrap()).unwrap();
    let key_c = CString::new(machine_dir.to_str().unwrap()).unwrap();
    let mut core = ptr::null_mut();
    let mut machine = ptr::null();
    // SAFETY: valid strings and out pointers.
    unsafe {
        ok(origami_core_new(root.as_ptr(), &mut core), "core_new");
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
    let qmp_port = ::origami_machine::control::read(&machine_dir)
        .expect("a running machine")
        .qmp_port;

    // The picture's size, once the guest sets a mode.
    let started = Instant::now();
    let (w, h) = loop {
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
        if n > 0 && heads[0].active != 0 {
            break (heads[0].width, heads[0].height);
        }
        assert!(started.elapsed() < Duration::from_secs(600), "no picture");
        std::thread::sleep(Duration::from_millis(100));
    };
    eprintln!("picture {w}x{h} after {:?}", started.elapsed());

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
        window_id: window as u64, // c_ulong is wider elsewhere
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

    if disk.is_some() {
        irix_phase(&mut rig, qmp_port, &work);
        let s = rig.stats();
        eprintln!(
            "MEASURE IRIX run: frames {} imported lines {} CPU-copied picture bytes {}",
            s.frames, s.imported_lines, s.copied_bytes
        );
        if want_import {
            assert_eq!(s.copied_bytes, 0, "a picture byte was copied by the CPU");
        }
        unsafe {
            origami_view_detach(rig.view);
            origami_machine_release(machine);
            origami_core_release(core);
        }
        return;
    }
    // The PROM runs its diagnostics, then stops on a dialog: wait for a
    // picture with real content that has been still for a while.
    let boot = Instant::now();
    loop {
        rig.settle(Duration::from_secs(4), Duration::from_secs(900));
        let (sw, sh, want) = screendump(qmp_port, &work.join("prom.ppm"));
        let colours: std::collections::HashSet<&[u8]> = want.chunks(3).collect();
        eprintln!("after {:?}: {} colours", boot.elapsed(), colours.len());
        if colours.len() > 100 {
            assert_eq!((sw, sh), (w, h));
            break;
        }
        assert!(
            boot.elapsed() < Duration::from_secs(900),
            "the PROM never drew its dialog"
        );
    }
    let cursor = rig.cursor();
    let (differ, _) = rig.compare(&|to| screendump(qmp_port, to), &work, "prom");
    eprintln!(
        "MEASURE window vs screendump: {differ} of {} pixels differ (cursor on: {})",
        w * h,
        cursor.on
    );
    if cursor.on == 0 {
        assert_eq!(differ, 0, "the window is not QEMU's picture");
    }
    // The readback is what the view drew, so it matches the screendump too.
    let (rw, rh, got) = rig.readback();
    let (_, _, want) = screendump(qmp_port, &work.join("prom-readback.ppm"));
    assert_eq!((rw, rh), (w, h));
    let differ = differing(&got, &want).len();
    eprintln!("MEASURE readback vs screendump: {differ} pixels differ");
    if rig.cursor().on == 0 {
        assert_eq!(differ, 0, "the readback is not QEMU's picture");
    }

    // The PROM is still running its power-on diagnostics, which ignore the
    // keyboard; it moves on by itself to a screen that does not.
    let t = Instant::now();
    let seq = rig.seq();
    while rig.seq() == seq {
        if rig.wait(100) {
            rig.present();
        }
        assert!(
            t.elapsed() < Duration::from_secs(300),
            "the PROM never moved on"
        );
    }
    eprintln!("the PROM moved on after {:?}", t.elapsed());
    rig.settle(Duration::from_secs(5), Duration::from_secs(300));

    // A hidden view asks for nothing: a key repaints the PROM, and no wake
    // comes until the view is shown again.
    let before = rig.stats();
    // SAFETY: a valid view.
    unsafe { ok(origami_view_set_visible(rig.view, 0), "hide") };
    let hidden = WAKES.load(Ordering::Relaxed);
    let seq0 = rig.seq();
    rig.send(&[key(keys_space(), true), key(keys_space(), false)]);
    std::thread::sleep(Duration::from_secs(6));
    let woken_hidden = WAKES.load(Ordering::Relaxed) - hidden;
    assert!(rig.seq() > seq0, "the key changed nothing");
    eprintln!("MEASURE hidden for 6 s while the guest repainted: {woken_hidden} wakes");
    // SAFETY: a valid view.
    unsafe { ok(origami_view_set_visible(rig.view, 1), "show") };
    let t = Instant::now();
    assert!(
        rig.wait(5000),
        "showing the view did not wake it for the change it missed"
    );
    eprintln!("MEASURE wake after showing: {:?}", t.elapsed());
    rig.settle(Duration::from_secs(4), Duration::from_secs(120));
    let cursor2 = rig.cursor();
    let (differ, _) = rig.compare(&|to| screendump(qmp_port, to), &work, "after-key");
    eprintln!(
        "MEASURE after a key: {differ} pixels differ (cursor on: {})",
        cursor2.on
    );
    if cursor2.on == 0 {
        assert_eq!(differ, 0, "the window is not QEMU's picture after a change");
    }
    assert_eq!(woken_hidden, 0, "a hidden view was woken");

    // The pointer: move it onto the picture and see whether the guest shows one.
    rig.send(&[origami_input_event {
        kind: ORIGAMI_INPUT_POINTER_AT,
        code: 0,
        down: 0,
        head: 0,
        x: (w / 2) as i32,
        y: (h / 2) as i32,
    }]);
    rig.settle(Duration::from_secs(3), Duration::from_secs(60));
    let c = rig.cursor();
    eprintln!(
        "cursor: present {} on {} at {},{} hot {},{} {}x{}",
        c.present, c.on, c.x, c.y, c.hot_x, c.hot_y, c.width, c.height
    );
    if c.on != 0 {
        let (differ, _) = rig.compare(&|to| screendump(qmp_port, to), &work, "cursor");
        eprintln!("MEASURE cursor: {differ} pixels differ from the screendump (the cursor's rectangle is {}x{})", c.width, c.height);
        assert!(
            differ > 0 && differ <= (c.width * c.height) as usize,
            "the cursor was not drawn where the guest has it"
        );
    }

    // Idle: the picture is still, so nothing may wake or present.
    rig.settle(Duration::from_secs(3), Duration::from_secs(60));
    let ticks0 = thread_ticks();
    let (wakes0, presents0, cpu0) = (WAKES.load(Ordering::Relaxed), rig.presents, process_cpu());
    let idle = Duration::from_secs(10);
    let t = Instant::now();
    while t.elapsed() < idle {
        if rig.wait(200) {
            rig.present();
        }
    }
    let ticks1 = thread_ticks();
    for (name, t) in &ticks1 {
        let d = t - ticks0.get(name).copied().unwrap_or(0);
        if d > 0 {
            eprintln!("MEASURE idle thread {name}: {d} ticks (10 ms each)");
        }
    }
    let wakes = WAKES.load(Ordering::Relaxed) - wakes0;
    let cpu = process_cpu() - cpu0;
    eprintln!(
        "MEASURE idle {:?}: {wakes} wakes, {} presents, {:.1} ms process CPU ({:.3}% of one core)",
        idle,
        rig.presents - presents0,
        cpu * 1e3,
        cpu / idle.as_secs_f64() * 100.0
    );

    let s = rig.stats();
    let vs = rig.view_stats();
    let per = |ns: u64| ns as f64 / vs.presents.max(1) as f64 / 1e3;
    eprintln!(
        "MEASURE per presented frame (mean of {}): take+upload {:.1} us, acquire+draw {:.1} us, ours {:.1} us; the window system's present {:.1} us",
        vs.presents, per(vs.take_ns), per(vs.draw_ns), per(vs.take_ns + vs.draw_ns), per(vs.present_ns)
    );
    let mut ours = rig.ours_us.clone();
    eprintln!(
        "MEASURE our CPU per presented frame (take, upload, draw; not the window system's present): p50 {:.0} us p95 {:.0} us max {:.0} us over {} presents",
        percentile(&mut ours, 0.5), percentile(&mut ours, 0.95), percentile(&mut ours, 1.0), ours.len()
    );
    let mut wall = rig.wall_ms.clone();
    let mut cpu_ms = rig.cpu_ms.clone();
    eprintln!(
        "MEASURE present: {} presents; wall ms p50 {:.3} p95 {:.3} max {:.3}; thread CPU ms p50 {:.3} p95 {:.3} max {:.3}",
        rig.presents,
        percentile(&mut wall, 0.5),
        percentile(&mut wall, 0.95),
        percentile(&mut wall, 1.0),
        percentile(&mut cpu_ms, 0.5),
        percentile(&mut cpu_ms, 0.95),
        percentile(&mut cpu_ms, 1.0)
    );
    eprintln!(
        "MEASURE frames {} lines {} (imported {}) CPU-copied picture bytes {} (before the key: {})",
        s.frames, s.lines, s.imported_lines, s.copied_bytes, before.copied_bytes
    );
    // SAFETY: valid handles, released once.
    unsafe {
        origami_view_detach(rig.view);
        origami_machine_release(machine);
        origami_core_release(core);
    }
    assert_eq!(s.counted, 1);
    if want_import {
        assert_eq!(s.copied_bytes, 0, "a picture byte was copied by the CPU");
        assert!(s.imported_lines > 0);
    } else {
        assert!(s.copied_bytes > 0);
    }
    assert_eq!(wakes, 0, "the still picture woke the viewer");
}
