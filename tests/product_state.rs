//! The starter presets against a product build's own QEMU and init tool:
//! each names a catalogue offering, and creates and reopens its machine.
//! The other tests use stand-ins, so this runs only on request; the product
//! build's CTest runs it on native builds:
//!
//! ORIGAMI_RUNTIME_DIR=out/linux/run/libexec/origami cargo test --test product_state -- --ignored

use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn origami(root: &Path, args: &[&OsStr]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_origami"))
        .args(args)
        .env("XDG_CACHE_HOME", root.join("cache"))
        .output()
        .unwrap()
}

/// A synthetic image of a size the init tool accepts for `kind`: a MIPS
/// branch-to-self and its delay slot, zero-padded. It is not SGI firmware.
fn synthetic_image(path: &Path, kind: &str) {
    let size = match kind {
        // An IP35 image must decode to exactly the PROM size QEMU loads.
        "ip35-prom" => 1_476_264,
        _ => 64 * 1024,
    };
    let mut image = vec![0x10, 0x00, 0xff, 0xff, 0, 0, 0, 0];
    image.resize(size, 0);
    fs::write(path, image).unwrap();
}

#[test]
#[ignore = "needs a product build's QEMU and qemu-sgi-machine-init in ORIGAMI_RUNTIME_DIR"]
fn every_preset_creates_the_catalogue_storage_and_reopens() {
    assert!(
        std::env::var_os("ORIGAMI_RUNTIME_DIR").is_some(),
        "select the product's libexec/origami with ORIGAMI_RUNTIME_DIR"
    );
    let root = TempDir(std::env::temp_dir().join(format!(
        "origami-product-state-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    )));
    fs::create_dir(&root.0).unwrap();
    let catalog = origami::catalogue().unwrap();
    let mut failures = Vec::new();
    // Each starter must name an offering of this QEMU, with the PROMs that
    // offering needs.
    for profile in origami::profiles::STARTERS {
        match origami::preset(&catalog, profile.id) {
            Ok(offering) => {
                let io_prom = offering.init_inputs.iter().any(|i| i.name == "io-prom");
                if io_prom != profile.io_prom.is_some() {
                    failures.push(format!("{}: IO PROM selection", profile.id));
                }
            }
            Err(error) => failures.push(format!("{}: {error}", profile.id)),
        }
    }
    for (name, offering) in origami::presets(&catalog) {
        let machine = root.0.join(format!("{name}, with comma"));
        let mut args: Vec<OsString> = vec![
            "create".into(),
            machine.clone().into(),
            "--preset".into(),
            name.clone().into(),
        ];
        for input in &offering.init_inputs {
            let image = root.0.join(format!("{name}-{}.bin", input.name));
            synthetic_image(&image, &input.kind);
            let option = if input.name == "boot-prom" {
                "--prom"
            } else {
                "--io-prom"
            };
            args.extend([option.into(), image.into()]);
        }
        let args: Vec<_> = args.iter().map(OsString::as_os_str).collect();
        let output = origami(&root.0, &args);
        if !output.status.success() {
            failures.push(format!(
                "{name}: create: {}",
                String::from_utf8_lossy(&output.stderr)
            ));
            continue;
        }
        for item in &offering.storage {
            let path = machine.join(format!("state/{}.raw", item.name));
            match fs::metadata(&path) {
                Ok(metadata) if metadata.len() == item.size => (),
                other => failures.push(format!("{name}: {}: {other:?}", path.display())),
            }
        }
        for command in ["validate", "show-command"] {
            let output = origami(&root.0, &[command.as_ref(), machine.as_os_str()]);
            if !output.status.success() {
                failures.push(format!(
                    "{name}: {command}: {}",
                    String::from_utf8_lossy(&output.stderr)
                ));
            }
        }
    }
    // The init tool refuses an image the machine does not read.
    let image = root.0.join("ip27.bin");
    synthetic_image(&image, "ip27-prom");
    let machine = root.0.join("origin200 with an IO PROM");
    let output = origami(
        &root.0,
        &[
            "create".as_ref(),
            machine.as_os_str(),
            "--preset".as_ref(),
            "origin200-1".as_ref(),
            "--prom".as_ref(),
            image.as_os_str(),
            "--io-prom".as_ref(),
            image.as_os_str(),
        ],
    );
    let error = String::from_utf8_lossy(&output.stderr);
    if output.status.success() || !error.contains("takes no --io-prom") || machine.exists() {
        failures.push(format!("origin200-1 with --io-prom: {error}"));
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
