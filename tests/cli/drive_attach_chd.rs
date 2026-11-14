use crate::support;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;

fn run(root: &Path, args: &[&str]) -> Output {
    support::origami(root).args(args).output().unwrap()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn machine(root: &Path) -> PathBuf {
    let prom = root.join("synthetic-prom.bin");
    fs::write(&prom, vec![0; 1024 * 1024]).unwrap();
    let machine = root.join("machine");
    let create = run(
        root,
        &[
            "create",
            machine.to_str().unwrap(),
            "--preset",
            "origin200-1",
            "--prom",
            prom.to_str().unwrap(),
        ],
    );
    assert!(create.status.success(), "{}", stderr(&create));
    machine
}

/// The first 64 bytes of a version 5 CHD header, which is all attach reads.
fn chd(root: &Path, name: &str, unit: u32) -> PathBuf {
    let mut header = vec![0u8; 124];
    header[..8].copy_from_slice(b"MComprHD");
    header[8..12].copy_from_slice(&124u32.to_be_bytes());
    header[12..16].copy_from_slice(&5u32.to_be_bytes());
    header[60..64].copy_from_slice(&unit.to_be_bytes());
    let path = root.join(name);
    fs::write(&path, header).unwrap();
    path
}

fn attach(root: &Path, machine: &Path, image: &Path, extra: &[&str]) -> Output {
    let mut args = vec![
        "drive-attach",
        machine.to_str().unwrap(),
        image.to_str().unwrap(),
    ];
    args.extend(extra);
    run(root, &args)
}

fn drives(machine: &Path) -> String {
    fs::read_to_string(machine.join("machine.toml")).unwrap()
}

#[test]
fn a_chd_attaches_as_a_disk() {
    let root = tempfile::tempdir().unwrap();
    let machine = machine(root.path());
    let image = chd(root.path(), "irix.chd", 512);
    let out = attach(
        root.path(),
        &machine,
        &image,
        &["--type", "disk", "--target", "1"],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    let toml = drives(&machine);
    assert!(
        toml.contains("irix.chd") && toml.contains("read_only = false"),
        "{toml}"
    );
    assert!(!toml.contains("overlay"), "{toml}");
}

#[test]
fn overlay_is_remembered_for_a_chd_disk() {
    let root = tempfile::tempdir().unwrap();
    let machine = machine(root.path());
    let image = chd(root.path(), "irix.chd", 512);
    let out = attach(
        root.path(),
        &machine,
        &image,
        &["--type", "disk", "--target", "1", "--overlay"],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(drives(&machine).contains("overlay = true"));
}

#[test]
fn a_chd_is_refused_as_a_cdrom_or_tape() {
    let root = tempfile::tempdir().unwrap();
    let machine = machine(root.path());
    let image = chd(root.path(), "disc.chd", 512);
    let before = drives(&machine);
    for kind in ["cdrom", "tape"] {
        let out = attach(
            root.path(),
            &machine,
            &image,
            &["--type", kind, "--target", "4"],
        );
        assert!(!out.status.success());
        assert!(
            stderr(&out).contains("CD-ROM and tape CHDs are not supported"),
            "{}",
            stderr(&out)
        );
    }
    assert_eq!(drives(&machine), before);
}

#[test]
fn a_file_that_is_not_a_hard_disk_chd_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let machine = machine(root.path());
    let before = drives(&machine);
    let text = root.path().join("text.chd");
    fs::write(&text, vec![b'x'; 200]).unwrap();
    let cd = chd(root.path(), "cd.chd", 2448);
    for (image, wanted) in [(&text, "not a CHD image"), (&cd, "only hard disk CHDs")] {
        let out = attach(
            root.path(),
            &machine,
            image,
            &["--type", "disk", "--target", "1"],
        );
        assert!(!out.status.success());
        assert!(stderr(&out).contains(wanted), "{}", stderr(&out));
    }
    assert_eq!(drives(&machine), before);
}

#[test]
fn overlay_is_refused_for_other_image_formats() {
    let root = tempfile::tempdir().unwrap();
    let machine = machine(root.path());
    let image = root.path().join("disk.img");
    fs::write(&image, b"raw").unwrap();
    let before = drives(&machine);
    let out = attach(
        root.path(),
        &machine,
        &image,
        &["--type", "disk", "--target", "1", "--overlay"],
    );
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("overlay applies to CHD"),
        "{}",
        stderr(&out)
    );
    assert_eq!(drives(&machine), before);
}

#[test]
fn a_header_shorter_than_a_chd_header_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let machine = machine(root.path());
    let short = root.path().join("short.chd");
    fs::write(&short, b"MComprHD").unwrap();
    let out = attach(
        root.path(),
        &machine,
        &short,
        &["--type", "disk", "--target", "1"],
    );
    assert!(!out.status.success());
    assert!(stderr(&out).contains("not a CHD image"), "{}", stderr(&out));
}
