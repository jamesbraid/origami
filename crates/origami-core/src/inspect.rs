//! Reading a machine: opening it through an engine, validating it, showing
//! it and listing the catalogue.

use crate::report::{collect, Incomplete};
use crate::Report;
use origami_engine::{Catalogue, Engine};
use origami_machine::dir::MachineDir;
use origami_machine::model::Machine;
use origami_machine::{
    addressing, graphics, listing, read_machine, serial, shares, MachineFile, Offering, Result,
};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The machine directory `path` names, made canonical.
pub fn directory(path: &Path) -> Result<PathBuf> {
    dunce::canonicalize(path).map_err(|error| {
        format!("cannot open machine directory '{}': {error}. Use the directory you created with origami create", path.display()).into()
    })
}

/// A machine file that passed the engine's checks, with the catalogue it
/// was checked against.
pub struct Loaded {
    pub dir: PathBuf,
    pub file: MachineFile,
    pub catalogue: Arc<Catalogue>,
}

impl Loaded {
    pub fn offering(&self) -> Result<&Offering> {
        origami_machine::offering(&self.catalogue, &self.file.machine)
    }
}

/// Check `file` through `engine` and return the catalogue it was checked
/// against.
pub fn check(engine: &dyn Engine, dir: &Path, file: &MachineFile) -> Result<Arc<Catalogue>> {
    let catalogue = engine.catalogue()?;
    let machine = Machine::from_file(&catalogue, file)?;
    engine
        .validate(&machine, &MachineDir::new(dir))
        .into_result()?;
    origami_machine::offering(&catalogue, &file.machine)?;
    Ok(catalogue)
}

/// Read the machine in `dir` and check it.
pub fn open(engine: &dyn Engine, dir: &Path) -> Result<Loaded> {
    let file = read_machine(dir)?;
    let catalogue = check(engine, dir, &file)?;
    Ok(Loaded {
        dir: dir.to_path_buf(),
        file,
        catalogue,
    })
}

/// The preset list: a table, every preset in detail, or one preset in
/// detail.
pub fn listing(engine: &dyn Engine, preset: Option<&str>, all: bool) -> Result<String> {
    let machines = engine.catalogue()?;
    match (preset, all) {
        (Some(preset), _) => listing::detail(&machines, preset),
        (None, true) => listing::all(&machines),
        (None, false) => listing::table(&machines),
    }
}

/// Check the machine in `dir`: the warnings, then the verdict.
pub fn validate(engine: &dyn Engine, dir: &Path) -> Result<Report> {
    let loaded = open(engine, dir)?;
    let (machines, file) = (&loaded.catalogue, &loaded.file);
    let offer = loaded.offering()?;
    let mut report = Report::default();
    let warnings = origami_machine::mac::duplicate_warnings(machines, dir, file)
        .into_iter()
        .chain(origami_machine::emu_id::duplicate_warnings(dir, file))
        .chain(origami_machine::emu_id::unconfigured_warnings(file));
    for warning in warnings {
        report.warning(warning);
    }
    report.result(match &file.machine.processor {
        Some(processor) => format!(
            "valid: {} ({} CPUs, {processor})",
            offer.topology, offer.cpus
        ),
        None => format!("valid: {} ({} CPUs)", offer.topology, offer.cpus),
    });
    Ok(report)
}

/// What `origami show` says about the machine in `dir`.
pub fn show(engine: &dyn Engine, dir: &Path) -> std::result::Result<Report, Incomplete> {
    collect(|report| {
        let Loaded {
            file, catalogue, ..
        } = open(engine, dir)?;
        let machines = &catalogue;
        let offer = origami_machine::offering(machines, &file.machine)?;
        report.detail(format!(
            "{}: {} CPUs, {} nodes, {} per node",
            file.machine.model, offer.cpus, offer.nodes, file.machine.memory_per_node,
        ));
        for line in graphics::show_lines(machines, &offer.topology, &file.pipe) {
            report.detail(line);
        }
        report.detail(match &file.machine.processor {
            Some(id) => format!("processor: {} ({id})", offer.processor),
            None => format!("processor: {}", offer.processor),
        });
        report.detail(format!("state: {}", dir.join("state").display()));
        let console = file.machine.console.as_deref();
        report.detail(format!(
            "console: {}",
            console.map_or_else(|| serial::default_console(offer), Ok)?
        ));
        report.detail(match offer.default_network() {
            Some(adapter) => format!(
                "network: {} ({}{})",
                file.network.mode,
                adapter
                    .interface
                    .as_deref()
                    .map_or_else(String::new, |name| format!("{name}, ")),
                adapter.chip()
            ),
            None => format!("network: {}", file.network.mode),
        });
        if matches!(file.network.mode.as_str(), "user" | "private") {
            report.detail(format!(
                "subnet: {}",
                addressing::Addresses::of(&file)?.cidr()
            ));
        }
        let extra_args = file.engine.extra_args();
        if !extra_args.is_empty() {
            report.detail(format!(
                "qemu extra-args (unsupported): {}",
                extra_args
                    .iter()
                    .map(|arg| origami_machine::shell_quote(arg))
                    .collect::<Vec<_>>()
                    .join(" ")
            ));
        }
        for share in &file.share {
            report.detail(format!("share {}: {}", share.name, share.path.display()));
            if let Ok(lines) = shares::mount_lines(&file, share) {
                report.detail(format!("  {}", lines[1]));
            }
        }
        for forward in &file.network.forward {
            let status = if file.network.mode == "user" {
                ""
            } else {
                " (inactive until user networking)"
            };
            report.detail(format!(
                "forward {}: {} 127.0.0.1:{} -> guest:{}{}",
                forward.name, forward.protocol, forward.host_port, forward.guest_port, status
            ));
        }
        if let Some(identity) = &file.identity {
            report.detail(format!("identity MAC: {}", identity.mac));
            if let Some(emu_id) = identity.emu_id {
                report.detail(format!("emu-id: {emu_id}"));
            }
        }
        for drive in &file.drive {
            report.detail(format!(
                "{}: {} at {}:{} ({})",
                drive.name,
                drive.kind,
                offer.scsi_bus_name(drive.bus),
                drive.target,
                drive.image
            ));
        }
        Ok(())
    })
}
