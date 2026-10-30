//! `origami upgrade` on machine directories laid out as Origami 0.1.1 left
//! them: the machine.toml its create wrote, the PROM copy in firmware/, and
//! the state files its first run made. Also on format 2 machines, laid out
//! as Origami 0.2.1's create and first run left them.
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

/// No preset has this topology and population, so only the machine list says
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
        // macOS reports temporary directories through the /private symlink.
        stderr(&output).contains(&format!(
            "origami upgrade {}",
            fs::canonicalize(&machine).unwrap().display()
        )),
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
    assert_eq!(state("module0-nvram.raw").len(), 2048);
    assert_eq!(state("nvram0.raw.clock"), [0x3c; 16]);
    let text = fs::read_to_string(machine.join("machine.toml")).unwrap();
    assert!(text.starts_with("format = 4"), "{text}");
    assert!(!text.contains("firmware"), "{text}");
    assert!(
        text.contains("[[pipe]]\nslot = \"pci-5\"\nfamily = \"psitech\"\nproduct = \"rad4\""),
        "{text}"
    );
    let output = origami(root.path(), &["show", path]);
    assert!(output.status.success(), "{}", stderr(&output));
    let shown = String::from_utf8_lossy(&output.stdout);
    assert!(shown.contains("forward ssh: tcp 127.0.0.1:2222 -> guest:22"));
    assert!(shown.contains("system: disk at scsi.0:1 (drives/system.qcow2)"));

    let before = snapshot(&machine);
    let output = origami(root.path(), &["upgrade", path]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(String::from_utf8_lossy(&output.stdout).contains("needs no upgrade"));
    assert_eq!(snapshot(&machine), before);
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
fn a_failed_upgrade_changes_nothing_and_a_later_one_keeps_every_old_file() {
    let root = tempfile::tempdir().unwrap();
    let io_prom = root.path().join("io6prom.img");
    fs::write(&io_prom, b"io6").unwrap();
    let text = ORIGIN2000.replace(
        "[network]\nmode = \"user\"\n",
        "[network]\nmode = \"private\"\nendpoint = \"tcp:127.0.0.1:4242\"\nmac = \"08:00:69:12:34:56\"\n",
    );
    let mut state = ip27_state(4);
    // A clock of another size is not this machine's, so a fresh one replaces it.
    state[1].1 = vec![7; 8];
    let machine = format1(root.path(), "origin2000", &text, &borrowed(&state));
    let path = machine.to_str().unwrap();
    let upgrade = || {
        origami(
            root.path(),
            &["upgrade", path, "--io-prom", io_prom.to_str().unwrap()],
        )
    };
    let before = snapshot(&machine);
    let tool = common::init_tool(root.path());
    let away = tool.with_extension("away");
    fs::rename(&tool, &away).unwrap();
    assert!(!upgrade().status.success());
    assert_eq!(snapshot(&machine), before);
    fs::rename(&away, &tool).unwrap();

    let output = upgrade();
    assert!(output.status.success(), "{}", stderr(&output));
    for (file, bytes) in &before {
        if !file.ends_with("machine.toml") {
            assert_eq!(&fs::read(file).unwrap(), bytes, "{}", file.display());
        }
    }
    let state = |name: &str| fs::read(machine.join("state").join(name)).unwrap();
    assert_eq!(state("nvram0-clock.raw"), [0; 16]);
    assert_eq!(state("io0-flash.raw"), [0; 1048576]);
    let output = origami(root.path(), &["show", path]);
    assert!(output.status.success(), "{}", stderr(&output));
    let shown = String::from_utf8_lossy(&output.stdout);
    assert!(shown.contains("network: private"), "{shown}");
    assert!(shown.contains("identity MAC: 08:00:69:12:34:56"), "{shown}");
}

#[test]
fn a_machine_without_a_topology_takes_the_one_it_ran_on() {
    let root = tempfile::tempdir().unwrap();
    let io_prom = root.path().join("io6prom.img");
    fs::write(&io_prom, b"io6").unwrap();
    let text = ORIGIN2000.replace("topology = \"origin2000-rack\"\n", "");
    let text = text.replace("population = [\n    2,\n    2,\n    2,\n    2,\n]\n", "");
    assert!(!text.contains("topology") && !text.contains("population"));
    let machine = format1(root.path(), "origin2000", &text, &borrowed(&ip27_state(4)));
    let path = machine.to_str().unwrap();
    let output = origami(
        root.path(),
        &["upgrade", path, "--io-prom", io_prom.to_str().unwrap()],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    let text = fs::read_to_string(machine.join("machine.toml")).unwrap();
    assert!(text.contains("topology = \"origin2000-rack\""), "{text}");
    assert_eq!(
        fs::read(machine.join("state/node3-flash.raw")).unwrap(),
        [4; 1048576]
    );
    let output = origami(root.path(), &["validate", path]);
    assert!(output.status.success(), "{}", stderr(&output));
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
    // Cache directories that cannot exist stop the download before it
    // reaches the network.
    fs::write(root.path().join("cache"), b"").unwrap();
    fs::write(root.path().join("home"), b"").unwrap();
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
fn nvram_already_under_its_new_name_stays_in_place() {
    use std::os::unix::fs::MetadataExt;
    let root = tempfile::tempdir().unwrap();
    let machine = format1(
        root.path(),
        "origin200",
        ORIGIN200,
        &borrowed(&ip27_state(0)),
    );
    fs::write(machine.join("drives/system.qcow2"), b"disk").unwrap();
    let nvram = machine.join("state/nvram0.raw");
    let inode = fs::metadata(&nvram).unwrap().ino();
    let output = origami(root.path(), &["upgrade", machine.to_str().unwrap()]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(fs::metadata(&nvram).unwrap().ino(), inode);
    assert_eq!(fs::read(&nvram).unwrap(), [0x5a; 32768]);

    // One of another size is not replaced with a fresh one.
    let machine = format1(
        root.path(),
        "short",
        ORIGIN200,
        &[("nvram0.raw", vec![7; 100])],
    );
    fs::write(machine.join("drives/system.qcow2"), b"disk").unwrap();
    let before = snapshot(&machine);
    let output = origami(root.path(), &["upgrade", machine.to_str().unwrap()]);
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("nvram0.raw is not 32768 bytes"),
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

const ORIGIN200_FORMAT2: &str = r#"format = 2

[machine]
topology = "origin200"
population = [1]
model = "origin200"
nodes = 1
cpus_per_node = 1
memory_per_node = "64MiB"
graphics = "rad4"

[identity]
mac = "08:00:69:12:34:56"

[network]
mode = "private"
endpoint = "tcp:127.0.0.1:4242"

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

/// A format 2 machine named `name` under `root`, with the state files
/// 0.2.1's create made and a guest disk.
fn format2(root: &Path, name: &str, machine_toml: &str, state: &[(&str, Vec<u8>)]) -> PathBuf {
    let dir = root.join(name);
    for sub in ["drives", "state", "logs"] {
        fs::create_dir_all(dir.join(sub)).unwrap();
    }
    fs::write(dir.join("machine.toml"), machine_toml).unwrap();
    fs::write(dir.join("drives/system.qcow2"), b"disk").unwrap();
    fs::write(dir.join("state/machine.lock"), b"").unwrap();
    for (path, bytes) in state {
        fs::write(dir.join("state").join(path), bytes).unwrap();
    }
    dir
}

fn origin200_format2_state() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("node0-flash.raw", vec![0x27; 1048576]),
        ("nvram0.raw", vec![0x5a; 32768]),
        ("nvram0-clock.raw", vec![0x3c; 16]),
    ]
}

/// Cache directories that cannot exist stop a PROM download before it
/// reaches the network.
fn no_downloads(root: &Path) {
    fs::write(root.join("cache"), b"").unwrap();
    fs::write(root.join("home"), b"").unwrap();
}

#[test]
fn format2_origin200_gains_module_nvram_and_keeps_everything_else() {
    let root = tempfile::tempdir().unwrap();
    let machine = format2(
        root.path(),
        "origin200",
        ORIGIN200_FORMAT2,
        &origin200_format2_state(),
    );
    let prom = root.path().join("ip27prom.img");
    fs::write(&prom, b"ip27").unwrap();
    let path = machine.to_str().unwrap();

    let before = snapshot(&machine);
    let output = origami(root.path(), &["validate", path]);
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains(&format!(
            "is from Origami 0.2; upgrade it once with origami upgrade {}",
            fs::canonicalize(&machine).unwrap().display()
        )),
        "{}",
        stderr(&output)
    );
    assert_eq!(snapshot(&machine), before);

    let output = origami(
        root.path(),
        &["upgrade", path, "--prom", prom.to_str().unwrap()],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    let arguments =
        fs::read_to_string(root.path().join("runtime/qemu-sgi-machine-init.args")).unwrap();
    assert!(
        arguments.contains(&format!("--boot-prom\n{}\n", prom.display())),
        "{arguments}"
    );
    let after = snapshot(&machine);
    for (file, bytes) in &before {
        if !file.ends_with("machine.toml") {
            assert_eq!(after.get(file), Some(bytes), "{}", file.display());
        }
    }
    let added: Vec<_> = after.keys().filter(|f| !before.contains_key(*f)).collect();
    assert_eq!(added, [&machine.join("state/module0-nvram.raw")]);
    assert_eq!(after[&machine.join("state/module0-nvram.raw")].len(), 2048);
    let text = fs::read_to_string(machine.join("machine.toml")).unwrap();
    assert_eq!(
        text.parse::<toml::Table>().unwrap(),
        (ORIGIN200_FORMAT2
            .replace("format = 2", "format = 4")
            .replace("graphics = \"rad4\"\n", "")
            + "\n[[pipe]]\nslot = \"pci-5\"\nfamily = \"psitech\"\nproduct = \"rad4\"\n")
            .parse::<toml::Table>()
            .unwrap()
    );
    let output = origami(root.path(), &["show", path]);
    assert!(output.status.success(), "{}", stderr(&output));
    let shown = String::from_utf8_lossy(&output.stdout);
    assert!(shown.contains("forward ssh: tcp 127.0.0.1:2222 -> guest:22"));
    assert!(shown.contains("identity MAC: 08:00:69:12:34:56"), "{shown}");

    // A second run needs no PROM and changes nothing.
    no_downloads(root.path());
    let output = origami(root.path(), &["upgrade", path]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(String::from_utf8_lossy(&output.stdout).contains("needs no upgrade"));
    assert_eq!(snapshot(&machine), after);
}

#[test]
fn an_interrupted_format2_upgrade_finishes_without_a_prom() {
    let root = tempfile::tempdir().unwrap();
    let mut state = origin200_format2_state();
    // The earlier run moved its file in, then stopped before machine.toml.
    state.push(("module0-nvram.raw", vec![0x11; 2048]));
    let machine = format2(root.path(), "origin200", ORIGIN200_FORMAT2, &state);
    fs::create_dir_all(machine.join("state.format3")).unwrap();
    fs::write(machine.join("state.format3/module0-nvram.raw"), b"partial").unwrap();
    no_downloads(root.path());
    let path = machine.to_str().unwrap();
    let output = origami(root.path(), &["upgrade", path]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(!machine.join("state.format3").exists());
    assert_eq!(
        fs::read(machine.join("state/module0-nvram.raw")).unwrap(),
        [0x11; 2048]
    );
    let output = origami(root.path(), &["validate", path]);
    assert!(output.status.success(), "{}", stderr(&output));
}

#[test]
fn a_format2_upgrade_without_a_prom_names_the_option_and_changes_nothing() {
    let root = tempfile::tempdir().unwrap();
    let machine = format2(
        root.path(),
        "origin200",
        ORIGIN200_FORMAT2,
        &origin200_format2_state(),
    );
    no_downloads(root.path());
    let before = snapshot(&machine);
    let output = origami(root.path(), &["upgrade", machine.to_str().unwrap()]);
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("--prom FILE"),
        "{}",
        stderr(&output)
    );
    assert!(
        stderr(&output).contains("its machine.toml is unchanged"),
        "{}",
        stderr(&output)
    );
    assert_eq!(snapshot(&machine), before);
}

#[test]
fn a_format2_machine_missing_guest_state_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let mut state = origin200_format2_state();
    state.retain(|(name, _)| *name != "nvram0.raw");
    let machine = format2(root.path(), "origin200", ORIGIN200_FORMAT2, &state);
    let prom = root.path().join("ip27prom.img");
    fs::write(&prom, b"ip27").unwrap();
    let before = snapshot(&machine);
    let output = origami(
        root.path(),
        &[
            "upgrade",
            machine.to_str().unwrap(),
            "--prom",
            prom.to_str().unwrap(),
        ],
    );
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("nvram0.raw. Restore it from a backup"),
        "{}",
        stderr(&output)
    );
    assert_eq!(snapshot(&machine), before);
}

#[test]
fn format2_origin2000_reads_both_proms_and_a_fuel_needs_none() {
    let root = tempfile::tempdir().unwrap();
    let prom = root.path().join("ip27prom.img");
    let io_prom = root.path().join("io6prom.img");
    fs::write(&prom, b"ip27").unwrap();
    fs::write(&io_prom, b"io6").unwrap();
    let text = ORIGIN2000
        .replace("format = 1", "format = 2")
        .replace("[firmware]\nimage = \"firmware/prom.bin\"\n\n", "");
    let mut state = vec![
        ("io0-flash.raw", vec![6; 1048576]),
        ("nvram0.raw", vec![0x5a; 32768]),
        ("nvram0-clock.raw", vec![0x3c; 16]),
    ];
    for node in [
        "node0-flash.raw",
        "node1-flash.raw",
        "node2-flash.raw",
        "node3-flash.raw",
    ] {
        state.push((node, vec![1; 1048576]));
    }
    let machine = format2(root.path(), "origin2000", &text, &state);
    let path = machine.to_str().unwrap();
    let output = origami(
        root.path(),
        &[
            "upgrade",
            path,
            "--prom",
            prom.to_str().unwrap(),
            "--io-prom",
            io_prom.to_str().unwrap(),
        ],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    let arguments =
        fs::read_to_string(root.path().join("runtime/qemu-sgi-machine-init.args")).unwrap();
    assert!(arguments.contains(&format!("--io-prom\n{}\n", io_prom.display())));
    assert_eq!(
        fs::read(machine.join("state/io0-flash.raw")).unwrap(),
        [6; 1048576]
    );
    assert_eq!(
        fs::read(machine.join("state/module0-nvram.raw"))
            .unwrap()
            .len(),
        2048
    );
    let output = origami(root.path(), &["validate", path]);
    assert!(output.status.success(), "{}", stderr(&output));

    // A Fuel has no module controller NVRAM, so only its format changes.
    let fuel = format2(
        root.path(),
        "fuel",
        "format = 2\n\n[machine]\ntopology = \"fuel\"\npopulation = [1]\nmodel = \"fuel\"\n\
         nodes = 1\ncpus_per_node = 1\nmemory_per_node = \"512MiB\"\ngraphics = \"none\"\n",
        &[
            ("node0-flash.raw", vec![1; 16777216]),
            ("nvram0.raw", vec![2; 32768]),
            ("nvram0-clock.raw", vec![3; 16]),
        ],
    );
    no_downloads(root.path());
    let before = snapshot(&fuel);
    let output = origami(root.path(), &["upgrade", fuel.to_str().unwrap()]);
    assert!(output.status.success(), "{}", stderr(&output));
    // Its old topology left out the VPro board every Fuel has.
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("gains the VPro V12"),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let after = snapshot(&fuel);
    assert_eq!(after.len(), before.len());
    let text = fs::read_to_string(fuel.join("machine.toml")).unwrap();
    assert!(text.starts_with("format = 4"), "{text}");
    assert!(text.contains("slot = \"widget-13\""), "{text}");
}

/// A format 3 machine as Origami 0.3.1's create left it, with storage.
fn format3(root: &Path, name: &str, machine: &str) -> PathBuf {
    let dir = root.join(name);
    for sub in ["drives", "state", "logs"] {
        fs::create_dir_all(dir.join(sub)).unwrap();
    }
    fs::write(dir.join("machine.toml"), format!("format = 3\n\n{machine}")).unwrap();
    fs::write(dir.join("state/machine.lock"), b"").unwrap();
    dir
}

#[test]
fn format3_graphics_become_pipes_or_the_upgrade_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let rack = |graphics: &str| {
        format!(
            "[machine]\ntopology = \"origin2000-rack\"\npopulation = [2, 2, 2, 2]\n\
             model = \"origin2000\"\nnodes = 4\ncpus_per_node = 2\n\
             memory_per_node = \"64MiB\"\ngraphics = \"{graphics}\"\n"
        )
    };
    let machine = format3(root.path(), "impact", &rack("esi"));
    let output = origami(root.path(), &["upgrade", machine.to_str().unwrap()]);
    assert!(output.status.success(), "{}", stderr(&output));
    let text = fs::read_to_string(machine.join("machine.toml")).unwrap();
    assert!(text.starts_with("format = 4"), "{text}");
    assert!(
        text.contains("[[pipe]]\nslot = \"io3\"\nfamily = \"mgras\"\nproduct = \"esi\""),
        "{text}"
    );
    assert!(!text.contains("graphics"), "{text}");

    let machine = format3(root.path(), "kona", &rack("infinite-reality"));
    let before = snapshot(&machine);
    let output = origami(root.path(), &["upgrade", machine.to_str().unwrap()]);
    assert!(!output.status.success());
    assert_eq!(
        stderr(&output).trim(),
        format!(
            "origami: cannot upgrade the format 3 machine {}: topology 'origin2000-rack' does \
             not take an InfiniteReality pipe in io3: an Origin 2000 with an InfiniteReality \
             pipe is an Onyx2, so use onyx2-rack; its machine.toml is unchanged",
            fs::canonicalize(&machine).unwrap().display()
        )
    );
    assert_eq!(snapshot(&machine), before);
}

/// A current machine as Origami 0.5.0's create left an Onyx2 rack: its
/// state has no InfiniteReality pipe flash.
const ONYX2_RACK_CURRENT: &str = r#"format = 4

[machine]
topology = "onyx2-rack"
population = [2, 2]
model = "onyx2"
nodes = 2
cpus_per_node = 2
memory_per_node = "64MiB"

[[pipe]]
slot = "io3"
family = "kona"
product = "ir3"

[pipe.settings]
display = "dg5-2"
geometry = "ge16-4"
rm-count = "1"
rm-type = "rm9"
texture-mb = "256"

[network]
mode = "user"
"#;

#[test]
fn a_current_machine_gains_the_pipe_flash_and_keeps_its_state() {
    let root = tempfile::tempdir().unwrap();
    let io_prom = root.path().join("io6prom.img");
    fs::write(&io_prom, b"io6").unwrap();
    let prom = root.path().join("ip27prom.img");
    fs::write(&prom, b"ip27").unwrap();
    let dir = root.path().join("onyx2");
    for sub in ["drives", "state", "logs"] {
        fs::create_dir_all(dir.join(sub)).unwrap();
    }
    fs::write(dir.join("machine.toml"), ONYX2_RACK_CURRENT).unwrap();
    // What the guest has saved since it was created.
    let saved = [
        ("node0-flash", 1048576),
        ("node1-flash", 1048576),
        ("io0-flash", 1048576),
        ("nvram0", 32768),
        ("nvram0-clock", 16),
        ("module0-nvram", 2048),
    ];
    for (index, (name, size)) in saved.iter().enumerate() {
        fs::write(
            dir.join(format!("state/{name}.raw")),
            vec![index as u8 + 1; *size],
        )
        .unwrap();
    }
    let path = dir.to_str().unwrap();
    let before = snapshot(&dir);

    // QEMU does not reopen the state without the store, and says how to add it.
    let output = origami(root.path(), &["validate", path]);
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("io3-kona-flash.raw. Add it with origami upgrade"),
        "{}",
        stderr(&output)
    );

    let output = origami(
        root.path(),
        &[
            "upgrade",
            path,
            "--prom",
            prom.to_str().unwrap(),
            "--io-prom",
            io_prom.to_str().unwrap(),
        ],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("io3-kona-flash.raw"), "{stdout}");
    assert!(stdout.contains("io7-kona-flash.raw"), "{stdout}");
    // The tool is asked for the machine's own selection.
    let arguments =
        fs::read_to_string(root.path().join("runtime/qemu-sgi-machine-init.args")).unwrap();
    assert!(
        arguments.contains("onyx2,topology=onyx2-rack,nodes=2\n"),
        "{arguments}"
    );
    let after = snapshot(&dir);
    for slot in ["io3", "io7"] {
        let flash = &after[&dir.join(format!("state/{slot}-kona-flash.raw"))];
        assert_eq!(flash.len(), 524288);
    }
    // Everything else is exactly as the guest left it.
    for (path, bytes) in &before {
        assert_eq!(&after[path], bytes, "{}", path.display());
    }
    // Beside the two stores, only the lock the upgrade takes is new.
    let added: Vec<_> = after
        .keys()
        .filter(|path| !before.contains_key(*path))
        .map(|path| path.file_name().unwrap().to_str().unwrap().to_string())
        .collect();
    assert_eq!(
        added,
        ["io3-kona-flash.raw", "io7-kona-flash.raw", "machine.lock"]
    );
    assert!(!dir.join("state.pipe-flash").exists());
    let output = origami(root.path(), &["validate", path]);
    assert!(output.status.success(), "{}", stderr(&output));

    // A second upgrade finds nothing to add and leaves the flash alone.
    fs::write(dir.join("state/io3-kona-flash.raw"), vec![0xaa; 524288]).unwrap();
    let before = snapshot(&dir);
    let output = origami(root.path(), &["upgrade", path]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("is current"),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert_eq!(snapshot(&dir), before);
}

#[test]
fn a_current_machine_without_a_pipe_has_nothing_to_add() {
    let root = tempfile::tempdir().unwrap();
    let dir = format3(
        root.path(),
        "rack",
        "[machine]\ntopology = \"origin2000-rack\"\npopulation = [2, 2, 2, 2]\n\
         model = \"origin2000\"\nnodes = 4\ncpus_per_node = 2\n\
         memory_per_node = \"64MiB\"\ngraphics = \"none\"\n",
    );
    let output = origami(root.path(), &["upgrade", dir.to_str().unwrap()]);
    assert!(output.status.success(), "{}", stderr(&output));
    let before = snapshot(&dir);
    let output = origami(root.path(), &["upgrade", dir.to_str().unwrap()]);
    assert!(String::from_utf8_lossy(&output.stdout).contains("is current"));
    assert_eq!(snapshot(&dir), before);
}

#[test]
fn a_format3_machine_gains_the_pipe_flash_with_its_upgrade() {
    let root = tempfile::tempdir().unwrap();
    let io_prom = root.path().join("io6prom.img");
    fs::write(&io_prom, b"io6").unwrap();
    let prom = root.path().join("ip27prom.img");
    fs::write(&prom, b"ip27").unwrap();
    let dir = format3(
        root.path(),
        "onyx2",
        "[machine]\ntopology = \"onyx2-rack\"\npopulation = [2, 2]\nmodel = \"onyx2\"\n\
         nodes = 2\ncpus_per_node = 2\nmemory_per_node = \"64MiB\"\n\
         graphics = \"infinite-reality\"\n",
    );
    let output = origami(
        root.path(),
        &[
            "upgrade",
            dir.to_str().unwrap(),
            "--prom",
            prom.to_str().unwrap(),
            "--io-prom",
            io_prom.to_str().unwrap(),
        ],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    for slot in ["io3", "io7"] {
        let flash = dir.join(format!("state/{slot}-kona-flash.raw"));
        assert_eq!(fs::metadata(flash).unwrap().len(), 524288);
    }
    let text = fs::read_to_string(dir.join("machine.toml")).unwrap();
    assert!(text.starts_with("format = 4"), "{text}");
}
