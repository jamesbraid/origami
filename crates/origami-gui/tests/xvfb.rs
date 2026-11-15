//! A real Octane2 PROM in a real window, a key that reaches the guest, and a
//! hundred pop-out and dock cycles, under a virtual X server with a window
//! manager (a bare X server delivers no key events to a window).
//!
//! Ignored by default: it boots QEMU and needs Xvfb, openbox, xdotool and
//! ImageMagick on PATH, a Vulkan driver (software is fine), and these:
//!
//! - ORIGAMI_RUNTIME_DIR: the directory holding the QEMU the product runs,
//!   built from a QEMU with the shared-memory display (`display-shm`)
//! - ORIGAMI_BIN: the `origami` executable
//! - ORIGAMI_GUI_TEST_DIR: an empty or new directory for machines, sockets and
//!   evidence (screenshots are left in its `out` directory)
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
    let nums: Vec<u8> = text
        .trim()
        .trim_start_matches("srgb(")
        .trim_end_matches(')')
        .split(',')
        .map(|n| {
            n.trim()
                .parse()
                .unwrap_or_else(|_| panic!("pixel {text:?}"))
        })
        .collect();
    (nums[0], nums[1], nums[2])
}

fn is_grey((r, g, b): (u8, u8, u8)) -> bool {
    r == g && g == b && r > 100
}

fn key(display: &str, args: &[&str]) {
    run_ok(Command::new("xdotool").env("DISPLAY", display).args(args));
}

#[test]
#[ignore = "boots QEMU under Xvfb; see the header"]
fn an_octane2_prom_in_a_window_a_key_and_a_hundred_pop_outs() {
    let work = var("ORIGAMI_GUI_TEST_DIR");
    let origami = var("ORIGAMI_BIN");
    let gui = PathBuf::from(env!("CARGO_BIN_EXE_origami-gui"));
    let out = work.join("out");
    let xdg = work.join("xdg");
    std::fs::create_dir_all(&out).unwrap();
    std::fs::create_dir_all(&xdg).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&xdg, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let machine = work.join("octane2");
    let _ = std::fs::remove_dir_all(&machine);
    run_ok(
        Command::new(&origami)
            .arg("create")
            .arg(&machine)
            .args(["--preset", "octane2-vpro"]),
    );

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
    let _wm = Proc(
        Command::new("openbox")
            .env("DISPLAY", &display)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("openbox"),
    );
    std::thread::sleep(Duration::from_secs(1));

    // The machine runs apart from every view: start it here, attach below.
    run_ok(
        Command::new(&origami)
            .env("XDG_RUNTIME_DIR", &xdg)
            .arg("run")
            .arg(&machine)
            .args(["--display", "shm", "--background"]),
    );
    struct StopMachine(PathBuf, PathBuf, PathBuf);
    impl Drop for StopMachine {
        fn drop(&mut self) {
            let _ = Command::new(&self.0)
                .arg("stop")
                .arg(&self.1)
                .env("XDG_RUNTIME_DIR", &self.2)
                .status();
        }
    }
    let _stop = StopMachine(origami.clone(), machine.clone(), xdg.clone());

    let spawn_gui = |tag: &str, extra: &[&str]| -> (Proc, PathBuf, PathBuf) {
        let stats = out.join(format!("stats-{tag}.json"));
        let log = out.join(format!("gui-{tag}.log"));
        let child = Command::new(&gui)
            .env("DISPLAY", &display)
            .env("XDG_RUNTIME_DIR", &xdg)
            .env("ORIGAMI_BIN", &origami)
            .env("RUST_BACKTRACE", "1")
            .arg("--stats-file")
            .arg(&stats)
            .args(extra)
            .arg(&machine)
            .stderr(std::fs::File::create(&log).unwrap())
            .stdout(Stdio::null())
            .spawn()
            .expect("origami-gui");
        (Proc(child), stats, log)
    };

    // 1. The PROM on screen. With no disk it runs its power-on diagnostics,
    // which fail, and stops on a dialog that says "Press any key to
    // continue". That dialog has a grey Continue button where the menu behind
    // it has a blue gradient, which is what the wait looks for.
    let (mut gui1, s1, log1) = spawn_gui("view", &[]);
    let alive = |p: &mut Proc, log: &Path| {
        if let Some(status) = p.0.try_wait().unwrap() {
            panic!(
                "origami-gui exited early ({status}): {}",
                std::fs::read_to_string(log).unwrap_or_default()
            );
        }
    };
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
    assert!(run_ok(
        Command::new(&origami)
            .arg("status")
            .arg(&machine)
            .env("XDG_RUNTIME_DIR", &xdg)
    )
    .contains("running"));
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
    assert!(run_ok(
        Command::new(&origami)
            .arg("status")
            .arg(&machine)
            .env("XDG_RUNTIME_DIR", &xdg)
    )
    .contains("running"));

    // 5. The machine ends and is started again under a view that stays open:
    // the view notices, then finds the new display and shows its PROM.
    let (mut gui3, s3, log3) = spawn_gui("restart", &[]);
    wait_for("the view to attach", Duration::from_secs(120), || {
        alive(&mut gui3, &log3);
        counter(&s3, "frames_taken") > 0
    });
    let engine_running = |p: &Path| stats(p).map(|s| s["engine"][0]["running"] == true);
    assert_eq!(engine_running(&s3), Some(true));
    run_ok(
        Command::new(&origami)
            .arg("stop")
            .arg(&machine)
            .env("XDG_RUNTIME_DIR", &xdg),
    );
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
            .env("XDG_RUNTIME_DIR", &xdg)
            .arg("run")
            .arg(&machine)
            .args(["--display", "shm", "--background"]),
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
    drop(gui3);
    assert!(!std::fs::read_to_string(&log3)
        .unwrap_or_default()
        .contains("GPU:"));
}
