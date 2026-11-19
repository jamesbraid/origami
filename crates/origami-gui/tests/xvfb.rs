//! A real Octane2 in a real window under a virtual X server with a window
//! manager (a bare X server delivers no key events to a window). One test
//! shows the PROM, sends a key and pops the view out a hundred times; another
//! boots IRIX and watches the guest's cursor follow the host pointer; one
//! boots IRIX with its console on serial and uses the serial view to log in,
//! run commands, scroll back, copy and paste, then reads the logs view and
//! stops and starts the machine from the toolbar; the last starts a machine
//! with no display socket and checks the status says why, then recovers it
//! with force stop, the PROM console buttons, start and reset.
//!
//! Ignored by default: it boots QEMU and needs Xvfb, openbox, xdotool and
//! ImageMagick on PATH, a Vulkan driver (software is fine), and these:
//!
//! - ORIGAMI_RUNTIME_DIR: the directory holding the QEMU the product runs,
//!   built from a QEMU with the shared-memory display (`display-shm`)
//! - ORIGAMI_BIN: the `origami` executable
//! - ORIGAMI_GUI_TEST_DIR: an empty or new directory for machines and
//!   evidence (screenshots are left in each test's `out` directory)
//! - ORIGAMI_GUI_TEST_GUI (optional): another `origami-gui` to run in place of
//!   the one this build made, to compare a change against the one before it
//! - ORIGAMI_GUI_TEST_IRIX_DISK (the IRIX tests only): the published
//!   `octane-games-be780408.chd` (SHA-256 below), which the test boots with an
//!   overlay and so leaves a `.diff.chd` beside, removed at the start and end
//!
//! The machines are started to export their scanout memory on the GUI's own
//! adapter (`origami-gui --gpu-info` names it), so the GUI imports it and draws
//! it without copying: the tests check the counters for that. With
//! `ORIGAMI_GUI_ZERO_COPY=0` they run the copying path instead, and a device
//! that cannot import (no external-memory extension) falls back to it by
//! itself.
//!
//! Run it with `cargo test -p origami-gui --test xvfb -- --ignored --nocapture`.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn var(name: &str) -> PathBuf {
    std::env::var_os(name)
        .map(PathBuf::from)
        .unwrap_or_else(|| panic!("{name} is not set; see the header of tests/xvfb.rs"))
}

struct Proc(Child);

impl Drop for Proc {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn run_ok(cmd: &mut Command) -> String {
    let out = cmd.output().unwrap_or_else(|e| panic!("{cmd:?}: {e}"));
    assert!(
        out.status.success(),
        "{cmd:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn wait_for(what: &str, timeout: Duration, mut f: impl FnMut() -> bool) {
    let start = Instant::now();
    while !f() {
        assert!(
            start.elapsed() < timeout,
            "timed out after {timeout:?} waiting for {what}"
        );
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn stats(path: &Path) -> Option<serde_json::Value> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

fn engine_running(p: &Path) -> Option<bool> {
    stats(p).map(|s| s["engine"][0]["running"] == true)
}

fn counter(path: &Path, name: &str) -> u64 {
    stats(path).and_then(|s| s[name].as_u64()).unwrap_or(0)
}

fn screenshot(display: &str, to: &Path) {
    run_ok(
        Command::new("import")
            .env("DISPLAY", display)
            .args(["-window", "root"])
            .arg(to),
    );
}

/// Number of distinct colours in a screenshot.
fn colours(png: &Path) -> u64 {
    run_ok(Command::new("identify").args(["-format", "%k"]).arg(png))
        .trim()
        .parse()
        .unwrap()
}

/// How many pixels differ between two screenshots, within `region` (WxH+X+Y).
fn differing(a: &Path, b: &Path, region: &str) -> u64 {
    let crop = |p: &Path| {
        let out = a.with_file_name(format!(
            "crop-{}-{}.png",
            p.file_stem().unwrap().to_string_lossy(),
            region.replace(['+', 'x'], "_")
        ));
        run_ok(
            Command::new("convert")
                .arg(p)
                .args(["-crop", region, "+repage"])
                .arg(&out),
        );
        out
    };
    let (ca, cb) = (crop(a), crop(b));
    let out = Command::new("compare")
        .args(["-metric", "AE"])
        .arg(&ca)
        .arg(&cb)
        .arg("null:")
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stderr)
        .split_whitespace()
        .next()
        .unwrap()
        .parse::<f64>()
        .unwrap() as u64
}

/// The Continue button of the PROM's diagnostics dialog, in screen pixels,
/// with the main window where a window manager puts it on a 1920x1200 screen
/// (the picture shown at 1:1).
const CONTINUE_BUTTON: (u32, u32) = (1262, 895);

fn pixel(png: &Path, (x, y): (u32, u32)) -> (u8, u8, u8) {
    let text = run_ok(Command::new("convert").arg(png).args([
        "-format",
        &format!("%[pixel:p{{{x},{y}}}]"),
        "info:",
    ]));
    // "srgb(r,g,b)", or "gray(v)" for a grey one.
    let (grey, inner) = match text.trim().split_once('(') {
        Some(("srgb", rest)) => (false, rest),
        Some(("gray", rest)) => (true, rest),
        _ => panic!("pixel {text:?}"),
    };
    let nums: Vec<u8> = inner
        .trim_end_matches(')')
        .split(',')
        .map(|n| {
            n.trim()
                .parse()
                .unwrap_or_else(|_| panic!("pixel {text:?}"))
        })
        .collect();
    if grey {
        (nums[0], nums[0], nums[0])
    } else {
        (nums[0], nums[1], nums[2])
    }
}

fn is_grey((r, g, b): (u8, u8, u8)) -> bool {
    r == g && g == b && r > 100
}

fn key(display: &str, args: &[&str]) {
    run_ok(Command::new("xdotool").env("DISPLAY", display).args(args));
}

/// A virtual X server with a window manager, and the paths one test uses.
struct Rig {
    origami: PathBuf,
    gui: PathBuf,
    out: PathBuf,
    work: PathBuf,
    display: String,
    /// The adapter's `--display-gpu` value when the GUI can import QEMU's
    /// memory, so every machine here is started to export on it.
    gpu: Option<String>,
    _xvfb: Proc,
    _wm: Proc,
}

/// The machine is stopped when the guard goes, whatever the test did.
struct StopMachine(PathBuf, PathBuf);

impl Drop for StopMachine {
    fn drop(&mut self) {
        let _ = Command::new(&self.0).arg("stop").arg(&self.1).status();
    }
}

impl Rig {
    fn start(name: &str) -> Rig {
        let work = var("ORIGAMI_GUI_TEST_DIR").join(name);
        let out = work.join("out");
        let _ = std::fs::remove_dir_all(&work);
        std::fs::create_dir_all(&out).unwrap();
        // A virtual X server, with a window manager.
        let xvfb = Command::new("Xvfb")
            .args([
                "-displayfd",
                "1",
                "-screen",
                "0",
                "1920x1200x24",
                "-nolisten",
                "tcp",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("Xvfb");
        let mut xvfb = Proc(xvfb);
        let display = {
            use std::io::BufRead;
            let mut line = String::new();
            std::io::BufReader::new(xvfb.0.stdout.take().unwrap())
                .read_line(&mut line)
                .unwrap();
            format!(":{}", line.trim())
        };
        let wm = Proc(
            Command::new("openbox")
                .env("DISPLAY", &display)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("openbox"),
        );
        std::thread::sleep(Duration::from_secs(1));
        let gui = std::env::var_os("ORIGAMI_GUI_TEST_GUI").map_or_else(
            || PathBuf::from(env!("CARGO_BIN_EXE_origami-gui")),
            PathBuf::from,
        );
        let info = run_ok(Command::new(&gui).arg("--gpu-info"));
        eprintln!("{info}");
        let gpu = info
            .lines()
            .find_map(|l| l.strip_prefix("display-gpu: "))
            .map(str::to_string);
        Rig {
            origami: var("ORIGAMI_BIN"),
            gui,
            gpu,
            out,
            work,
            display,
            _xvfb: xvfb,
            _wm: wm,
        }
    }

    fn create(&self, name: &str, preset: &str) -> PathBuf {
        let machine = self.work.join(name);
        run_ok(
            Command::new(&self.origami)
                .arg("create")
                .arg(&machine)
                .args(["--preset", preset]),
        );
        machine
    }

    /// `origami run` options that publish the display and, where the GUI can
    /// import QEMU's memory, export it on the GUI's adapter.
    fn display_args(&self) -> Vec<String> {
        let mut args: Vec<String> = ["--display", "shm", "--background"]
            .map(String::from)
            .into();
        if let Some(gpu) = &self.gpu {
            args.extend(["--display-gpu".into(), gpu.clone()]);
        }
        args
    }

    /// The machine runs apart from every view: start it here, attach below.
    fn run(&self, machine: &Path) -> StopMachine {
        let mut cmd = Command::new(&self.origami);
        cmd.arg("run").arg(machine).args(self.display_args());
        // Extra QEMU arguments, for tracing, as in
        // ORIGAMI_GUI_TEST_QEMU_ARGS="-d trace:sgi_scanout_frame".
        for a in std::env::var("ORIGAMI_GUI_TEST_QEMU_ARGS")
            .unwrap_or_default()
            .split_whitespace()
        {
            cmd.arg(format!("--qemu-arg={a}"));
        }
        run_ok(&mut cmd);
        StopMachine(self.origami.clone(), machine.to_path_buf())
    }

    fn spawn_gui(&self, machine: &Path, tag: &str, extra: &[&str]) -> (Proc, PathBuf, PathBuf) {
        let stats = self.out.join(format!("stats-{tag}.json"));
        let log = self.out.join(format!("gui-{tag}.log"));
        let child = Command::new(&self.gui)
            .env("DISPLAY", &self.display)
            .env("ORIGAMI_BIN", &self.origami)
            .env("RUST_BACKTRACE", "1")
            .env(
                "ORIGAMI_GUI_TRACE_CURSOR",
                self.out.join(format!("cursor-{tag}.txt")),
            )
            .arg("--stats-file")
            .arg(&stats)
            .args(extra)
            .arg(machine)
            .stderr(std::fs::File::create(&log).unwrap())
            .stdout(Stdio::null())
            .spawn()
            .expect("origami-gui");
        (Proc(child), stats, log)
    }
}

/// What the counters say about how the picture reached the GPU: with an
/// import, every line was drawn from QEMU's memory and no pixel was copied or
/// uploaded by the CPU; without, every line was.
fn check_copies(stats_file: &Path, imported: bool) {
    let s = stats(stats_file).expect("stats");
    let e = &s["engine"][0];
    eprintln!(
        "backend {:?}: frames {} imported, rows {} imported / {} uploaded, \
         {} bytes uploaded, engine copied {} bytes",
        e["backend"],
        s["frames_imported"],
        s["rows_imported"],
        s["rows_uploaded"],
        s["bytes_uploaded"],
        e["copied_bytes"]
    );
    if imported {
        assert!(
            s["frames_imported"].as_u64() > Some(0),
            "nothing was imported: {s}"
        );
        assert_eq!(s["rows_uploaded"], 0, "rows were uploaded by the CPU: {s}");
        assert_eq!(
            s["bytes_uploaded"], 0,
            "pixels were uploaded by the CPU: {s}"
        );
        assert_eq!(e["copied_bytes"], 0, "the engine copied pixels: {s}");
        assert!(
            e["backend"].as_str().is_some_and(|b| b.contains("import")),
            "{s}"
        );
    } else {
        assert_eq!(s["frames_imported"], 0, "{s}");
        assert!(s["bytes_uploaded"].as_u64() > Some(0), "{s}");
    }
}

fn alive(p: &mut Proc, log: &Path) {
    if let Some(status) = p.0.try_wait().unwrap() {
        panic!(
            "origami-gui exited early ({status}): {}",
            std::fs::read_to_string(log).unwrap_or_default()
        );
    }
}

#[test]
#[ignore = "boots QEMU under Xvfb; see the header"]
fn an_octane2_prom_in_a_window_a_key_and_a_hundred_pop_outs() {
    let rig = Rig::start("prom");
    let (origami, out, display) = (rig.origami.clone(), rig.out.clone(), rig.display.clone());
    let machine = rig.create("octane2", "octane2-vpro");
    let _stop = rig.run(&machine);
    let spawn_gui = |tag: &str, extra: &[&str]| rig.spawn_gui(&machine, tag, extra);

    // 1. The PROM on screen. With no disk it runs its power-on diagnostics,
    // which fail, and stops on a dialog that says "Press any key to
    // continue". That dialog has a grey Continue button where the menu behind
    // it has a blue gradient, which is what the wait looks for.
    let (mut gui1, s1, log1) = spawn_gui("view", &[]);
    wait_for("the first frame upload", Duration::from_secs(300), || {
        alive(&mut gui1, &log1);
        counter(&s1, "frames_taken") > 0
    });
    let prom = out.join("prom.png");
    wait_for(
        "the PROM's diagnostics dialog",
        Duration::from_secs(600),
        || {
            alive(&mut gui1, &log1);
            screenshot(&display, &prom);
            is_grey(pixel(&prom, CONTINUE_BUTTON))
        },
    );
    // Let the picture finish drawing.
    std::thread::sleep(Duration::from_secs(3));
    screenshot(&display, &prom);
    let colours_seen = colours(&prom);
    eprintln!(
        "PROM screenshot: {colours_seen} colours, {} frames taken",
        counter(&s1, "frames_taken")
    );
    assert!(
        colours_seen > 300,
        "the screen looks blank: {colours_seen} colours (see {})",
        prom.display()
    );
    assert!(is_grey(pixel(&prom, CONTINUE_BUTTON)));
    assert_eq!(counter(&s1, "gpu_errors"), 0);
    check_copies(&s1, rig.gpu.is_some());

    // 2. Nothing is redrawn while nothing changes.
    let sample = || {
        (
            counter(&s1, "before_renders"),
            counter(&s1, "passes"),
            counter(&s1, "rows_uploaded") + counter(&s1, "rows_imported"),
        )
    };
    let idle0 = sample();
    std::thread::sleep(Duration::from_secs(10));
    let idle1 = sample();
    eprintln!(
        "idle for 10 s: renders {} -> {}, passes {} -> {}, rows {} -> {}",
        idle0.0, idle1.0, idle0.1, idle1.1, idle0.2, idle1.2
    );
    assert_eq!(
        idle1, idle0,
        "the GUI drew or uploaded while the guest was idle"
    );

    // 3. One key reaches the guest: focus the view and press space. The
    // dialog goes away and the PROM's menu appears, which only the guest can
    // do.
    key(&display, &["mousemove", "900", "700", "click", "1"]);
    std::thread::sleep(Duration::from_secs(1));
    let keys0 = counter(&s1, "keys_sent");
    let frames0 = counter(&s1, "frames_taken");
    key(&display, &["key", "space"]);
    wait_for(
        "the guest to repaint after the key",
        Duration::from_secs(60),
        || counter(&s1, "frames_taken") > frames0,
    );
    std::thread::sleep(Duration::from_secs(3));
    let after = out.join("after-key.png");
    screenshot(&display, &after);
    let keys1 = counter(&s1, "keys_sent");
    let changed = differing(&prom, &after, "1280x1024+320+153");
    eprintln!(
        "keys sent {keys0} -> {keys1}; frames {frames0} -> {}; {changed} pixels changed",
        counter(&s1, "frames_taken")
    );
    assert!(
        keys1 >= keys0 + 2,
        "key events did not reach the GUI (is a window manager running?)"
    );
    assert!(
        !is_grey(pixel(&after, CONTINUE_BUTTON)),
        "the dialog is still up"
    );
    assert!(
        changed > 10_000,
        "the picture hardly changed after the key ({changed} pixels)"
    );
    // The pointer: moving the host pointer over the view sends absolute
    // positions in the head's pixels, which QEMU accepts and turns into guest
    // mouse motion. The PROM shows no cursor and publishes none (the control
    // block says so), so there is no guest cursor to see move; this checks
    // that the positions go out and the connection stays healthy. The closed
    // loop on a guest that does publish its cursor is covered by QEMU's qtest.
    let moves0 = counter(&s1, "moves_sent");
    for (x, y) in [("700", "500"), ("900", "640"), ("1000", "300")] {
        key(&display, &["mousemove", x, y]);
        std::thread::sleep(Duration::from_millis(300));
    }
    std::thread::sleep(Duration::from_secs(2));
    let moves1 = counter(&s1, "moves_sent");
    eprintln!(
        "pointer: {} absolute positions sent, {} input errors",
        moves1 - moves0,
        counter(&s1, "input_errors")
    );
    assert!(
        moves1 >= moves0 + 3,
        "pointer moves were not sent ({moves0} -> {moves1})"
    );
    assert_eq!(counter(&s1, "input_errors"), 0);
    check_copies(&s1, rig.gpu.is_some());
    assert_eq!(
        stats(&s1).map(|s| s["engine"][0]["cursor"]["on"] == false),
        Some(true),
        "the PROM publishes no cursor"
    );
    assert_eq!(
        engine_running(&s1),
        Some(true),
        "the machine and its display survived the pointer"
    );

    // The About button opens the window with Slint's attribution widget.
    key(&display, &["mousemove", "404", "72", "click", "1"]);
    wait_for("the About window", Duration::from_secs(30), || {
        Command::new("xdotool")
            .env("DISPLAY", &display)
            .args(["search", "--name", "About origami"])
            .output()
            .map(|o| o.status.success() && !o.stdout.is_empty())
            .unwrap_or(false)
    });
    std::thread::sleep(Duration::from_secs(1));
    screenshot(&display, &out.join("about.png"));
    drop(gui1);
    let log = std::fs::read_to_string(&log1).unwrap_or_default();
    assert!(
        !log.contains("GPU:"),
        "GPU errors in {}: {log}",
        log1.display()
    );

    // 4. Pop out and dock a hundred times in a new view of the same machine.
    // The first view going away did not stop the machine.
    assert!(run_ok(Command::new(&origami).arg("status").arg(&machine)).contains("running"));
    let (mut gui2, s2, log2) = spawn_gui("popout", &["--popout-cycles", "100"]);
    let started = Instant::now();
    wait_for("the hundred cycles", Duration::from_secs(900), || {
        alive(&mut gui2, &log2);
        counter(&s2, "docks") >= 100
    });
    eprintln!(
        "pop-out loop: {} pop-outs, {} docks in {:?}",
        counter(&s2, "popouts"),
        counter(&s2, "docks"),
        started.elapsed()
    );
    // The window is left up for a moment after the last cycle.
    screenshot(&display, &out.join("after-popouts.png"));
    let status = loop {
        if let Some(st) = gui2.0.try_wait().unwrap() {
            break st;
        }
        assert!(
            started.elapsed() < Duration::from_secs(1000),
            "origami-gui did not exit after the cycles"
        );
        std::thread::sleep(Duration::from_millis(200));
    };
    let log = std::fs::read_to_string(&log2).unwrap_or_default();
    let (popouts, docks) = (counter(&s2, "popouts"), counter(&s2, "docks"));
    assert!(status.success(), "origami-gui failed: {log}");
    assert_eq!((popouts, docks), (100, 100));
    assert_eq!(counter(&s2, "gpu_errors"), 0, "GPU errors: {log}");
    assert!(
        !log.contains("GPU:") && !log.contains("panicked"),
        "errors in {}: {log}",
        log2.display()
    );
    assert!(run_ok(Command::new(&origami).arg("status").arg(&machine)).contains("running"));

    // 5. The machine ends and is started again under a view that stays open:
    // the view notices, then finds the new display and shows its PROM.
    let (mut gui3, s3, log3) = spawn_gui("restart", &[]);
    wait_for("the view to attach", Duration::from_secs(120), || {
        alive(&mut gui3, &log3);
        counter(&s3, "frames_taken") > 0
    });
    assert_eq!(engine_running(&s3), Some(true));
    run_ok(Command::new(&origami).arg("stop").arg(&machine));
    wait_for(
        "the view to notice the machine ended",
        Duration::from_secs(60),
        || {
            alive(&mut gui3, &log3);
            engine_running(&s3) == Some(false)
        },
    );
    let frames_before = counter(&s3, "frames_taken");
    run_ok(
        Command::new(&origami)
            .arg("run")
            .arg(&machine)
            .args(rig.display_args()),
    );
    wait_for(
        "the view to find the restarted machine",
        Duration::from_secs(120),
        || {
            alive(&mut gui3, &log3);
            engine_running(&s3) == Some(true) && counter(&s3, "frames_taken") > frames_before
        },
    );
    let again = out.join("after-restart.png");
    wait_for(
        "the restarted PROM's diagnostics dialog",
        Duration::from_secs(600),
        || {
            alive(&mut gui3, &log3);
            screenshot(&display, &again);
            is_grey(pixel(&again, CONTINUE_BUTTON))
        },
    );
    eprintln!(
        "restart: {frames_before} frames before, {} after",
        counter(&s3, "frames_taken")
    );
    assert_eq!(counter(&s3, "gpu_errors"), 0);
    check_copies(&s3, rig.gpu.is_some());
    drop(gui3);
    assert!(!std::fs::read_to_string(&log3)
        .unwrap_or_default()
        .contains("GPU:"));
}

#[test]
#[ignore = "boots QEMU under Xvfb; see the header"]
fn closing_the_window_exits_cleanly_with_the_machine_still_running() {
    // Every GPU resource must be gone before the event loop returns: left to
    // thread-local destructors, wgpu's own thread-locals can already be
    // destroyed and dropping a buffer aborts the process.
    let rig = Rig::start("close");
    let machine = rig.create("octane2", "octane2-vpro");
    let _stop = rig.run(&machine);
    let (mut gui, s, log) = rig.spawn_gui(&machine, "close", &[]);
    wait_for("the first frame", Duration::from_secs(300), || {
        alive(&mut gui, &log);
        counter(&s, "frames_taken") > 0
    });
    // Close the window the way a user does, through the window manager.
    run_ok(
        Command::new("wmctrl")
            .env("DISPLAY", &rig.display)
            .args(["-c", "origami"]),
    );
    let started = Instant::now();
    let status = loop {
        if let Some(st) = gui.0.try_wait().unwrap() {
            break st;
        }
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "origami-gui did not exit when its window was closed"
        );
        std::thread::sleep(Duration::from_millis(200));
    };
    let text = std::fs::read_to_string(&log).unwrap_or_default();
    assert!(status.success(), "exit {status}: {text}");
    assert!(
        !text.contains("panicked") && !text.contains("GPU:"),
        "errors on exit: {text}"
    );
    // The machine outlives the view.
    assert!(run_ok(Command::new(&rig.origami).arg("status").arg(&machine)).contains("running"));
}

/// The published IRIX 6.5.30 Octane disk with a graphics login, checked
/// against this before it is booted.
const IRIX_DISK_SHA256: &str = "be7804087a46faab1f7f4d9839e10c1862eaffa910361bdf4a4512868fa8606d";

/// Removes the overlay diff a boot leaves beside the shared disk.
struct DiffGuard(PathBuf);

impl DiffGuard {
    fn clear(&self) {
        let mut diff = self.0.as_os_str().to_owned();
        diff.push(".diff.chd");
        let _ = std::fs::remove_file(PathBuf::from(diff));
    }
}

impl Drop for DiffGuard {
    fn drop(&mut self) {
        self.clear();
    }
}

/// How far the arrow's red pixels may sit from the pointer, in head pixels.
/// The VPro has no hot-spot register, so QEMU reports the glyph's top left and
/// closes its loop on that; the red fill starts a pixel or two inside the
/// glyph's outline, so a few pixels either way is a hit.
const ARROW_TOLERANCE: i64 = 6;

/// The top left of the red pixels in the picture area of a screenshot, which
/// is the tip of IRIX's red arrow cursor (drawn by the guest into its pixels),
/// as a head pixel; None when there is none.
fn red_arrow(png: &Path) -> Option<(i64, i64)> {
    let text = run_ok(
        Command::new("convert")
            .arg(png)
            .args(["-crop", "1280x1024+320+153", "+repage", "-fuzz", "12%"])
            // Near red becomes pure green, then everything not exactly that is black.
            .args(["-fill", "#00ff00", "-opaque", "#ff0000", "-fuzz", "0"])
            .args(["-fill", "black", "+opaque", "#00ff00"])
            .args(["-trim", "-format", "%wx%h%X%Y", "info:"]),
    );
    // "WxH+X+Y" of the white pixels; a picture with none trims to nothing.
    let (size, rest) = text.trim().split_once('+')?;
    let (x, y) = rest.split_once('+')?;
    let (w, h) = size.split_once('x')?;
    let (w, h): (u32, u32) = (w.parse().ok()?, h.parse().ok()?);
    // A cursor is small; a bigger patch of red is something else.
    // An image with no red trims to a speck.
    if w < 4 || h < 4 || w > 200 || h > 200 {
        return None;
    }
    Some((x.parse().ok()?, y.parse().ok()?))
}

#[test]
#[ignore = "boots IRIX under Xvfb and needs the published disk; see the header"]
fn irix_cursor_follows_the_host_pointer() {
    let disk = var("ORIGAMI_GUI_TEST_IRIX_DISK");
    let sum = run_ok(Command::new("sha256sum").arg(&disk));
    assert!(
        sum.starts_with(IRIX_DISK_SHA256),
        "{} is not the published disk: {sum}",
        disk.display()
    );
    let guard = DiffGuard(disk.clone());
    guard.clear();

    let rig = Rig::start("irix");
    let display = rig.display.clone();
    let machine = rig.create("octane2", "octane2-vpro");
    run_ok(
        Command::new(&rig.origami)
            .arg("drive-attach")
            .arg(&machine)
            .arg(&disk)
            .args(["--type", "disk", "--target", "1", "--overlay"]),
    );
    let _stop = rig.run(&machine);
    let (mut gui, s, log) = rig.spawn_gui(&machine, "irix", &[]);

    // IRIX's login screen has the guest's red arrow cursor on it. The host
    // pointer is a red arrow too, so it is parked outside the window whenever
    // a screenshot is taken.
    let started = Instant::now();
    let shot = rig.out.join("irix-poll.png");
    let park = || key(&display, &["mousemove", "100", "100"]);
    park();
    wait_for(
        "the IRIX login screen with its cursor",
        Duration::from_secs(1200),
        || {
            alive(&mut gui, &log);
            screenshot(&display, &shot);
            // The login dialog's light grey (the PROM's dialog is darker),
            // and the guest's arrow somewhere on it.
            let (r, g, b) = pixel(&shot, (900, 300));
            r == g && g == b && r > 215 && red_arrow(&shot).is_some()
        },
    );
    eprintln!("IRIX login after {:?}", started.elapsed());
    std::thread::sleep(Duration::from_secs(5));
    let head = stats(&s).map(|v| {
        (
            v["engine"][0]["head"]["width"].as_i64(),
            v["engine"][0]["head"]["height"].as_i64(),
        )
    });
    assert_eq!(
        head,
        Some((Some(1280), Some(1024))),
        "the test assumes a 1280x1024 head shown at 1:1"
    );
    let before = rig.out.join("irix-before.png");
    screenshot(&display, &before);

    // Captured, which is how the GUI starts: the status offers the capture, a
    // click takes the pointer, and raw motion at an ordinary speed moves the
    // guest's arrow smoothly the way it was moved, without the guest's cursor
    // ever turning back.
    let m = machine_state(&s).expect("the viewer's state");
    assert_eq!(m["pointer_mode"], "capture");
    assert_eq!(m["captured"], false);
    assert!(
        text_of(&m, "status").contains("click the picture to capture the pointer"),
        "{}",
        text_of(&m, "status")
    );
    key(&display, &["mousemove", "900", "640", "click", "1"]);
    let m = wait_state(
        "the pointer to be captured",
        Duration::from_secs(20),
        &mut gui,
        &log,
        &s,
        |m| m["captured"] == true,
    );
    assert!(
        text_of(&m, "status").contains("press Ctrl+Alt to release"),
        "{}",
        text_of(&m, "status")
    );
    // The click that captured went no further than the viewer.
    assert_eq!(counter(&s, "input_errors"), 0);
    let trace = rig.out.join("cursor-irix.txt");
    // To the left edge first, so there is room to go right.
    let mut chain: Vec<String> = Vec::new();
    for _ in 0..100 {
        chain.extend(["mousemove_relative", "--", "-8", "0", "sleep", "0.008"].map(String::from));
    }
    key(
        &display,
        &chain.iter().map(String::as_str).collect::<Vec<_>>(),
    );
    std::thread::sleep(Duration::from_secs(2));
    let start_at = cursor_trace(&trace).len();
    let start = cursor_trace(&trace)
        .last()
        .copied()
        .expect("the guest's cursor");
    // 100 steps of 6 counts every 8 ms is 750 counts a second, to the right.
    let mut chain: Vec<String> = Vec::new();
    for _ in 0..100 {
        chain.extend(["mousemove_relative", "--", "6", "0", "sleep", "0.008"].map(String::from));
    }
    let args: Vec<&str> = chain.iter().map(String::as_str).collect();
    key(&display, &args);
    std::thread::sleep(Duration::from_secs(2));
    let moves = cursor_trace(&trace);
    let path: Vec<_> = moves[start_at..].to_vec();
    let end = *path.last().expect("the guest's cursor moved");
    let (reversals, wander) = (turned_back(&path, true), vertical_range(&path));
    eprintln!(
        "captured: {} counts right moved the guest's cursor from {:?} to {:?}; {} cursor changes, {} turn-backs, {} px of vertical wander",
        600, (start.1, start.2), (end.1, end.2), path.len(), reversals, wander
    );
    // IRIX moves a packet of up to 4 counts that many pixels and a larger one
    // further, so 600 counts are at least 600 pixels, less any that fell off the edge.
    let want = (1280 - 1 - i64::from(start.1)).min(600);
    assert!(
        i64::from(end.1) - i64::from(start.1) >= want * 9 / 10,
        "the guest's cursor went {} px for {want} counts",
        i64::from(end.1) - i64::from(start.1)
    );
    assert_eq!(reversals, 0, "the cursor turned back: {path:?}");
    // The reported hot spot steps a few pixels when the arrow's shape changes.
    assert!(wander <= 8, "the cursor wandered {wander} px: {path:?}");
    // Down is positive y, as the guest counts it: 60 steps of 6 counts.
    let start_at = cursor_trace(&trace).len();
    let top = cursor_trace(&trace)
        .last()
        .copied()
        .expect("the guest's cursor");
    let mut chain: Vec<String> = Vec::new();
    for _ in 0..60 {
        chain.extend(["mousemove_relative", "--", "0", "6", "sleep", "0.008"].map(String::from));
    }
    key(
        &display,
        &chain.iter().map(String::as_str).collect::<Vec<_>>(),
    );
    std::thread::sleep(Duration::from_secs(2));
    let path: Vec<_> = cursor_trace(&trace)[start_at..].to_vec();
    let bottom = *path.last().expect("the guest's cursor moved down");
    assert!(
        bottom.2 - top.2 >= 360 * 9 / 10,
        "360 counts down moved the guest's cursor from y = {} to {}: {path:?}",
        top.2,
        bottom.2
    );
    assert!(
        path.windows(2).all(|w| w[1].2 >= w[0].2 - 2),
        "the cursor turned back going down: {path:?}"
    );
    // Ctrl+Alt frees it.
    key(
        &display,
        &[
            "keydown", "ctrl", "keydown", "alt", "keyup", "alt", "keyup", "ctrl",
        ],
    );
    wait_state(
        "the pointer to be released",
        Duration::from_secs(20),
        &mut gui,
        &log,
        &s,
        |m| m["captured"] == false,
    );

    // Absolute, from the toolbar button: the arrow lands on the pointer.
    // The picture starts at (320, 153) on this screen, shown at 1:1. QEMU
    // learns where the guest's cursor is from what the head publishes, and
    // closes the loop on that, so the arrow must land on the pointer.
    click_button(&display, &s, "absolute");
    wait_state(
        "absolute mode",
        Duration::from_secs(20),
        &mut gui,
        &log,
        &s,
        |m| m["pointer_mode"] == "absolute",
    );
    let before = rig.out.join("irix-before.png");
    park();
    std::thread::sleep(Duration::from_secs(1));
    screenshot(&display, &before);
    for (x, y) in [(700i64, 500i64), (900, 640), (1000, 300), (400, 200)] {
        let want = (x - 320, y - 153);
        key(&display, &["mousemove", &x.to_string(), &y.to_string()]);
        std::thread::sleep(Duration::from_secs(4));
        park();
        std::thread::sleep(Duration::from_secs(1));
        screenshot(&display, &shot);
        let got = red_arrow(&shot);
        assert!(
            got.is_some_and(|(cx, cy)| (cx - want.0).abs() <= ARROW_TOLERANCE
                && (cy - want.1).abs() <= ARROW_TOLERANCE),
            "the guest's arrow is at {got:?}, the pointer was at {want:?} (see {})",
            shot.display()
        );
        eprintln!("pointer at ({x}, {y}): the guest's arrow at {got:?}, wanted {want:?}");
    }
    let after = rig.out.join("irix-after.png");
    screenshot(&display, &after);
    assert!(differing(&before, &after, "1280x1024+320+153") > 20);

    // At an ordinary speed the pointer names a new place every few
    // milliseconds. The viewer sends one in each period, so QEMU's loop
    // gets to finish each walk, and the arrow ends on the pointer without
    // having turned back.
    let sent_before = counter(&s, "moves_sent");
    let start_at = cursor_trace(&trace).len();
    let mut chain: Vec<String> = Vec::new();
    chain.extend(["mousemove", "500", "400"].map(String::from));
    for i in 1..=100 {
        chain.extend(
            [
                "mousemove",
                &(500 + i * 6).to_string(),
                "400",
                "sleep",
                "0.008",
            ]
            .map(String::from),
        );
    }
    let args: Vec<&str> = chain.iter().map(String::as_str).collect();
    key(&display, &args);
    std::thread::sleep(Duration::from_secs(4));
    let sent = counter(&s, "moves_sent") - sent_before;
    let path: Vec<_> = cursor_trace(&trace)[start_at..].to_vec();
    park();
    std::thread::sleep(Duration::from_secs(1));
    screenshot(&display, &shot);
    let got = red_arrow(&shot);
    let want = (1100 - 320, 400 - 153);
    eprintln!(
        "absolute, 101 pointer events over 0.8 s: {sent} targets sent, {} cursor changes, {} turn-backs, arrow at {got:?}, wanted {want:?}",
        path.len(),
        turned_back(&path, true)
    );
    assert!(sent <= 8, "{sent} targets for one sweep");
    // Measured on this build, a viewer that sent every move (one target for
    // each, 64 of them) sent the arrow across the whole head and back, from
    // x = -1 to 1271 in a 600 pixel sweep. QEMU's walk to each target still
    // overshoots a little and comes back; it must not be more than that.
    let (lo, hi) = (
        path.iter().map(|p| p.1).min().unwrap_or(0),
        path.iter().map(|p| p.1).max().unwrap_or(0),
    );
    assert!(
        lo >= 500 - 320 - 200 && hi <= 1100 - 320 + 200,
        "the arrow ranged over x = {lo}..{hi} for a sweep of {}..{}: {path:?}",
        500 - 320,
        1100 - 320
    );
    assert!(
        got.is_some_and(|(cx, cy)| (cx - want.0).abs() <= ARROW_TOLERANCE
            && (cy - want.1).abs() <= ARROW_TOLERANCE),
        "the guest's arrow is at {got:?}, the pointer ended at {want:?} (see {})",
        shot.display()
    );
    assert_eq!(counter(&s, "input_errors"), 0);
    assert_eq!(counter(&s, "gpu_errors"), 0);
    check_copies(&s, rig.gpu.is_some());
    assert_eq!(engine_running(&s), Some(true));
}

/// The guest's cursor over time, from the viewer's trace: (ms, x, y, on).
fn cursor_trace(path: &Path) -> Vec<(u64, i32, i32, u8)> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let mut f = l.split(' ');
            Some((
                f.next()?.parse().ok()?,
                f.next()?.parse().ok()?,
                f.next()?.parse().ok()?,
                f.next()?.parse().ok()?,
            ))
        })
        .collect()
}

/// How many times the cursor's x turned back against the way it was going
/// by more than 2 pixels, for a pointer that only goes right (`right`) or
/// only goes left.
fn turned_back(path: &[(u64, i32, i32, u8)], right: bool) -> usize {
    let mut peak = path.first().map_or(0, |p| p.1);
    let mut turns = 0;
    for p in path {
        let x = if right { p.1 } else { -p.1 };
        let peak_x = if right { peak } else { -peak };
        if x > peak_x {
            peak = p.1;
        } else if peak_x - x > 2 {
            turns += 1;
            peak = p.1;
        }
    }
    turns
}

/// The spread of the cursor's y over a path.
fn vertical_range(path: &[(u64, i32, i32, u8)]) -> i32 {
    let ys = path.iter().map(|p| p.2);
    ys.clone().max().unwrap_or(0) - ys.min().unwrap_or(0)
}

// ---- the serial console, the logs and the machine controls -----------------
//
// These read the viewer's own account of what it shows (`machines` in the
// stats file: the terminal's screen, the log lines, the status line, where each
// button is) and drive it with real X events, as a user would. Screenshots are
// kept for a person to look at.

fn machine_state(stats_file: &Path) -> Option<serde_json::Value> {
    let m = stats(stats_file)?["machines"].get(0)?.clone();
    (!m.is_null()).then_some(m)
}

fn text_of(m: &serde_json::Value, key: &str) -> String {
    m[key].as_str().unwrap_or_default().to_string()
}

/// The lines of the terminal's screen, trailing blanks removed.
fn screen_lines(m: &serde_json::Value) -> Vec<String> {
    text_of(m, "serial_screen")
        .lines()
        .map(|l| l.trim_end().to_string())
        .collect()
}

fn has_line(m: &serde_json::Value, line: &str) -> bool {
    screen_lines(m).iter().any(|l| l == line)
}

fn main_window(display: &str) -> String {
    run_ok(
        Command::new("xdotool")
            .env("DISPLAY", display)
            .args(["search", "--name", "^origami$"]),
    )
    .lines()
    .next()
    .expect("the origami window")
    .to_string()
}

fn mouse(display: &str, args: &[&str]) {
    let win = main_window(display);
    let mut cmd = Command::new("xdotool");
    cmd.env("DISPLAY", display);
    // Every `mousemove` in `args` is relative to the window.
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if *a == "mousemove" {
            cmd.args([
                "mousemove",
                "--window",
                &win,
                it.next().unwrap(),
                it.next().unwrap(),
            ]);
        } else {
            cmd.arg(a);
        }
    }
    run_ok(&mut cmd);
}

fn click_button(display: &str, stats_file: &Path, name: &str) {
    let m = machine_state(stats_file).expect("the viewer's state");
    let b = m["buttons"]
        .as_array()
        .and_then(|bs| bs.iter().find(|b| b["name"] == name))
        .unwrap_or_else(|| panic!("no button {name}: {}", m["buttons"]));
    let (x, y) = (
        b["x"].as_f64().unwrap() as i64,
        b["y"].as_f64().unwrap() as i64,
    );
    mouse(
        display,
        &["mousemove", &x.to_string(), &y.to_string(), "click", "1"],
    );
}

/// The centre of terminal cell (`row`, `col`), in window pixels.
fn cell_point(m: &serde_json::Value, row: usize, col: usize) -> (i64, i64) {
    let (tv, c) = (&m["text_view"], &m["cell"]);
    let scale = c["scale"].as_f64().unwrap_or(1.0);
    let (cw, ch) = (c["w"].as_f64().unwrap(), c["h"].as_f64().unwrap());
    (
        (tv["x"].as_f64().unwrap() + (col as f64 + 0.5) * cw / scale) as i64,
        (tv["y"].as_f64().unwrap() + (row as f64 + 0.5) * ch / scale) as i64,
    )
}

fn focus_view(display: &str, stats_file: &Path) {
    let m = machine_state(stats_file).expect("state");
    let (x, y) = cell_point(&m, 2, 2);
    mouse(
        display,
        &["mousemove", &x.to_string(), &y.to_string(), "click", "1"],
    );
}

fn type_text(display: &str, text: &str) {
    run_ok(Command::new("xdotool").env("DISPLAY", display).args([
        "type",
        "--delay",
        "60",
        "--clearmodifiers",
        text,
    ]));
}

fn press(display: &str, keys: &str) {
    key(display, &["key", "--clearmodifiers", keys]);
}

/// Wait until `f` holds for the viewer's state of the first machine,
/// failing early if the viewer died.
fn wait_state(
    what: &str,
    timeout: Duration,
    gui: &mut Proc,
    log: &Path,
    stats_file: &Path,
    mut f: impl FnMut(&serde_json::Value) -> bool,
) -> serde_json::Value {
    let start = Instant::now();
    loop {
        alive(gui, log);
        if let Some(m) = machine_state(stats_file) {
            if f(&m) {
                return m;
            }
        }
        if start.elapsed() > timeout {
            let last = machine_state(stats_file)
                .map(|m| {
                    format!(
                        "status: {}\nnotice: {} | busy: {} | console: {}\nscreen:\n{}",
                        text_of(&m, "status"),
                        text_of(&m, "notice"),
                        text_of(&m, "busy"),
                        m["console_kind"],
                        text_of(&m, "serial_screen")
                    )
                })
                .unwrap_or_default();
            let gui_log = std::fs::read_to_string(log).unwrap_or_default();
            panic!(
                "timed out after {timeout:?} waiting for {what}\n{last}\nviewer log:\n{gui_log}"
            );
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// What QEMU wrote 65,371 times to a machine's runner.log when a viewer could
/// not be given its display.
const SPAM: &str =
    "qemu-system-mips64: sgi-display-shm: failed to pass the region to the viewer: Operation not permitted";

fn append_to_runner_log(machine: &Path, line: &str, times: usize) {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(machine.join("logs/runner.log"))
        .unwrap();
    for _ in 0..times / 1000 {
        f.write_all(format!("{line}\n").repeat(1000).as_bytes())
            .unwrap();
    }
    f.write_all(format!("{line}\n").repeat(times % 1000).as_bytes())
        .unwrap();
}

fn log_entry_count(m: &serde_json::Value, key: &str, text: &str) -> u64 {
    m[key]
        .as_array()
        .and_then(|l| l.iter().find(|e| e["text"] == text))
        .and_then(|e| e["count"].as_u64())
        .unwrap_or(0)
}

#[test]
#[ignore = "boots IRIX under Xvfb and needs the published disk; see the header"]
fn irix_serial_login_command_scrollback_copy_paste_logs_and_restart() {
    let disk = var("ORIGAMI_GUI_TEST_IRIX_DISK");
    let sum = run_ok(Command::new("sha256sum").arg(&disk));
    assert!(
        sum.starts_with(IRIX_DISK_SHA256),
        "not the published disk: {sum}"
    );
    let guard = DiffGuard(disk.clone());
    guard.clear();

    let rig = Rig::start("serial");
    let display = rig.display.clone();
    let machine = rig.create("octane2", "octane2-vpro");
    run_ok(
        Command::new(&rig.origami)
            .arg("drive-attach")
            .arg(&machine)
            .arg(&disk)
            .args(["--type", "disk", "--target", "1", "--overlay"]),
    );
    let _stop = rig.run(&machine);
    // A fresh NVRAM puts IRIX's console on the serial line, and the viewer
    // opens on the serial view.
    let (mut gui, s, log) = rig.spawn_gui(&machine, "serial", &["--view", "serial"]);
    let shot = |name: &str| screenshot(&display, &rig.out.join(name));
    let wait =
        |what: &str, secs: u64, gui: &mut Proc, f: &mut dyn FnMut(&serde_json::Value) -> bool| {
            wait_state(what, Duration::from_secs(secs), gui, &log, &s, f)
        };

    let started = Instant::now();
    let m = wait("IRIX's serial login prompt", 1200, &mut gui, &mut |m| {
        text_of(m, "serial_screen").contains("login:")
    });
    eprintln!("serial login prompt after {:?}", started.elapsed());
    assert_eq!(text_of(&m, "view"), "serial");
    assert_eq!(m["serial_connected"], true);
    shot("serial-1-login-prompt.png");

    // Log in as the guest's user does: with the keyboard, in the view.
    focus_view(&display, &s);
    type_text(&display, "root");
    press(&display, "Return");
    wait("the TERM prompt", 60, &mut gui, &mut |m| {
        text_of(m, "serial_screen").contains("TERM = (")
    });
    press(&display, "Return");
    wait("a root shell prompt", 60, &mut gui, &mut |m| {
        screen_lines(m).last().is_some_and(|l| l.ends_with('#'))
    });
    shot("serial-2-logged-in.png");

    // A command and its output.
    type_text(&display, "echo origami-marker-4242");
    press(&display, "Return");
    wait("the command's output", 60, &mut gui, &mut |m| {
        has_line(m, "origami-marker-4242")
    });
    type_text(&display, "uname -s");
    press(&display, "Return");
    wait("uname's output", 60, &mut gui, &mut |m| {
        has_line(m, "IRIX64")
    });

    // Ctrl-C reaches the guest: a shell blocked on sleep gets its prompt back.
    type_text(&display, "sleep 600");
    press(&display, "Return");
    std::thread::sleep(Duration::from_secs(2));
    press(&display, "ctrl+c");
    type_text(&display, "echo after-ctrl-c-77");
    press(&display, "Return");
    wait("the prompt after Ctrl-C", 30, &mut gui, &mut |m| {
        has_line(m, "after-ctrl-c-77")
    });

    // Esc reaches the guest: vi leaves insert mode only on it, so the colon
    // command runs, vi quits, and the next command reaches the shell.
    type_text(&display, "vi /tmp/origami-esc");
    press(&display, "Return");
    std::thread::sleep(Duration::from_secs(4));
    type_text(&display, "iabc");
    press(&display, "Escape");
    type_text(&display, ":q!");
    press(&display, "Return");
    std::thread::sleep(Duration::from_secs(2));
    type_text(&display, "echo after-esc-88");
    press(&display, "Return");
    wait("the shell after vi", 60, &mut gui, &mut |m| {
        has_line(m, "after-esc-88")
    });

    // Scrollback: output longer than the screen, then the wheel.
    type_text(&display, "ls -l /usr/bin");
    press(&display, "Return");
    let m = wait("scrollback to build", 60, &mut gui, &mut |m| {
        m["serial_history"].as_u64().unwrap_or(0) > 60
    });
    let (px, py) = cell_point(&m, 10, 10);
    mouse(&display, &["mousemove", &px.to_string(), &py.to_string()]);
    for _ in 0..6 {
        key(&display, &["click", "4"]);
    }
    let m = wait("the view to scroll back", 20, &mut gui, &mut |m| {
        m["serial_scrolled"].as_u64().unwrap_or(0) > 5
    });
    eprintln!(
        "scrolled back {} of {} lines",
        m["serial_scrolled"], m["serial_history"]
    );
    shot("serial-3-scrolled-back.png");
    // A key goes to the guest and brings the end back.
    press(&display, "space");
    wait("the view to return to the end", 20, &mut gui, &mut |m| {
        m["serial_scrolled"] == 0
    });
    press(&display, "ctrl+u");

    // Copy and paste: drag over a line the guest printed, copy it, paste it
    // back to the guest and see it echoed again.
    type_text(&display, "echo copyme-31337");
    press(&display, "Return");
    let m = wait("the line to copy", 30, &mut gui, &mut |m| {
        has_line(m, "copyme-31337")
    });
    let row = screen_lines(&m)
        .iter()
        .rposition(|l| l == "copyme-31337")
        .unwrap();
    let (x0, y0) = cell_point(&m, row, 0);
    let (x1, _) = cell_point(&m, row, 11);
    mouse(
        &display,
        &[
            "mousemove",
            &x0.to_string(),
            &y0.to_string(),
            "mousedown",
            "1",
        ],
    );
    for step in 1..=6 {
        let x = x0 + (x1 - x0) * step / 6;
        mouse(&display, &["mousemove", &x.to_string(), &y0.to_string()]);
    }
    mouse(&display, &["mouseup", "1"]);
    let m = wait("the selection", 20, &mut gui, &mut |m| {
        m["selection"] == "copyme-31337"
    });
    shot("serial-4-selection.png");
    click_button(&display, &s, "copy");
    wait("the copy", 20, &mut gui, &mut |m| {
        m["copied"] == "copyme-31337"
    });
    let before = screen_lines(&m)
        .iter()
        .filter(|l| *l == "copyme-31337")
        .count();
    focus_view(&display, &s);
    type_text(&display, "echo ");
    click_button(&display, &s, "paste");
    press(&display, "Return");
    wait("the pasted text echoed", 30, &mut gui, &mut |m| {
        screen_lines(m)
            .iter()
            .filter(|l| *l == "copyme-31337")
            .count()
            > before
    });
    // The Ctrl+Shift chord pastes too, without the button.
    let after_button = screen_lines(&machine_state(&s).unwrap())
        .iter()
        .filter(|l| *l == "copyme-31337")
        .count();
    type_text(&display, "echo ");
    press(&display, "ctrl+shift+v");
    press(&display, "Return");
    wait("the chord's paste echoed", 30, &mut gui, &mut |m| {
        screen_lines(m)
            .iter()
            .filter(|l| *l == "copyme-31337")
            .count()
            > after_button
    });
    shot("serial-5-after-paste.png");

    // The logs: QEMU's runner log with a spamming error, merged to one line.
    click_button(&display, &s, "view-logs");
    append_to_runner_log(&machine, SPAM, 65_371);
    let m = wait(
        "the repeated error merged into one line",
        60,
        &mut gui,
        &mut |m| text_of(m, "view") == "logs" && log_entry_count(m, "runner_log", SPAM) == 65_371,
    );
    assert!(
        m["serial_log"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["text"].as_str().unwrap().contains("copyme-31337")),
        "serial.log in the view: {}",
        m["serial_log"]
    );
    std::thread::sleep(Duration::from_secs(1));
    shot("logs-1-merged-spam.png");

    // Copy from the logs: drag over the start of the first line of the
    // runner log, then select the whole log, with and without the repeat
    // count on a merged line.
    let m = machine_state(&s).unwrap();
    let (x0, y0) = cell_point(&m, 1, 0);
    let (x1, y1) = cell_point(&m, 1, 9);
    mouse(
        &display,
        &[
            "mousemove",
            &x0.to_string(),
            &y0.to_string(),
            "mousedown",
            "1",
        ],
    );
    for step in 1..=5 {
        let x = x0 + (x1 - x0) * step / 5;
        mouse(&display, &["mousemove", &x.to_string(), &y1.to_string()]);
    }
    mouse(&display, &["mouseup", "1"]);
    let m = wait("a drag in the logs", 20, &mut gui, &mut |m| {
        m["log_selection"]
            .as_str()
            .is_some_and(|t| t.chars().count() == 10)
    });
    let dragged = text_of(&m, "log_selection");
    press(&display, "ctrl+c");
    wait("the drag copied with Ctrl+C", 20, &mut gui, &mut |m| {
        m["copied"] == dragged.as_str()
    });
    press(&display, "ctrl+a");
    let m = wait("the whole log selected", 20, &mut gui, &mut |m| {
        m["log_selection"]
            .as_str()
            .is_some_and(|t| t.contains(SPAM))
    });
    assert_eq!(
        text_of(&m, "log_selection").matches(SPAM).count(),
        1,
        "a merged line is copied once"
    );
    press(&display, "ctrl+c");
    wait("select all copied", 20, &mut gui, &mut |m| {
        text_of(m, "copied").contains(SPAM) && !text_of(m, "copied").contains("(x65371)")
    });
    click_button(&display, &s, "counts");
    wait("the repeat count on", 20, &mut gui, &mut |m| {
        m["copy_counts"] == true
    });
    click_button(&display, &s, "copy");
    wait("the copy with its count", 20, &mut gui, &mut |m| {
        text_of(m, "copied").contains(&format!("{SPAM} (x65371)"))
    });
    shot("logs-2-selected.png");

    // Stop from the UI: the status names the last error runner.log holds.
    click_button(&display, &s, "stop");
    let m = wait("the machine to stop", 600, &mut gui, &mut |m| {
        m["running"] == false
    });
    let status = text_of(&m, "status");
    assert!(status.contains("QEMU is not running"), "{status}");
    assert!(
        status.contains(SPAM) && status.contains("65371"),
        "{status}"
    );
    wait_for("the runner to exit", Duration::from_secs(20), || {
        run_ok(Command::new(&rig.origami).arg("status").arg(&machine)).starts_with("stopped")
    });
    shot("controls-1-stopped.png");

    // Start from the UI: QEMU runs again and the serial console reconnects.
    click_button(&display, &s, "start");
    wait("the machine to run again", 300, &mut gui, &mut |m| {
        m["running"] == true && m["serial_connected"] == true
    });
    click_button(&display, &s, "view-serial");
    let m = wait(
        "the new boot on the serial console",
        300,
        &mut gui,
        &mut |m| {
            text_of(m, "serial_screen").contains("NVRAM")
                || text_of(m, "serial_screen").contains("login:")
        },
    );
    assert!(run_ok(Command::new(&rig.origami).arg("status").arg(&machine)).contains("running"));
    eprintln!("after restart: {}", text_of(&m, "status"));
    shot("controls-2-restarted.png");
    assert_eq!(counter(&s, "input_errors"), 0);
    assert_eq!(counter(&s, "gpu_errors"), 0);
}

#[test]
#[ignore = "boots QEMU under Xvfb; see the header"]
fn the_status_says_why_there_is_no_picture_and_the_controls_recover_it() {
    let rig = Rig::start("status");
    let display = rig.display.clone();
    let machine = rig.create("octane2", "octane2-vpro");
    // Started the way a failed display looks to the viewer: QEMU runs and
    // there is no display socket to connect to.
    run_ok(Command::new(&rig.origami).arg("run").arg(&machine).args([
        "--display",
        "none",
        "--background",
    ]));
    let _stop = StopMachine(rig.origami.clone(), machine.clone());
    let (mut gui, s, log) = rig.spawn_gui(&machine, "status", &["--debug"]);
    let shot = |name: &str| screenshot(&display, &rig.out.join(name));
    let wait =
        |what: &str, secs: u64, gui: &mut Proc, f: &mut dyn FnMut(&serde_json::Value) -> bool| {
            wait_state(what, Duration::from_secs(secs), gui, &log, &s, f)
        };

    let m = wait(
        "the status to name the missing socket",
        60,
        &mut gui,
        &mut |m| text_of(m, "status").contains("display socket does not exist"),
    );
    assert_eq!(m["debug"], true, "--debug turns the debug panel on");
    assert!(text_of(&m, "debug_text").contains("protocol: socket absent"));
    shot("status-1-no-socket.png");

    // QEMU's error, written tens of thousands of times: said once, with its count.
    append_to_runner_log(&machine, SPAM, 65_371);
    let m = wait("the status to carry QEMU's error", 60, &mut gui, &mut |m| {
        let status = text_of(m, "status");
        status.contains("QEMU reported") && status.contains("65371")
    });
    let status = text_of(&m, "status");
    assert!(
        status.contains(SPAM) && status.contains("65371"),
        "{status}"
    );
    shot("status-2-reported-error.png");

    // The debug button toggles the panel.
    click_button(&display, &s, "debug");
    wait("the debug panel off", 20, &mut gui, &mut |m| {
        m["debug"] == false
    });
    click_button(&display, &s, "debug");
    wait("the debug panel on", 20, &mut gui, &mut |m| {
        m["debug"] == true
    });

    // Force stop ends QEMU at once.
    click_button(&display, &s, "force-stop");
    wait("the machine to be force stopped", 60, &mut gui, &mut |m| {
        m["running"] == false
    });
    // The runner drops its run record, which the viewer goes by, a moment
    // before it lets go of the machine lock.
    wait_for("the runner to exit", Duration::from_secs(20), || {
        run_ok(Command::new(&rig.origami).arg("status").arg(&machine)).starts_with("stopped")
    });
    shot("status-3-force-stopped.png");

    // The PROM's console, set on the stopped machine.
    let console = |want: &str| {
        let out = run_ok(
            Command::new(&rig.origami)
                .arg("nvram")
                .arg(&machine)
                .args(["get", "console"]),
        );
        assert_eq!(out.trim(), format!("console={want}"));
    };
    click_button(&display, &s, "console-serial");
    wait("the serial console setting", 60, &mut gui, &mut |m| {
        text_of(m, "notice").contains("console is now serial") && m["busy"] == ""
    });
    console("d");
    click_button(&display, &s, "console-graphics");
    wait("the graphics console setting", 60, &mut gui, &mut |m| {
        text_of(m, "notice").contains("console is now graphics") && m["busy"] == ""
    });
    console("g");
    shot("status-4-console-graphics.png");

    // Start from the UI: this time QEMU gets the shared-memory display, and
    // the picture comes, drawn without a copy where the GPU allows.
    click_button(&display, &s, "start");
    let m = wait("the picture", 600, &mut gui, &mut |m| {
        text_of(m, "status").starts_with("1280x1024")
    });
    let status = text_of(&m, "status");
    eprintln!("status with a picture: {status}");
    let want = if rig.gpu.is_some() {
        "zero copy: yes"
    } else {
        "zero copy: no"
    };
    assert!(status.contains(want), "{status}");
    wait_for("the PROM to draw", Duration::from_secs(300), || {
        alive(&mut gui, &log);
        counter(&s, "frames_taken") > 20
    });
    shot("status-5-picture.png");

    // Reset from the UI: the guest restarts, QEMU keeps running.
    click_button(&display, &s, "reset");
    wait("the reset", 60, &mut gui, &mut |m| {
        text_of(m, "notice") == "reset sent"
    });
    assert!(run_ok(Command::new(&rig.origami).arg("status").arg(&machine)).contains("running"));
    assert_eq!(counter(&s, "gpu_errors"), 0);
}
