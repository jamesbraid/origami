//! Functional tests: full-machine boots of the built product.
//!
//! A trial is one (engine, system) pair, named `<engine>::<family>::<system>`,
//! such as `qemu::ip27::origin200-1`. Each row of `tests/data/systems.toml`
//! names the engines that boot it. The QEMU engine uses the product's CLI to
//! create the machine, attach a published IRIX disk and start it, then checks
//! login, `uname`, `hinv`, a network address, a ping, NFS reads and writes, a
//! CD-ROM read and a clean halt. Pass a name or part of one as the filter to
//! run a subset.
//!
//! ORIGAMI_PRODUCT names an extracted product tree (`bin/origami` and
//! `libexec/origami`). Without it every trial is listed as ignored. Optional:
//! ORIGAMI_FUNCTIONAL_WORK (machines and the PROM cache), ORIGAMI_FUNCTIONAL_DISKS
//! (downloaded disks, kept between runs) and ORIGAMI_FUNCTIONAL_OUT (results.json
//! and console logs per trial).

mod checks;
mod console;
mod data;
mod product;
mod qemu;

use std::fs;
use std::io::Write;
use std::path::PathBuf;

use libtest_mimic::{Arguments, Failed, Trial};

use data::{Data, Family, System};

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

pub struct Setup {
    pub product: PathBuf,
    pub work: PathBuf,
    pub disks: PathBuf,
    pub out: PathBuf,
}

fn setup() -> Option<Setup> {
    let product = env_path("ORIGAMI_PRODUCT")?;
    let work = env_path("ORIGAMI_FUNCTIONAL_WORK")
        .unwrap_or_else(|| std::env::temp_dir().join("origami-functional"));
    Some(Setup {
        product,
        disks: env_path("ORIGAMI_FUNCTIONAL_DISKS").unwrap_or_else(|| work.join("disks")),
        out: env_path("ORIGAMI_FUNCTIONAL_OUT").unwrap_or_else(|| work.join("out")),
        work,
    })
}

/// The engines that have a driver.
const ENGINES: &[&str] = &["qemu"];

/// Why this (engine, system) pair cannot run here, if it cannot.
fn skip_reason(engine: &str, family: &Family, system: &System) -> Option<String> {
    let host = data::host();
    if let Some(reason) = system.skip.as_ref().or(family.skip.as_ref()) {
        return Some(reason.clone());
    }
    if !ENGINES.contains(&engine) {
        return Some(format!("there is no {engine} driver yet"));
    }
    if family.disk.is_none() {
        return Some("no disk is published for this family".into());
    }
    if !system.hosts.iter().any(|h| h == host) {
        return Some(format!("not run on {host}"));
    }
    None
}

fn main() {
    let mut args = Arguments::from_args();
    // One machine at a time: a guest wants a whole runner's memory, and
    // trials share one downloaded disk.
    args.test_threads = Some(1);
    let data: &'static Data = Box::leak(Box::new(data::load()));
    let setup: &'static Option<Setup> = Box::leak(Box::new(setup()));

    let mut trials = Vec::new();
    let mut skipped = Vec::new();
    for system in &data.system {
        let family = data
            .family
            .get(&system.family)
            .unwrap_or_else(|| panic!("{}: no family {}", system.id, system.family));
        for engine in &system.engines {
            let name = format!("{engine}::{}::{}", system.family, system.id);
            let reason = skip_reason(engine, family, system).or_else(|| {
                setup
                    .is_none()
                    .then(|| "ORIGAMI_PRODUCT is not set".to_string())
            });
            let ignored = reason.is_some();
            if let Some(reason) = &reason {
                skipped.push((name.clone(), reason.clone()));
            }
            let run = move || {
                if let Some(reason) = reason {
                    return Err(Failed::from(format!("skipped: {reason}")));
                }
                let setup = setup.as_ref().unwrap();
                qemu::boot(setup, family, system, &name).map_err(Failed::from)
            };
            trials.push(
                Trial::test(format!("{engine}::{}::{}", system.family, system.id), run)
                    .with_ignored_flag(ignored)
                    .with_kind(family.uname.to_lowercase()),
            );
        }
    }

    // Skipped systems never reach a trial body, so say why in the job summary.
    if !args.list {
        let wanted = |name: &str| {
            args.filter
                .as_ref()
                .is_none_or(|f| name.contains(f.as_str()))
        };
        let lines: Vec<_> = skipped
            .iter()
            .filter(|(n, _)| wanted(n))
            .map(|(n, r)| format!("- skipped `{n}`: {r}"))
            .collect();
        for line in &lines {
            eprintln!("{line}");
        }
        summary(&lines.join("\n"));
    }
    libtest_mimic::run(&args, trials).exit();
}

pub fn summary(text: &str) {
    if text.is_empty() {
        return;
    }
    if let Some(path) = env_path("GITHUB_STEP_SUMMARY") {
        if let Ok(mut file) = fs::OpenOptions::new().append(true).create(true).open(path) {
            let _ = writeln!(file, "{text}");
        }
    }
}

pub fn slug(name: &str) -> String {
    name.replace("::", "-")
}
