//! IRIX 6.5.30 installs from the public media on an Origin 200 and boots to
//! a login.
//!
//! The steps are the ones a person follows: `origami install-init` points the
//! machine at the bundled Instigator, which serves the media from
//! origami-dist; `install-serve` runs that server; the PROM's menus are driven
//! over the serial console until the miniroot reaches `Inst>`; `install-apply`
//! types the package selection and waits for the first boot.
//!
//! An install that stalls leaves little behind, so a failure saves what tells
//! the layers apart (support/capture.rs) before anything is stopped.

use std::process::Stdio;
use std::time::Instant;

use regex::Regex;
use serde_json::{json, Value};

use crate::support::capture;
use crate::support::cleanup;
use crate::support::console::{login, Console};
use crate::support::guest::Guest;
use crate::support::product::Product;
use crate::support::{pause, secs, Result};

const PRESET: &str = "origin200-1";
const DISK_MB: &str = "4096";
const SERVER: &str = "10.98.0.2";
/// Inside the workflow step's 180 minute limit, with room for the capture.
const INSTALL_DEADLINE_MINUTES: u64 = 165;

/// Menus and prompts, answered in order. Each wait names what the PROM or
/// fx prints next.
fn drive(con: &mut Console, serving: &dyn Fn() -> Result<()>) -> Result<()> {
    let wait = |con: &mut Console, pattern: &str, s: u64| con.expect(pattern, secs(s)).map(|_| ());
    wait(con, r"Option\? ", 300)?;
    // Instigator reads the media's directories once the machine connects,
    // which takes minutes. A BOOTP request before that goes unanswered and
    // the PROM gives up, so the menu waits.
    serving()?;
    con.send("5\r")?;
    wait(con, r">> ", 60)?;
    con.line("boot -f bootp():/6.5.30/stand/fx.64")?;
    // fx: extended mode, device name, controller and drive all take defaults.
    wait(
        con,
        r"extended mode with all options available\? \(no\)",
        300,
    )?;
    for prompt in [r"\(dksc\) ", r"ctlr# = \(0\) ", r"drive# = \(1\) "] {
        con.send("\r")?;
        wait(con, prompt, 60)?;
    }
    con.send("\r")?;
    wait(con, r"fx> ", 60)?;
    con.line("exit")?;
    wait(con, r"write out changes\? \(yes\)", 60)?;
    con.send("\r")?;
    wait(con, r"Option\? ", 120)?;
    con.send("2\r")?;
    wait(con, r"or <enter> to start: ", 60)?;
    con.send("1\r")?;
    wait(con, r"name of the remote host: ", 60)?;
    con.line(SERVER)?;
    wait(con, r"(?i)directory.*: ", 60)?;
    con.line("/6.5.30/dist")?;
    wait(con, r"<enter> to start: ", 60)?;
    con.send("\r")?;

    // Keep answering until the miniroot reaches Inst>.
    let answers = [
        (
            r"Make new file system on /dev/dsk/realroot \[yes/no/sh/help\]: $",
            "yes",
        ),
        (r"Are you sure\? \[y/n\] \(n\): $", "y"),
        (r"Block size of filesystem 512 or 4096 bytes\?\s*$", "4096"),
        (r"hostname \(system name\) of your machine\?\s*$", "irix"),
        (r"network address of irix\?\s*$", "10.98.0.65"),
        (
            r"Press Enter for the IP class default \[0x[0-9a-f]+\]: $",
            "255.255.255.0",
        ),
    ];
    let end = Instant::now() + secs(1800);
    while Instant::now() < end {
        match con.expect(r"Inst> ", secs(20)) {
            Ok(_) => return Ok(()),
            Err(e) if e.to_string().starts_with("timed out") => {
                println!("WAITING>>> {}", tail(&con.buffer, 600));
                let recent = tail(&con.buffer, 300);
                for (pattern, answer) in &answers {
                    if Regex::new(pattern)?.is_match(&recent) {
                        con.line(answer)?;
                        con.buffer.clear();
                        break;
                    }
                }
            }
            Err(e) => return Err(e),
        }
    }
    Err("the miniroot never reached Inst>".into())
}

fn tail(text: &str, n: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    chars[chars.len().saturating_sub(n)..].iter().collect()
}

/// Wait for Instigator to log that it is serving. `install-serve` prints the
/// path of that log first.
fn wait_until_serving(serve_log: &std::path::Path) -> Result<()> {
    let end = Instant::now() + secs(900);
    let mut server_log = None;
    while Instant::now() < end {
        if server_log.is_none() {
            server_log = std::fs::read_to_string(serve_log)
                .unwrap_or_default()
                .lines()
                .find_map(|l| {
                    l.strip_prefix("Instigator log: ")
                        .map(std::path::PathBuf::from)
                });
        }
        if let Some(path) = &server_log {
            let text =
                String::from_utf8_lossy(&std::fs::read(path).unwrap_or_default()).into_owned();
            if text.contains("serving; stop with") {
                return Ok(());
            }
        }
        pause(secs(5));
    }
    Err(format!("Instigator never logged that it was serving ({server_log:?})").into())
}

fn install(
    product: &Product,
    base: &std::path::Path,
    profile: &str,
    serve_pid: &mut Option<u32>,
) -> Result<Value> {
    let logs = product.work.join("logs");
    std::fs::create_dir_all(&logs)?;
    product.origami_ok([
        "create".as_ref(),
        base.as_os_str(),
        "--preset".as_ref(),
        PRESET.as_ref(),
        "--memory-per-node".as_ref(),
        "256".as_ref(),
    ])?;
    product.origami_ok(["drive-create".as_ref(), base.as_os_str(), DISK_MB.as_ref()])?;
    product.origami_ok([
        "install-init".as_ref(),
        base.as_os_str(),
        "--profile".as_ref(),
        profile.as_ref(),
    ])?;
    product.origami_ok(["install-check".as_ref(), base.as_os_str()])?;

    let serve_log = std::fs::File::create(logs.join("install-serve.log"))?;
    let mut serve = product.command(&product.cli);
    serve
        .args(["install-serve"])
        .arg(base)
        .stdin(Stdio::null())
        .stdout(serve_log.try_clone()?)
        .stderr(serve_log);
    let mut serve = serve.spawn()?;
    *serve_pid = Some(serve.id());
    let serve_cleanup = {
        let pid = serve.id();
        let (product, base) = (product.clone(), base.to_path_buf());
        cleanup::register("install-serve", move || {
            // A cancelled or timed-out job lands here: save what shows a
            // stall before the kill below destroys it.
            capture::save(&product.evidence(), "install", &base, Some(pid));
            #[cfg(unix)]
            // SAFETY: signalling a child this test started.
            unsafe {
                libc::kill(pid as i32, libc::SIGTERM)
            };
            #[cfg(not(unix))]
            let _ = pid;
        })
    };
    let outcome = (|| -> Result<Value> {
        pause(secs(5));
        product.origami_ok([
            "run".as_ref(),
            base.as_os_str(),
            "--display".as_ref(),
            "none".as_ref(),
            "--background".as_ref(),
        ])?;
        let started = Instant::now();
        let mut con = Console::connect(base, Some(&logs.join("install-console.log")))?;
        let serving = || wait_until_serving(&logs.join("install-serve.log"));
        drive(&mut con, &serving)?;
        // Hand over at Inst>: install-apply types the package selection.
        con.close();
        // Types the package selection and waits through the install.
        product.origami_ok_for(
            ["install-apply".as_ref(), base.as_os_str()],
            secs(INSTALL_DEADLINE_MINUTES * 60),
        )?;
        let install_seconds = started.elapsed().as_secs();

        // install-apply returns once the login prompt is up on the first boot.
        let mut con = Console::connect(base, Some(&logs.join("first-boot-console.log")))?;
        login(&mut con, secs(300))?;
        let (status, text) = con.run("uname -R", secs(60))?;
        let version = Regex::new(r"6\.5\.\d+\w*")?
            .find(&text)
            .map(|m| m.as_str().to_string());
        if status != 0 || version.as_deref().is_none_or(|v| !v.starts_with("6.5.30")) {
            return Err(format!("`uname -R` printed {text:?} (exit {status}), not 6.5.30").into());
        }
        con.line("halt -p")?;
        pause(secs(30));
        con.close();
        Ok(
            json!({"preset": PRESET, "profile": profile, "uname_R": version, "install_seconds": install_seconds}),
        )
    })();
    if outcome.is_err() {
        // Evidence first: stopping Instigator and QEMU destroys what it shows.
        capture::save(&product.evidence(), "install", base, Some(serve.id()));
    }
    let _ = serve.kill();
    let _ = serve.wait();
    drop(serve_cleanup);
    outcome
}

#[test]
#[ignore = "installs IRIX from the public media (about an hour); needs ORIGAMI_PRODUCT and ORIGAMI_WORK"]
fn irix_installs_and_boots_on_an_origin_200() {
    let product = Product::from_env("install");
    let base = product.machines().join("origin200");
    let profile = std::env::var("ORIGAMI_INSTALL_PROFILE").unwrap_or_else(|_| "minimal".into());
    // Stops the machine at the end, and on a signal.
    let guest = Guest::adopt(&product, "install", "", base.clone());
    let mut serve_pid = None;
    // The job's step timeout gives a cancelled process seconds, and the
    // capture below takes up to twenty. Stop and capture here first.
    let finished = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let watchdog = finished.clone();
    std::thread::spawn(move || {
        pause(secs(INSTALL_DEADLINE_MINUTES * 60));
        if !watchdog.load(std::sync::atomic::Ordering::SeqCst) {
            eprintln!("the install passed its {INSTALL_DEADLINE_MINUTES} minute deadline");
            cleanup::run_all();
            std::process::exit(124);
        }
    });
    let result = install(&product, &base, &profile, &mut serve_pid);
    finished.store(true, std::sync::atomic::Ordering::SeqCst);
    drop(guest);
    match result {
        Ok(data) => {
            let doc = json!({"name": "install", "status": "pass", "data": data});
            let _ = std::fs::write(product.evidence().join("result.json"), doc.to_string());
        }
        Err(e) => {
            let doc = json!({"name": "install", "status": "fail", "error": e.to_string()});
            let _ = std::fs::write(product.evidence().join("result.json"), doc.to_string());
            panic!("install: {e}");
        }
    }
}
