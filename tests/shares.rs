#![cfg(unix)]

mod common;

use std::fs;
use std::path::Path;
use std::process::Output;

fn run(root: &Path, args: &[&str]) -> Output {
    common::origami(Path::new(env!("CARGO_BIN_EXE_origami")), root)
        .args(args)
        .output()
        .unwrap()
}

fn ok(root: &Path, args: &[&str]) -> String {
    let output = run(root, args);
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn fail(root: &Path, args: &[&str]) -> String {
    let output = run(root, args);
    assert!(!output.status.success(), "{args:?} succeeded");
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn machine(root: &Path) -> String {
    let prom = root.join("synthetic-prom.bin");
    fs::write(&prom, vec![0; 1024 * 1024]).unwrap();
    let machine = root.join("machine");
    let machine = machine.to_str().unwrap().to_string();
    ok(
        root,
        &[
            "create",
            &machine,
            "--preset",
            "origin200-1",
            "--prom",
            prom.to_str().unwrap(),
        ],
    );
    machine
}

#[test]
fn share_adds_lists_and_removes_a_folder() {
    let root = tempfile::tempdir().unwrap();
    let machine = machine(root.path());
    let code = root.path().join("code");
    fs::create_dir(&code).unwrap();
    let code = code.to_str().unwrap();

    let out = ok(root.path(), &["share", &machine, "src", code]);
    assert!(out.contains("mkdir -p /mnt/src"), "{out}");
    assert!(out.contains("mount 192.0.2.4:/src /mnt/src"), "{out}");
    let toml = fs::read_to_string(Path::new(&machine).join("machine.toml")).unwrap();
    assert!(
        toml.contains("[[share]]") && toml.contains("name = \"src\""),
        "{toml}"
    );

    let show = ok(root.path(), &["show", &machine]);
    assert!(show.contains(&format!("share src: {code}")), "{show}");
    assert!(show.contains("mount 192.0.2.4:/src /mnt/src"), "{show}");

    let err = fail(root.path(), &["share", &machine, "src", code]);
    assert!(err.contains("share src exists"), "{err}");
    let err = fail(root.path(), &["share", &machine, "a/b", code]);
    assert!(err.contains("share name must use"), "{err}");
    let err = fail(root.path(), &["share", &machine, "other", "/no/such/dir"]);
    assert!(err.contains("not a directory"), "{err}");

    ok(root.path(), &["share", &machine, "src", "--remove"]);
    let toml = fs::read_to_string(Path::new(&machine).join("machine.toml")).unwrap();
    assert!(!toml.contains("[[share]]"), "{toml}");
    let err = fail(root.path(), &["share", &machine, "src", "--remove"]);
    assert!(err.contains("unknown share"), "{err}");
}

#[test]
fn shares_need_a_network_that_can_carry_them() {
    let root = tempfile::tempdir().unwrap();
    let machine = machine(root.path());
    let code = root.path().join("code");
    fs::create_dir(&code).unwrap();
    ok(
        root.path(),
        &["share", &machine, "src", code.to_str().unwrap()],
    );

    let err = fail(root.path(), &["network-set", &machine, "--mode", "none"]);
    assert!(
        err.contains("shares need user networking: origami network-set DIR --mode user"),
        "{err}"
    );
    // A host network is the machine's own LAN: the stack needs an address
    // on it. macOS has vmnet where Linux has a tap device.
    let host = if cfg!(target_os = "macos") {
        ["--mode", "vmnet", "--endpoint", "/var/run/socket_vmnet"]
    } else {
        ["--mode", "tap", "--interface", "tap0"]
    };
    let set = |extra: &[&str]| {
        let mut args = vec!["network-set", machine.as_str()];
        args.extend(host);
        args.extend(extra);
        args.iter().map(|a| a.to_string()).collect::<Vec<_>>()
    };
    let run_set = |extra: &[&str]| {
        let args = set(extra);
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        run(root.path(), &args)
    };
    let output = run_set(&[]);
    assert!(!output.status.success());
    let err = String::from_utf8_lossy(&output.stderr);
    assert!(err.contains("--share-address"), "{err}");
    let output = run_set(&["--share-address", "192.168.1.50/24"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let show = ok(root.path(), &["show", &machine]);
    assert!(show.contains("mount 192.168.1.50:/src /mnt/src"), "{show}");

    // The private installation network hosts the stack beside Instigator.
    ok(
        root.path(),
        &[
            "network-set",
            &machine,
            "--mode",
            "private",
            "--endpoint",
            "tcp:127.0.0.1:49999",
        ],
    );
    let show = ok(root.path(), &["show", &machine]);
    assert!(show.contains("mount 10.98.0.4:/src /mnt/src"), "{show}");
}
