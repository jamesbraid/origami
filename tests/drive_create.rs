#![cfg(unix)]

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::{SystemTime, UNIX_EPOCH};

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "origami-drive-create-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn run(root: &Path, args: &[&str]) -> Output {
    common::origami(Path::new(env!("CARGO_BIN_EXE_origami")), root)
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn drive_create_rejects_an_occupied_system_target_before_writing() {
    let root = TempDir::new();
    let prom = root.path().join("synthetic-prom.bin");
    fs::write(&prom, vec![0; 1024 * 1024]).unwrap();
    let machine = root.path().join("machine");
    let create = run(
        root.path(),
        &[
            "create",
            machine.to_str().unwrap(),
            "--preset",
            "origin200-1",
            "--prom",
            prom.to_str().unwrap(),
        ],
    );
    assert!(
        create.status.success(),
        "{}",
        String::from_utf8_lossy(&create.stderr)
    );

    let sentinel = root.path().join("sentinel-disk.img");
    fs::write(&sentinel, b"external disk sentinel\n").unwrap();
    let attach = run(
        root.path(),
        &[
            "drive-attach",
            machine.to_str().unwrap(),
            sentinel.to_str().unwrap(),
            "--type",
            "disk",
            "--target",
            "1",
        ],
    );
    assert!(
        attach.status.success(),
        "{}",
        String::from_utf8_lossy(&attach.stderr)
    );

    let machine_file = machine.join("machine.toml");
    let machine_before = fs::read(&machine_file).unwrap();
    let sentinel_before = fs::read(&sentinel).unwrap();
    // The fake runtime has no qemu-img, so reaching it would fail differently.
    assert!(!root.path().join("runtime/qemu-img").exists());
    let create_disk = run(
        root.path(),
        &["drive-create", machine.to_str().unwrap(), "16"],
    );

    let stderr = String::from_utf8_lossy(&create_disk.stderr);
    assert!(!create_disk.status.success());
    assert!(
        stderr.contains("target 1") && stderr.contains("occupied"),
        "expected occupied target diagnostic, got: {stderr}"
    );
    assert_eq!(fs::read(&machine_file).unwrap(), machine_before);
    assert_eq!(fs::read(&sentinel).unwrap(), sentinel_before);
    assert!(!machine.join("drives/system.qcow2").exists());
}
