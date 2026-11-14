//! Contract tests: the product's own commands against the real QEMU runtime
//! the product is pinned to, with no guest running. Every machine starts
//! paused (`-S`), so QEMU parses and accepts each argument the product
//! builds, and the machine list the fake serves is checked against the real
//! one. A change in QEMU that breaks the product shows here, not in a guest.
//!
//! They need a runtime directory: `ORIGAMI_RUNTIME_DIR` names the one
//! holding `qemu-system-mips64`, `qemu-img` and `qemu-sgi-machine-init`.
//! A plain `cargo test` lists these as ignored.
//!
//!     ORIGAMI_RUNTIME_DIR=DIR cargo test --test runtime -- --include-ignored

#[path = "../support/mod.rs"]
mod support;

use qapi::qmp;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::Duration;

/// The runtime directory the tests were pointed at.
fn runtime() -> PathBuf {
    let dir = std::env::var_os("ORIGAMI_RUNTIME_DIR").unwrap_or_else(|| {
        panic!("ORIGAMI_RUNTIME_DIR names no QEMU runtime; set it to run these tests")
    });
    let dir = PathBuf::from(dir);
    assert!(
        dir.join(if cfg!(windows) {
            "qemu-system-mips64.exe"
        } else {
            "qemu-system-mips64"
        })
        .is_file(),
        "{} holds no qemu-system-mips64",
        dir.display()
    );
    dir
}

/// The real machine list.
fn machines() -> origami::Machines {
    runtime();
    let runtime = origami::runtime::Runtime::locate().expect("the origami test executable");
    origami::machines(&runtime).unwrap()
}

fn origami(root: &Path, args: &[&OsStr]) -> Output {
    Command::new(support::ORIGAMI)
        .args(args)
        .env("ORIGAMI_RUNTIME_DIR", runtime())
        .env("ORIGAMI_CACHE_DIR", root.join("cache"))
        .env_remove("ORIGAMI_FAKE")
        .output()
        .unwrap()
}

/// `origami command machine`, or why it failed.
fn run(root: &Path, command: &str, machine: &Path, extra: &[&str]) -> Result<Output, String> {
    let mut args = vec![command.as_ref(), machine.as_os_str()];
    args.extend(extra.iter().map(OsStr::new));
    let output = origami(root, &args);
    if output.status.success() {
        Ok(output)
    } else {
        Err(format!(
            "{command}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

/// Start `machine` paused, ask QEMU over QMP what it is doing, and stop it.
/// QEMU must accept every argument the product builds for the machine,
/// open its storage and answer on QMP. A machine that has not been told to
/// run reports `prelaunch`; `paused` is what it reports after `stop`.
fn start_and_stop(root: &Path, machine: &Path) -> Result<(), String> {
    let start = ["--background", "--display", "none", "--qemu-arg=-S"];
    run(root, "run", machine, &start).map_err(|error| {
        let log = fs::read_to_string(machine.join("logs/runner.log")).unwrap_or_default();
        format!("{error}\n{log}")
    })?;
    let _guard = support::Running::new(root, machine);
    let record = origami::control::read(machine).map_err(|error| error.to_string())?;
    let mut qmp = origami::control::verified_qmp(&record).map_err(|error| error.to_string())?;
    let status = qmp
        .execute(&qmp::query_status {})
        .map_err(|error| error.to_string())?;
    // QEMU serves one QMP client at a time, and `stop` is another.
    drop(qmp);
    if status.status != qmp::RunState::prelaunch {
        return Err(format!("QEMU did not wait for `cont`: {:?}", status.status));
    }
    run(root, "stop", machine, &[])?;
    let stopped = |machine: &Path| {
        run(root, "status", machine, &[])
            .map(|output| String::from_utf8_lossy(&output.stdout).trim() == "stopped")
    };
    for _ in 0..200 {
        if stopped(machine)? {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err("QEMU did not stop".into())
}

/// A synthetic image of a size the init tool accepts for `kind`: a MIPS
/// branch-to-self and its delay slot, zero-padded. It is not SGI firmware.
fn synthetic_image(path: &Path, kind: &str) -> Vec<u8> {
    let size = match kind {
        // An IP35 image must decode to exactly the PROM size QEMU loads.
        "ip35-prom" => 1_476_264,
        _ => 64 * 1024,
    };
    let mut image = vec![0x10, 0x00, 0xff, 0xff, 0, 0, 0, 0];
    image.resize(size, 0);
    fs::write(path, &image).unwrap();
    image
}

/// A machine from `preset` with synthetic PROMs; its directory.
fn create(
    root: &Path,
    name: &str,
    preset: &str,
    offering: &origami::Offering,
    extra: &[&str],
) -> Result<PathBuf, String> {
    let machine = root.join(name);
    let mut args: Vec<OsString> = vec!["--preset".into(), preset.into()];
    for input in &offering.init_inputs {
        let image = root.join(format!("{name}-{}.bin", input.name));
        synthetic_image(&image, &input.kind);
        let option = if input.name == "boot-prom" {
            "--prom"
        } else {
            "--io-prom"
        };
        args.extend([option.into(), image.into()]);
    }
    args.extend(extra.iter().map(OsString::from));
    let args: Vec<_> = args.iter().map(|arg| arg.to_str().unwrap()).collect();
    run(root, "create", &machine, &args)?;
    Ok(machine)
}

/// Create `name` from its preset, check its storage and arguments, and start
/// it paused; what went wrong, if anything.
fn check_preset(root: &Path, name: &str, offering: &origami::Offering) -> Vec<String> {
    let mut failures = Vec::new();
    let machine = match create(root, &format!("{name}, with comma"), name, offering, &[]) {
        Ok(machine) => machine,
        Err(error) => {
            failures.push(format!("{name}: {error}"));
            return failures;
        }
    };
    for item in &offering.storage {
        let path = machine.join(format!("state/{}.raw", item.name));
        match fs::metadata(&path) {
            Ok(metadata) if metadata.len() == item.size => (),
            other => failures.push(format!("{name}: {}: {other:?}", path.display())),
        }
    }
    // The machine has the system disk its machine list offers, so QEMU
    // is also handed that drive.
    match run(root, "drive-create", &machine, &["16"]) {
        Ok(_) => {
            let image = machine.join("drives/system.qcow2");
            let head = fs::read(&image).unwrap_or_default();
            if head.get(..4) != Some(b"QFI\xfb") {
                failures.push(format!("{name}: qemu-img made no qcow2 image"));
            }
        }
        Err(error) if error.contains("no system disk target") => {}
        Err(error) => failures.push(format!("{name}: {error}")),
    }
    let result = run(root, "validate", &machine, &[])
        .and_then(|_| run(root, "show-command", &machine, &[]))
        .and_then(|shown| {
            // Only an Origin 3000, whose I-brick has no PS/2, gets USB
            // input; Fuel and Tezro keep theirs.
            let shown = String::from_utf8_lossy(&shown.stdout).into_owned();
            let usb =
                shown.contains(" -device usb-kbd,bus=usb-bus.0 -device usb-mouse,bus=usb-bus.0");
            if usb == (offering.product == "origin3000") && !shown.contains("usb-tablet") {
                Ok(())
            } else {
                Err(format!("show-command USB input is {usb}: {shown}"))
            }
        })
        .and_then(|_| start_and_stop(root, &machine));
    if let Err(error) = result {
        failures.push(format!("{name}: {error}"));
    }
    failures
}

#[test]
#[ignore = "needs the QEMU runtime: set ORIGAMI_RUNTIME_DIR"]
fn every_preset_creates_reopens_and_starts_paused() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    let machines = machines();
    let mut failures = Vec::new();
    // Each starter must name an offering of this QEMU, and the registry
    // must have every image kind a preset's offering reads.
    for profile in origami::profiles::STARTERS {
        if let Err(error) = origami::preset(&machines, profile.id) {
            failures.push(format!("{}: {error}", profile.id));
        }
    }
    // Every topology the upgrade moves a machine to must exist, or the rename
    // would silently not apply.
    for topology in origami::migrate::renamed_to() {
        if !machines.offerings.iter().any(|o| o.topology == topology) {
            failures.push(format!(
                "the upgrade moves machines to {topology}, which QEMU lacks"
            ));
        }
    }
    let manifest = origami::assets::manifest().unwrap();
    // Only presets create machines, so an offering with no preset, such as
    // the O2 whose PROM has no download, needs none.
    for (_, offering) in origami::presets(&machines) {
        for input in &offering.init_inputs {
            if manifest.get(&input.kind).is_err() {
                failures.push(format!("{}: no {} download", offering.topology, input.kind));
            }
        }
    }
    // A few at a time: most of a start is QEMU allocating and the product
    // waiting for it.
    let presets = origami::presets(&machines);
    let next = std::sync::atomic::AtomicUsize::new(0);
    let found = std::sync::Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        for _ in 0..4 {
            scope.spawn(|| loop {
                let index = next.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let Some((name, offering)) = presets.get(index) else {
                    break;
                };
                let failed = check_preset(root, name, offering);
                found.lock().unwrap().extend(failed);
            });
        }
    });
    failures.extend(found.into_inner().unwrap());
    // The init tool refuses an image the machine does not read.
    let image = root.join("ip27.bin");
    synthetic_image(&image, "ip27-prom");
    let image = image.to_str().unwrap();
    let machine = root.join("origin200 with an IO PROM");
    match run(
        root,
        "create",
        &machine,
        &[
            "--preset",
            "origin200-1",
            "--prom",
            image,
            "--io-prom",
            image,
        ],
    ) {
        Err(error) if error.contains("takes no --io-prom") && !machine.exists() => (),
        other => failures.push(format!("origin200-1 with --io-prom: {other:?}")),
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Every way the product connects a machine to a network, on the preset
/// every other test starts. The host-dependent modes need a device or a
/// daemon and are left to the functional and integration tests.
#[test]
#[ignore = "needs the QEMU runtime: set ORIGAMI_RUNTIME_DIR"]
fn each_network_mode_starts_paused() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    let machines = machines();
    let offering = origami::preset(&machines, "origin200-1").unwrap();
    let code = root.join("code");
    fs::create_dir(&code).unwrap();
    let peer = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("tcp:{}", peer.local_addr().unwrap());
    let forward_port = origami::control::free_port().unwrap().to_string();
    let cases: [(&str, Vec<&str>); 5] = [
        ("user", vec![]),
        ("none", vec!["--mode", "none"]),
        (
            "private",
            vec!["--mode", "private", "--endpoint", endpoint.as_str()],
        ),
        // Without an endpoint, `run` hosts the private network itself.
        ("hosted", vec!["--mode", "private"]),
        ("shared", vec![]),
    ];
    let mut failures = Vec::new();
    for (name, network) in cases {
        let machine = create(root, name, "origin200-1", offering, &[]).unwrap();
        let mut set = vec!["--mode", "user"];
        if !network.is_empty() {
            set = network;
        }
        let result = run(root, "network-set", &machine, &set)
            .and_then(|_| {
                if name == "user" {
                    run(
                        root,
                        "network-forward-add",
                        &machine,
                        &[
                            "web",
                            "--protocol",
                            "tcp",
                            "--host-port",
                            &forward_port,
                            "--guest-port",
                            "80",
                        ],
                    )
                    .map(drop)
                } else if name == "shared" {
                    run(root, "share", &machine, &["src", code.to_str().unwrap()]).map(drop)
                } else {
                    Ok(())
                }
            })
            .and_then(|_| start_and_stop(root, &machine));
        if let Err(error) = result {
            failures.push(format!("{name}: {error}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// The harness has to be able to see QEMU refuse: an argument it does not
/// know must fail the start, with QEMU's own message.
#[test]
#[ignore = "needs the QEMU runtime: set ORIGAMI_RUNTIME_DIR"]
fn qemu_refusing_an_argument_fails_the_start() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    let machines = machines();
    let offering = origami::preset(&machines, "origin200-1").unwrap();
    let machine = create(root, "machine", "origin200-1", offering, &[]).unwrap();
    let error = run(
        root,
        "run",
        &machine,
        &[
            "--background",
            "--display",
            "none",
            "--qemu-arg=-S",
            "--qemu-arg=-no-such-option",
        ],
    )
    .unwrap_err();
    assert!(error.contains("machine failed to start"), "{error}");
    assert!(error.contains("no-such-option"), "{error}");
}

/// The machine list the fake runtime serves is cut from a real QEMU's
/// reply and says so in `tests/fixtures/generate-machines.py`. When QEMU's
/// list changes under a field the tests copy, the fake would go on serving
/// the old one and the component tests would pass against a catalogue that
/// no longer exists.
#[test]
#[ignore = "needs the QEMU runtime: set ORIGAMI_RUNTIME_DIR"]
fn the_fakes_machine_list_is_cut_from_the_real_one() {
    let real = real_machine_list();
    let fixture: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sgi-machines.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        fixture["version"], real["version"],
        "the machine list version moved"
    );
    let mut stale = Vec::new();
    for key in ["offerings", "topologies", "setting-definitions"] {
        for record in fixture[key].as_array().unwrap() {
            if !real[key]
                .as_array()
                .unwrap()
                .iter()
                .any(|candidate| is_cut_from(record, candidate))
            {
                stale.push(format!("{key}: {record}"));
            }
        }
    }
    assert!(
        stale.is_empty(),
        "regenerate tests/fixtures/sgi-machines.json with generate-machines.py; the real list has no record for:\n{}",
        stale.iter().map(|record| record.chars().take(300).collect::<String>()).collect::<Vec<_>>().join("\n")
    );
}

/// Whether every field of `cut` is in `whole`, with the same value.
fn is_cut_from(cut: &serde_json::Value, whole: &serde_json::Value) -> bool {
    match (cut, whole) {
        (serde_json::Value::Object(cut), serde_json::Value::Object(whole)) => {
            cut.iter().all(|(key, value)| {
                whole
                    .get(key)
                    .is_some_and(|other| is_cut_from(value, other))
            })
        }
        (serde_json::Value::Array(cut), serde_json::Value::Array(whole)) => {
            cut.len() == whole.len() && cut.iter().zip(whole).all(|(a, b)| is_cut_from(a, b))
        }
        _ => cut == whole,
    }
}

/// QEMU's `query-sgi-machines` reply, unparsed.
fn real_machine_list() -> serde_json::Value {
    let qemu = runtime().join(if cfg!(windows) {
        "qemu-system-mips64.exe"
    } else {
        "qemu-system-mips64"
    });
    let mut child = Command::new(qemu)
        .args([
            "-M",
            "none",
            "-qmp",
            "stdio",
            "-nodefaults",
            "-display",
            "none",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    lines.next().unwrap().unwrap(); // greeting
    let mut ask = |command: &str| -> serde_json::Value {
        writeln!(stdin, r#"{{"execute": "{command}"}}"#).unwrap();
        serde_json::from_str(&lines.next().unwrap().unwrap()).unwrap()
    };
    ask("qmp_capabilities");
    let reply = ask("query-sgi-machines");
    let _ = child.kill();
    let _ = child.wait();
    reply["return"].clone()
}

/// The runtime's own tools answer, so a layout or packaging change that
/// loses one shows here.
#[test]
#[ignore = "needs the QEMU runtime: set ORIGAMI_RUNTIME_DIR"]
fn the_runtime_carries_its_tools() {
    let root = tempfile::tempdir().unwrap();
    let suffix = if cfg!(windows) { ".exe" } else { "" };
    for tool in ["qemu-system-mips64", "qemu-img", "qemu-sgi-machine-init"] {
        assert!(
            runtime().join(format!("{tool}{suffix}")).is_file(),
            "{tool}"
        );
    }
    let img = Command::new(runtime().join(format!("qemu-img{suffix}")))
        .arg("--version")
        .output()
        .unwrap();
    assert!(img.status.success());
    let version = String::from_utf8_lossy(&origami(root.path(), &[OsStr::new("version")]).stdout)
        .into_owned();
    assert!(!version.contains("qemu unavailable"), "{version}");
}

/// Machines Origami 0.5.1 made name the Tezro and Onyx 350 topologies it
/// listed then. They take the catalogue's node-board names, keep their state, and
/// start on this QEMU.
#[test]
#[ignore = "needs the QEMU runtime: set ORIGAMI_RUNTIME_DIR"]
fn machines_with_the_051_topology_names_upgrade_and_start() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    let machines = machines();
    for (preset, old) in [
        ("tezro", "tezro-tower-multi"),
        ("onyx350", "onyx350-compat"),
    ] {
        let new = &origami::preset(&machines, preset).unwrap().topology;
        let offering = origami::preset(&machines, preset).unwrap();
        let machine = root.join(preset);
        let mut args: Vec<OsString> = vec!["--preset".into(), preset.into()];
        for input in &offering.init_inputs {
            let image = root.join(format!("{preset}-{}.bin", input.name));
            synthetic_image(&image, &input.kind);
            args.extend(["--prom".into(), image.into()]);
        }
        let args: Vec<_> = args.iter().map(|arg| arg.to_str().unwrap()).collect();
        run(root, "create", &machine, &args).unwrap();
        // 0.5.1 wrote the old topology name.
        let text = fs::read_to_string(machine.join("machine.toml")).unwrap();
        let text = text.replace(
            &format!("topology = \"{new}\""),
            &format!("topology = \"{old}\""),
        );
        assert!(text.contains(old), "{text}");
        fs::write(machine.join("machine.toml"), text).unwrap();
        let before =
            fs::read(machine.join(format!("state/{}.raw", offering.storage[0].name))).unwrap();

        let error = run(root, "validate", &machine, &[]).unwrap_err();
        assert!(
            error.contains(&format!("topology '{old}' is now '{new}'")),
            "{error}"
        );
        run(root, "upgrade", &machine, &[]).unwrap();
        run(root, "validate", &machine, &[]).unwrap();
        let after =
            fs::read(machine.join(format!("state/{}.raw", offering.storage[0].name))).unwrap();
        assert!(
            before == after,
            "{preset}: the upgrade changed the machine's state"
        );
        start_and_stop(root, &machine).unwrap();
    }
}

#[test]
#[ignore = "needs the QEMU runtime: set ORIGAMI_RUNTIME_DIR"]
fn format1_origin2000_without_a_topology_upgrades_and_starts() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    let machine = root.join("origin2000");
    for sub in ["firmware", "drives", "logs", "state/node-proms"] {
        fs::create_dir_all(machine.join(sub)).unwrap();
    }
    fs::write(
        machine.join("machine.toml"),
        "format = 1\n\n[machine]\nmodel = \"origin2000\"\nnodes = 4\ncpus_per_node = 2\n\
         memory_per_node = \"64MiB\"\ngraphics = \"none\"\n\n[firmware]\n\
         image = \"firmware/prom.bin\"\n\n[network]\nmode = \"user\"\n",
    )
    .unwrap();
    let prom = synthetic_image(&machine.join("firmware/prom.bin"), "ip27-prom");
    // Origami 0.1.1 built each node's flash from the PROM, erased past it.
    for node in 1..=4 {
        let mut flash = vec![0xff; 1048576];
        flash[..prom.len()].copy_from_slice(&prom);
        flash[prom.len()] = node;
        fs::write(
            machine.join(format!("state/node-proms/node{node}.bin")),
            flash,
        )
        .unwrap();
    }
    fs::write(machine.join("state/nvram0.raw"), vec![0x5a; 32768]).unwrap();
    fs::write(machine.join("state/nvram0.raw.clock"), [0; 16]).unwrap();
    let io_prom = root.join("io6prom.bin");
    synthetic_image(&io_prom, "io6-prom");

    run(
        root,
        "upgrade",
        &machine,
        &["--io-prom", io_prom.to_str().unwrap()],
    )
    .unwrap();
    for node in 0..4u8 {
        let flash = fs::read(machine.join(format!("state/node{node}-flash.raw"))).unwrap();
        assert_eq!(flash[prom.len()], node + 1, "node {node} flash");
    }
    assert_eq!(
        fs::read(machine.join("state/nvram0.raw")).unwrap(),
        [0x5a; 32768]
    );
    run(root, "validate", &machine, &[]).unwrap();
    start_and_stop(root, &machine).unwrap();
}
