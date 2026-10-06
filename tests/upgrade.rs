//! `origami upgrade` on machine directories laid out as Origami 0.1.1 left
//! them: the machine.toml its create wrote, the PROM copy in firmware/, and
//! the state files its first run made.
#![cfg(unix)]

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;

fn origami(root: &Path, args: &[&str]) -> Output {
    common::origami(Path::new(env!("CARGO_BIN_EXE_origami")), root)
        .args(args)
        .output()
        .unwrap()
}

/// Every file under `dir` and its contents.
fn snapshot(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut files = BTreeMap::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(path) = pending.pop() {
        for entry in fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else {
                files.insert(path.clone(), fs::read(&path).unwrap());
            }
        }
    }
    files
}

/// A format 1 machine named `name` under `root` with `machine.toml` and
/// state files, as 0.1.1's create and first run left it.
fn format1(root: &Path, name: &str, machine_toml: &str, state: &[(&str, Vec<u8>)]) -> PathBuf {
    let dir = root.join(name);
    for sub in ["firmware", "drives", "state", "logs"] {
        fs::create_dir_all(dir.join(sub)).unwrap();
    }
    fs::write(dir.join("machine.toml"), machine_toml).unwrap();
    fs::write(dir.join("firmware/prom.bin"), vec![0x27; 524288]).unwrap();
    fs::write(dir.join("state/machine.lock"), b"").unwrap();
    for (path, bytes) in state {
        let path = dir.join("state").join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }
    dir
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

const ORIGIN200: &str = r#"format = 1

[machine]
topology = "origin200"
population = [1]
model = "origin200"
nodes = 1
cpus_per_node = 1
memory_per_node = "64MiB"
graphics = "rad4"

[firmware]
image = "firmware/prom.bin"

[network]
mode = "user"

[[network.forward]]
name = "ssh"
protocol = "tcp"
host_port = 2222
guest_port = 22

[[drive]]
name = "system"
type = "disk"
bus = 0
target = 1
image = "drives/system.qcow2"
read_only = false
"#;

const ORIGIN2000: &str = r#"format = 1

[machine]
topology = "origin2000-rack"
population = [
    2,
    2,
    2,
    2,
]
model = "origin2000"
nodes = 4
cpus_per_node = 2
memory_per_node = "64MiB"
graphics = "none"

[firmware]
image = "firmware/prom.bin"

[network]
mode = "user"
"#;

/// No preset has this topology and population, so only the catalogue says
/// which IO PROM it reads.
const ONYX2_RACK: &str = r#"format = 1

[machine]
topology = "onyx2-rack"
population = [
    2,
    2,
]
model = "onyx2"
nodes = 2
cpus_per_node = 2
memory_per_node = "64MiB"
graphics = "infinite-reality"

[firmware]
image = "firmware/prom.bin"

[network]
mode = "user"
"#;

fn ip27_state(nodes: u8) -> Vec<(String, Vec<u8>)> {
    let mut state = vec![
        ("nvram0.raw".to_string(), vec![0x5a; 32768]),
        ("nvram0.raw.clock".to_string(), vec![0x3c; 16]),
    ];
    for node in 1..=nodes {
        state.push((format!("node-proms/node{node}.bin"), vec![node; 1048576]));
    }
    state
}

fn borrowed(state: &[(String, Vec<u8>)]) -> Vec<(&str, Vec<u8>)> {
    state
        .iter()
        .map(|(path, bytes)| (path.as_str(), bytes.clone()))
        .collect()
}

#[test]
fn origin200_upgrades_once_and_keeps_its_nvram_drives_and_forwards() {
    let root = tempfile::tempdir().unwrap();
    let machine = format1(
        root.path(),
        "origin200",
        ORIGIN200,
        &borrowed(&ip27_state(0)),
    );
    fs::write(machine.join("drives/system.qcow2"), b"disk").unwrap();
    let path = machine.to_str().unwrap();

    // Other commands send the machine to upgrade and leave it alone.
    let before = snapshot(&machine);
    let output = origami(root.path(), &["validate", path]);
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains(&format!("origami upgrade {path}")),
        "{}",
        stderr(&output)
    );
    assert_eq!(snapshot(&machine), before);

    let output = origami(root.path(), &["upgrade", path]);
    assert!(output.status.success(), "{}", stderr(&output));
    let state = |name: &str| fs::read(machine.join("state").join(name)).unwrap();
    assert_eq!(state("nvram0.raw"), [0x5a; 32768]);
    assert_eq!(state("nvram0-clock.raw"), [0x3c; 16]);
    assert_eq!(state("node0-flash.raw").len(), 1048576);
    assert_eq!(state("nvram0.raw.clock"), [0x3c; 16]);
    let text = fs::read_to_string(machine.join("machine.toml")).unwrap();
    assert!(text.starts_with("format = 2"), "{text}");
    assert!(!text.contains("firmware"), "{text}");
    let output = origami(root.path(), &["show", path]);
    assert!(output.status.success(), "{}", stderr(&output));
    let shown = String::from_utf8_lossy(&output.stdout);
    assert!(shown.contains("forward ssh: tcp 127.0.0.1:2222 -> guest:22"));
    assert!(shown.contains("system: disk at scsi.0:1 (drives/system.qcow2)"));

    let output = origami(root.path(), &["upgrade", path]);
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("needs no upgrade"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn origin2000_and_onyx2_read_the_io_prom_given() {
    let root = tempfile::tempdir().unwrap();
    let io_prom = root.path().join("io6prom.img");
    fs::write(&io_prom, b"io6").unwrap();
    for (name, text, nodes) in [("origin2000", ORIGIN2000, 4), ("onyx2", ONYX2_RACK, 2)] {
        let machine = format1(root.path(), name, text, &borrowed(&ip27_state(nodes)));
        let path = machine.to_str().unwrap();
        let output = origami(
            root.path(),
            &["upgrade", path, "--io-prom", io_prom.to_str().unwrap()],
        );
        assert!(output.status.success(), "{name}: {}", stderr(&output));
        let arguments =
            fs::read_to_string(root.path().join("runtime/qemu-sgi-machine-init.args")).unwrap();
        assert!(
            arguments.contains(&format!("--io-prom\n{}\n", io_prom.display())),
            "{arguments}"
        );
        for node in 0..nodes {
            let flash = fs::read(machine.join(format!("state/node{node}-flash.raw"))).unwrap();
            assert_eq!(flash, [node + 1; 1048576], "{name} node {node}");
        }
        assert_eq!(
            fs::read(machine.join("state/nvram0.raw")).unwrap(),
            [0x5a; 32768]
        );
        let output = origami(root.path(), &["validate", path]);
        assert!(output.status.success(), "{name}: {}", stderr(&output));
    }
}

#[test]
fn io_prom_without_a_file_or_cached_copy_names_the_option() {
    let root = tempfile::tempdir().unwrap();
    let machine = format1(
        root.path(),
        "origin2000",
        ORIGIN2000,
        &borrowed(&ip27_state(4)),
    );
    // A cache directory that cannot exist stops the download before it
    // reaches the network.
    fs::write(root.path().join("cache"), b"").unwrap();
    let before = snapshot(&machine);
    let output = origami(root.path(), &["upgrade", machine.to_str().unwrap()]);
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("--io-prom FILE"),
        "{}",
        stderr(&output)
    );
    assert_eq!(snapshot(&machine), before);
}

#[test]
fn upgrade_refuses_a_machine_that_holds_its_lock() {
    let root = tempfile::tempdir().unwrap();
    let machine = format1(
        root.path(),
        "origin200",
        ORIGIN200,
        &borrowed(&ip27_state(0)),
    );
    fs::write(machine.join("drives/system.qcow2"), b"disk").unwrap();
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(machine.join("state/machine.lock"))
        .unwrap();
    lock.try_lock().unwrap();
    let before = snapshot(&machine);
    let output = origami(root.path(), &["upgrade", machine.to_str().unwrap()]);
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("stop the machine"),
        "{}",
        stderr(&output)
    );
    assert_eq!(snapshot(&machine), before);
}
