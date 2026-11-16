//! A real QEMU Octane2 VPro boot through the C frame plane, drawn into an X11
//! window on a real GPU and compared with QEMU's own screendump.
//!
//! Ignored by default: it starts a machine. It needs a Vulkan device, an X
//! server (`DISPLAY`, Xvfb is enough; the window is made as large as the
//! picture so the draw is 1:1), ImageMagick's `import`, and:
//!
//! - ORIGAMI_RUNTIME_DIR: the QEMU runtime tree (a QEMU with display protocol 3)
//! - ORIGAMI_BIN: the `origami` executable, which creates and runs machines
//! - ORIGAMI_GUI_TEST_DIR: a directory for the machine
//!
//! `ORIGAMI_GUI_ZERO_COPY=0` runs the copying path. Run it in release mode
//! with `cargo test -p origami-ffi --release --test qemu_boot -- --ignored --nocapture`;
//! the numbers it prints start with `MEASURE`.
#![cfg(all(unix, feature = "qemu", feature = "render", not(feature = "fake")))]
#![allow(clippy::unnecessary_cast)]

use origami_ffi::*;
use std::ffi::{c_char, c_int, c_uint, c_ulong, c_void, CString};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::ptr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

#[link(name = "X11")]
extern "C" {
    fn XOpenDisplay(name: *const c_char) -> *mut c_void;
    fn XDefaultScreen(d: *mut c_void) -> c_int;
    fn XRootWindow(d: *mut c_void, screen: c_int) -> c_ulong;
    fn XCreateSimpleWindow(
        d: *mut c_void,
        parent: c_ulong,
        x: c_int,
        y: c_int,
        w: c_uint,
        h: c_uint,
        border_width: c_uint,
        border: c_ulong,
        background: c_ulong,
    ) -> c_ulong;
    fn XMapWindow(d: *mut c_void, w: c_ulong) -> c_int;
    fn XSync(d: *mut c_void, discard: c_int) -> c_int;
}

fn var(name: &str) -> PathBuf {
    std::env::var_os(name)
        .map(PathBuf::from)
        .unwrap_or_else(|| panic!("{name} is not set; see the header of qemu_boot.rs"))
}

fn ok(status: origami_status, what: &str) {
    if status != ORIGAMI_OK {
        let mut buf = [0 as c_char; 512];
        // SAFETY: a writable buffer of its stated length.
        unsafe { origami_last_error(buf.as_mut_ptr(), buf.len()) };
        let text = unsafe { std::ffi::CStr::from_ptr(buf.as_ptr()) };
        panic!("{what}: status {status}: {}", text.to_string_lossy());
    }
}

/// CPU seconds this thread has used.
fn thread_cpu() -> f64 {
    let mut t = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: a valid timespec.
    unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut t) };
    t.tv_sec as f64 + t.tv_nsec as f64 * 1e-9
}

/// CPU ticks each thread has used, by name.
fn thread_ticks() -> std::collections::BTreeMap<String, u64> {
    let mut out = std::collections::BTreeMap::new();
    for t in std::fs::read_dir("/proc/self/task").unwrap().flatten() {
        let Ok(stat) = std::fs::read_to_string(t.path().join("stat")) else {
            continue;
        };
        let (Some(open), Some(close)) = (stat.find('('), stat.rfind(')')) else {
            continue;
        };
        let f: Vec<&str> = stat[close + 2..].split(' ').collect();
        let ticks: u64 = f[11].parse::<u64>().unwrap_or(0) + f[12].parse::<u64>().unwrap_or(0);
        let name = format!(
            "{} {}",
            &stat[open + 1..close],
            t.file_name().to_string_lossy()
        );
        out.insert(name, ticks);
    }
    out
}

/// CPU seconds the whole process has used.
fn process_cpu() -> f64 {
    let mut u: libc::rusage = unsafe { std::mem::zeroed() };
    // SAFETY: a valid rusage.
    unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut u) };
    let tv = |t: libc::timeval| t.tv_sec as f64 + t.tv_usec as f64 * 1e-6;
    tv(u.ru_utime) + tv(u.ru_stime)
}

fn percentile(v: &mut [f64], p: f64) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v.get(((v.len() as f64 - 1.0) * p).round() as usize)
        .copied()
        .unwrap_or(0.0)
}

fn ppm(bytes: &[u8]) -> (u32, u32, Vec<u8>) {
    let mut at = 0;
    let mut field = || {
        while bytes[at].is_ascii_whitespace() {
            at += 1;
        }
        let start = at;
        while !bytes[at].is_ascii_whitespace() {
            at += 1;
        }
        String::from_utf8_lossy(&bytes[start..at]).into_owned()
    };
    assert_eq!(field(), "P6");
    let (w, h): (u32, u32) = (field().parse().unwrap(), field().parse().unwrap());
    assert_eq!(field(), "255");
    at += 1;
    (w, h, bytes[at..].to_vec())
}

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
fn screendump(qmp_port: u16, to: &Path) -> (u32, u32, Vec<u8>) {
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

/// What the X server shows in the window.
fn shot(window: c_ulong, to: &Path) -> (u32, u32, Vec<u8>) {
    let st = Command::new("import")
        .args(["-window", &window.to_string(), "-depth", "8"])
        .arg(format!("ppm:{}", to.display()))
        .status()
        .expect("ImageMagick import");
    assert!(st.success(), "import failed");
    let mut bytes = Vec::new();
    std::fs::File::open(to)
        .unwrap()
        .read_to_end(&mut bytes)
        .unwrap();
    ppm(&bytes)
}

fn differing(a: &[u8], b: &[u8]) -> Vec<usize> {
    a.chunks(3)
        .zip(b.chunks(3))
        .enumerate()
        .filter(|(_, (x, y))| x != y)
        .map(|(i, _)| i)
        .collect()
}

static WAKES: AtomicU64 = AtomicU64::new(0);

unsafe extern "C" fn wake(_: *mut c_void) {
    WAKES.fetch_add(1, Ordering::Relaxed);
}

struct Rig {
    machine: *const origami_machine,
    view: *mut origami_view,
    window: c_ulong,
    fd: i32,
    presents: u64,
    wall_ms: Vec<f64>,
    cpu_ms: Vec<f64>,
    /// Per present: our own part (take, upload, draw) in microseconds.
    ours_us: Vec<f64>,
    ours_last_ns: u64,
    last_present: Instant,
}

impl Rig {
    /// Wait up to `ms` for a wake; true when one came.
    fn wait(&self, ms: i32) -> bool {
        let mut p = libc::pollfd {
            fd: self.fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one valid pollfd.
        unsafe { libc::poll(&mut p, 1, ms) > 0 }
    }

    fn present(&mut self) -> u32 {
        // SAFETY: valid handles.
        unsafe { ok(origami_frame_wake_clear(self.machine), "wake_clear") };
        let (w0, c0) = (Instant::now(), thread_cpu());
        let mut drawn = 9;
        // SAFETY: valid view and out pointer.
        unsafe { ok(origami_view_present(self.view, &mut drawn), "present") };
        if drawn == ORIGAMI_DRAWN_PRESENTED {
            self.presents += 1;
            self.wall_ms.push(w0.elapsed().as_secs_f64() * 1e3);
            self.cpu_ms.push((thread_cpu() - c0) * 1e3);
            let vs = self.view_stats();
            let ours = vs.take_ns + vs.draw_ns;
            self.ours_us.push((ours - self.ours_last_ns) as f64 / 1e3);
            self.ours_last_ns = ours;
            self.last_present = Instant::now();
        }
        drawn
    }

    /// Present on every wake until nothing has been drawn for `quiet`.
    fn settle(&mut self, quiet: Duration, limit: Duration) {
        let start = Instant::now();
        self.last_present = Instant::now();
        loop {
            if self.wait(100) {
                self.present();
            }
            if self.presents > 0 && self.last_present.elapsed() > quiet {
                return;
            }
            assert!(start.elapsed() < limit, "the picture never settled");
        }
    }

    /// Draw twice more: NVIDIA's presentation to Xvfb shows the previous
    /// image until the next present.
    fn flush(&mut self) {
        self.present();
        self.present();
        std::thread::sleep(Duration::from_millis(300));
    }

    /// The window against QEMU's screendump, taken while the guest is quiet:
    /// the pixels that differ and the colours in the picture. A guest that
    /// moves between the two shots (a wake) is measured again.
    fn compare(&mut self, qmp_port: u16, work: &Path, name: &str) -> (usize, usize) {
        for _ in 0..6 {
            self.settle(Duration::from_secs(3), Duration::from_secs(120));
            self.flush();
            let wakes = WAKES.load(Ordering::Relaxed);
            let (_, _, got) = shot(self.window, &work.join(format!("{name}-window.ppm")));
            let (_, _, want) = screendump(qmp_port, &work.join(format!("{name}.ppm")));
            if WAKES.load(Ordering::Relaxed) == wakes {
                let colours: std::collections::HashSet<&[u8]> = want.chunks(3).collect();
                return (differing(&got, &want).len(), colours.len());
            }
        }
        panic!("the guest never stayed still for a comparison");
    }

    fn view_stats(&self) -> origami_view_counters {
        let mut vs = origami_view_counters {
            size: std::mem::size_of::<origami_view_counters>() as u32,
            reserved: 0,
            presents: 0,
            take_ns: 0,
            draw_ns: 0,
            present_ns: 0,
        };
        // SAFETY: a valid view and out pointer.
        unsafe { ok(origami_view_stats(self.view, &mut vs), "view_stats") };
        vs
    }

    fn seq(&self) -> u64 {
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
                origami_frame_heads(self.machine, heads.as_mut_ptr(), 1, &mut n),
                "heads",
            )
        };
        heads[0].seq
    }

    fn stats(&self) -> origami_frame_counters {
        let mut s = origami_frame_counters {
            size: std::mem::size_of::<origami_frame_counters>() as u32,
            counted: 0,
            frames: 0,
            lines: 0,
            imported_lines: 0,
            copied_bytes: 0,
        };
        // SAFETY: valid machine and out pointer.
        unsafe { ok(origami_frame_stats(self.machine, &mut s), "stats") };
        s
    }

    fn cursor(&self) -> origami_cursor {
        let mut c = origami_cursor {
            size: std::mem::size_of::<origami_cursor>() as u32,
            present: 0,
            on: 0,
            x: 0,
            y: 0,
            hot_x: 0,
            hot_y: 0,
            width: 0,
            height: 0,
            gen: 0,
            image_bytes: 0,
        };
        let mut image = vec![0u8; 64 * 64 * 4];
        // SAFETY: valid machine, out pointer and buffer.
        unsafe {
            ok(
                origami_frame_cursor(
                    self.machine,
                    0,
                    u32::MAX,
                    &mut c,
                    image.as_mut_ptr(),
                    image.len(),
                ),
                "cursor",
            )
        };
        c
    }

    fn send(&self, events: &[origami_input_event]) {
        // SAFETY: valid events for their count.
        unsafe {
            ok(
                origami_input_send(self.machine, events.as_ptr(), events.len()),
                "input",
            )
        };
    }
}

fn key(code: u32, down: bool) -> origami_input_event {
    origami_input_event {
        kind: ORIGAMI_INPUT_KEY,
        code,
        down: u32::from(down),
        head: 0,
        x: 0,
        y: 0,
    }
}

#[test]
#[ignore = "starts a machine; see the header"]
fn an_octane2_boot_reaches_the_window_without_the_cpu_touching_a_pixel() {
    let want_import = std::env::var("ORIGAMI_GUI_ZERO_COPY").map_or(true, |v| v != "0");
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
    let (differ, _) = rig.compare(qmp_port, &work, "prom");
    eprintln!(
        "MEASURE window vs screendump: {differ} of {} pixels differ (cursor on: {})",
        w * h,
        cursor.on
    );
    if cursor.on == 0 {
        assert_eq!(differ, 0, "the window is not QEMU's picture");
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
    let (differ, _) = rig.compare(qmp_port, &work, "after-key");
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
        let (differ, _) = rig.compare(qmp_port, &work, "cursor");
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

fn keys_space() -> u32 {
    origami_key::Space as u32
}
