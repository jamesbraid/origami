//! Machines against a product build's own QEMU and init tool: each starter
//! preset names a catalogue offering and its machine is created, reopened,
//! started and stopped, and a format 1 machine upgrades and starts. The
//! other tests use stand-ins, so this runs only on request; the product
//! build's CTest runs it on native builds:
//!
//! ORIGAMI_RUNTIME_DIR=out/linux/run/libexec/origami cargo test --test product_state -- --ignored

use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::Path;
use std::process::{Command, Output};
use std::time::Duration;

fn origami(root: &Path, args: &[&OsStr]) -> Output {
    assert!(
        std::env::var_os("ORIGAMI_RUNTIME_DIR").is_some(),
        "select the product's libexec/origami with ORIGAMI_RUNTIME_DIR"
    );
    Command::new(env!("CARGO_BIN_EXE_origami"))
        .args(args)
        .env("XDG_CACHE_HOME", root.join("cache"))
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

/// Start `machine`'s QEMU in the background and stop it again. QEMU must
/// accept the machine's storage and answer on QMP.
fn start_and_stop(root: &Path, machine: &Path) -> Result<(), String> {
    run(root, "run", machine, &["--background", "--display", "none"]).map_err(|error| {
        let log = fs::read_to_string(machine.join("logs/runner.log")).unwrap_or_default();
        format!("{error}\n{log}")
    })?;
    run(root, "stop", machine, &[])?;
    for _ in 0..200 {
        let status = run(root, "status", machine, &[])?;
        if String::from_utf8_lossy(&status.stdout).trim() == "stopped" {
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

#[test]
#[ignore = "needs a product build's QEMU and qemu-sgi-machine-init in ORIGAMI_RUNTIME_DIR"]
fn every_preset_creates_reopens_and_starts() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    let catalog = origami::catalogue().unwrap();
    let mut failures = Vec::new();
    // Each starter must name an offering of this QEMU, and the registry
    // must have every image kind an offering reads.
    for profile in origami::profiles::STARTERS {
        if let Err(error) = origami::preset(&catalog, profile.id) {
            failures.push(format!("{}: {error}", profile.id));
        }
    }
    let manifest = origami::assets::manifest().unwrap();
    for offering in &catalog.offerings {
        for input in &offering.init_inputs {
            if manifest.get(&input.kind).is_err() {
                failures.push(format!("{}: no {} download", offering.topology, input.kind));
            }
        }
    }
    for (name, offering) in origami::presets(&catalog) {
        let machine = root.join(format!("{name}, with comma"));
        let mut args: Vec<OsString> = vec!["--preset".into(), name.clone().into()];
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
        let args: Vec<_> = args.iter().map(|arg| arg.to_str().unwrap()).collect();
        if let Err(error) = run(root, "create", &machine, &args) {
            failures.push(format!("{name}: {error}"));
            continue;
        }
        for item in &offering.storage {
            let path = machine.join(format!("state/{}.raw", item.name));
            match fs::metadata(&path) {
                Ok(metadata) if metadata.len() == item.size => (),
                other => failures.push(format!("{name}: {}: {other:?}", path.display())),
            }
        }
        let result = run(root, "validate", &machine, &[])
            .and_then(|_| run(root, "show-command", &machine, &[]))
            .and_then(|_| start_and_stop(root, &machine));
        if let Err(error) = result {
            failures.push(format!("{name}: {error}"));
        }
    }
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

#[test]
#[ignore = "needs a product build's QEMU and qemu-sgi-machine-init in ORIGAMI_RUNTIME_DIR"]
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
