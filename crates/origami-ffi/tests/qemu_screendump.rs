//! A real QEMU Octane2 through the core, dumped with `screendump`.
//!
//! Ignored by default: it starts a machine. Needs ORIGAMI_BIN (the `origami`
//! executable, with ORIGAMI_RUNTIME_DIR for QEMU) and ORIGAMI_GUI_TEST_DIR.
#![cfg(all(unix, feature = "qemu", not(feature = "fake")))]

use origami_ffi::OrigamiCore;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

fn var(name: &str) -> PathBuf {
    std::env::var_os(name)
        .map(PathBuf::from)
        .unwrap_or_else(|| panic!("{name} is not set; see the header"))
}

#[test]
#[ignore = "starts a machine; see the header"]
fn screendump_of_head_zero_writes_a_picture() {
    let bin = var("ORIGAMI_BIN");
    let work = var("ORIGAMI_GUI_TEST_DIR").join(format!("dump-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).unwrap();
    let made = Command::new(&bin)
        .arg("create")
        .arg(work.join("octane2"))
        .args(["--preset", "octane2-vpro"])
        .output()
        .unwrap();
    assert!(
        made.status.success(),
        "{}",
        String::from_utf8_lossy(&made.stderr)
    );
    let dir = work.join("octane2").canonicalize().unwrap();
    let core = OrigamiCore::new(work.to_string_lossy().into_owned(), None).unwrap();
    let machine = core
        .start_machine(dir.to_string_lossy().into_owned())
        .unwrap();
    let started = Instant::now();
    while !machine.view().heads.iter().any(|h| h.active) {
        assert!(started.elapsed() < Duration::from_secs(300), "no picture");
        std::thread::sleep(Duration::from_millis(100));
    }
    let shot = work.join("shot.ppm");
    let dumped = machine.screendump(0, shot.to_string_lossy().into_owned());
    let _ = Command::new(&bin)
        .args(["stop", "--force"])
        .arg(&dir)
        .status();
    dumped.unwrap();
    assert!(std::fs::read(&shot).unwrap().starts_with(b"P6"));
}
