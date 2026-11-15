//! A real Octane2 in a real window under a virtual X server with a window
//! manager (a bare X server delivers no key events to a window). One test
//! shows the PROM, sends a key and pops the view out a hundred times; the other
//! boots IRIX and watches the guest's cursor follow the host pointer.
//!
//! Ignored by default: it boots QEMU and needs Xvfb, openbox, xdotool and
//! ImageMagick on PATH, a Vulkan driver (software is fine), and these:
//!
//! - ORIGAMI_RUNTIME_DIR: the directory holding the QEMU the product runs,
//!   built from a QEMU with the shared-memory display (`display-shm`)
//! - ORIGAMI_BIN: the `origami` executable
//! - ORIGAMI_GUI_TEST_DIR: an empty or new directory for machines and
//!   evidence (screenshots are left in each test's `out` directory)
//! - ORIGAMI_GUI_TEST_IRIX_DISK (the IRIX test only): the published
//!   `octane-games-be780408.chd` (SHA-256 below), which the test boots with an
//!   overlay and so leaves a `.diff.chd` beside, removed at the start and end
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
const CONTINUE_BUTTON: (u32, u32) = (1262, 870);

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
        Rig {
            origami: var("ORIGAMI_BIN"),
            gui: PathBuf::from(env!("CARGO_BIN_EXE_origami-gui")),
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

    /// The machine runs apart from every view: start it here, attach below.
    fn run(&self, machine: &Path) -> StopMachine {
        let mut cmd = Command::new(&self.origami);
        cmd.arg("run")
            .arg(machine)
            .args(["--display", "shm", "--background"]);
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

    // 2. Nothing is redrawn while nothing changes.
    let sample = || {
        (
            counter(&s1, "before_renders"),
            counter(&s1, "passes"),
            counter(&s1, "rows_uploaded"),
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
    let changed = differing(&prom, &after, "1280x1024+320+128");
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
    key(&display, &["mousemove", "404", "98", "click", "1"]);
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
    run_ok(Command::new(&origami).arg("run").arg(&machine).args([
        "--display",
        "shm",
        "--background",
    ]));
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
            .args(["-crop", "1280x1024+320+128", "+repage", "-fuzz", "12%"])
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

    // The picture starts at (320, 128) on this screen, shown at 1:1. QEMU
    // learns where the guest's cursor is from what the head publishes, and
    // closes the loop on that, so the arrow must land on the pointer.
    for (x, y) in [(700i64, 500i64), (900, 640), (1000, 300), (400, 200)] {
        let want = (x - 320, y - 128);
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
    assert!(differing(&before, &after, "1280x1024+320+128") > 20);
    assert_eq!(counter(&s, "input_errors"), 0);
    assert_eq!(counter(&s, "gpu_errors"), 0);
    assert_eq!(engine_running(&s), Some(true));
}
