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
    let code = root.join("code");
    fs::create_dir(&code).unwrap();
    ok(root, &["share", &machine, "src", code.to_str().unwrap()]);
    machine
}

fn netdev(command: &str) -> &str {
    command
        .split_whitespace()
        .find(|word| word.starts_with("user,id=net0"))
        .unwrap_or_else(|| panic!("no user netdev in {command}"))
}

#[test]
fn a_machine_starts_on_the_default_subnet() {
    let root = tempfile::tempdir().unwrap();
    let machine = machine(root.path());
    let show = ok(root.path(), &["show", &machine]);
    assert!(show.contains("subnet: 10.98.0.0/24"), "{show}");
    let toml = fs::read_to_string(Path::new(&machine).join("machine.toml")).unwrap();
    assert!(!toml.contains("subnet"), "{toml}");
    let command = ok(root.path(), &["show-command", &machine]);
    assert!(
        command.contains("user,id=net0,net=10.98.0.0/24,host=10.98.0.2,dhcpstart=10.98.0.65"),
        "{command}"
    );
}

#[test]
fn the_user_network_follows_the_subnet() {
    let root = tempfile::tempdir().unwrap();
    let machine = machine(root.path());
    let out = ok(
        root.path(),
        &[
            "network-set",
            &machine,
            "--mode",
            "user",
            "--subnet",
            "10.42.0.0/24",
        ],
    );
    assert!(out.contains("subnet: 10.42.0.0/24"), "{out}");
    assert!(
        out.contains("rm /var/adm/proclaim.lease_info* /etc/resolv.conf")
            && out.contains("10.98.0.0/24"),
        "{out}"
    );
    let toml = fs::read_to_string(Path::new(&machine).join("machine.toml")).unwrap();
    assert!(toml.contains("subnet = \"10.42.0.0/24\""), "{toml}");

    let command = ok(root.path(), &["show-command", &machine]);
    let netdev = netdev(&command);
    assert!(
        netdev.starts_with("user,id=net0,net=10.42.0.0/24,host=10.42.0.2,dhcpstart=10.42.0.65"),
        "{netdev}"
    );
    let show = ok(root.path(), &["show", &machine]);
    assert!(show.contains("subnet: 10.42.0.0/24"), "{show}");
    assert!(show.contains("mount 10.42.0.4:/src /mnt/src"), "{show}");

    // Naming the same subnet again is no change, and leaving it out keeps it.
    let out = ok(
        root.path(),
        &[
            "network-set",
            &machine,
            "--mode",
            "user",
            "--subnet",
            "10.42.0.0/24",
        ],
    );
    assert!(!out.contains("proclaim"), "{out}");
    let out = ok(root.path(), &["network-set", &machine, "--mode", "user"]);
    assert!(
        out.contains("subnet: 10.42.0.0/24") && !out.contains("proclaim"),
        "{out}"
    );
}

#[test]
fn the_private_network_follows_the_subnet_and_keeps_it_across_modes() {
    let root = tempfile::tempdir().unwrap();
    let machine = machine(root.path());
    ok(
        root.path(),
        &[
            "network-set",
            &machine,
            "--mode",
            "private",
            "--subnet",
            "172.20.5.0/24",
        ],
    );
    let show = ok(root.path(), &["show", &machine]);
    assert!(show.contains("subnet: 172.20.5.0/24"), "{show}");
    // The hosted private network's host is where the share is mounted.
    assert!(show.contains("mount 172.20.5.2:/src /mnt/src"), "{show}");
    let command = ok(root.path(), &["show-command", &machine]);
    assert!(command.contains("stream,id=net0"), "{command}");

    // Back to user mode: the subnet stays.
    ok(root.path(), &["network-set", &machine, "--mode", "user"]);
    let command = ok(root.path(), &["show-command", &machine]);
    assert!(
        netdev(&command).starts_with("user,id=net0,net=172.20.5.0/24,host=172.20.5.2"),
        "{command}"
    );
}

#[test]
fn only_private_slash_24_subnets_are_accepted() {
    let root = tempfile::tempdir().unwrap();
    let machine = machine(root.path());
    for (subnet, why) in [
        ("8.8.8.0/24", "10.0.0.0/8, 172.16.0.0/12 or 192.168.0.0/16"),
        (
            "192.0.2.0/24",
            "10.0.0.0/8, 172.16.0.0/12 or 192.168.0.0/16",
        ),
        ("10.42.0.0/16", "must be a /24"),
        ("10.42.0.7/24", "must end in .0"),
        ("10.42.0.0", "must look like A.B.C.0/24"),
    ] {
        let err = fail(
            root.path(),
            &[
                "network-set",
                &machine,
                "--mode",
                "user",
                "--subnet",
                subnet,
            ],
        );
        assert!(err.contains(&format!("subnet '{subnet}'")), "{err}");
        assert!(err.contains(why), "{err}");
        assert!(err.contains("such as 10.98.0.0/24"), "{err}");
    }
    let err = fail(
        root.path(),
        &[
            "network-set",
            &machine,
            "--mode",
            "none",
            "--subnet",
            "10.42.0.0/24",
        ],
    );
    assert!(
        err.contains("--subnet requires --mode user or private"),
        "{err}"
    );
    // Nothing above changed the machine.
    let toml = fs::read_to_string(Path::new(&machine).join("machine.toml")).unwrap();
    assert!(!toml.contains("subnet"), "{toml}");

    // A hand-edited file is checked too.
    let path = Path::new(&machine).join("machine.toml");
    let edited = toml.replace("[network]", "[network]\nsubnet = \"8.8.8.0/24\"");
    assert_ne!(edited, toml);
    fs::write(&path, edited).unwrap();
    let err = fail(root.path(), &["validate", &machine]);
    assert!(err.contains("subnet '8.8.8.0/24'"), "{err}");
}
