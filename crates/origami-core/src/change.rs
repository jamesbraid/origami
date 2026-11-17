//! Edits to a stopped machine. Every step of a batch is applied to a copy
//! of `machine.toml` under the machine's edit lock; the file and the stores
//! are written only when every step succeeded.

use crate::inspect::check;
use crate::spec::{DriveRequest, GraphicsRequest, NetworkChange, NetworkMode};
use crate::Report;
use origami_engine::{Catalogue, Engine};
use origami_machine::{
    addressing, control, graphics, nvram, read_machine, serial, shares, Drive, MachineFile,
    Network, Offering, PortForward, Result, Share,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Change {
    SetGraphics(GraphicsRequest),
    /// The serial line the console uses; none for the machine's default.
    SetConsole(Option<String>),
    /// PROM environment variables, as name and value.
    SetNvram(Vec<(String, String)>),
    SetNetwork(NetworkChange),
    AddForward(PortForward),
    RemoveForward(String),
    AddShare {
        name: String,
        host_dir: PathBuf,
    },
    RemoveShare(String),
    AttachDrive(DriveRequest),
    DetachDrive(String),
    /// A new, empty system disk in the engine's preferred format.
    CreateDisk {
        size_mib: u64,
    },
    /// The CPUs per node, within the processor module the machine was made
    /// with. The node flash records the module at creation, so naming
    /// another is refused.
    SetProcessor {
        module: Option<String>,
        cpus_per_node: Option<u32>,
    },
    /// Memory per node in MiB, from the list the machine's offering accepts.
    SetMemory(u32),
    /// Machine settings, as `create --set` takes them. Each replaces the
    /// machine's value for that name; the others stay.
    SetSettings(BTreeMap<String, String>),
}

impl Change {
    /// What the edit lock's refusal says the machine must not be doing.
    fn action(&self) -> &'static str {
        match self {
            Self::SetGraphics(_) => "changing its graphics",
            Self::SetConsole(_) => "changing its console",
            Self::SetNvram(_) => "reading or changing its NVRAM",
            Self::SetNetwork(_) | Self::AddForward(_) | Self::RemoveForward(_) => {
                "changing its network"
            }
            Self::AddShare { .. } | Self::RemoveShare(_) => "changing its shares",
            Self::AttachDrive(_) | Self::CreateDisk { .. } => "changing drives",
            Self::DetachDrive(_) => "detaching a drive",
            Self::SetProcessor { .. } => "changing its processor",
            Self::SetMemory(_) => "changing its memory",
            Self::SetSettings(_) => "changing its settings",
        }
    }
}

/// The offerings with `offer`'s topology, nodes and processor module: the
/// ones a processor change may move between.
fn same_module<'a>(
    catalogue: &'a Catalogue,
    offer: &'a Offering,
) -> impl Iterator<Item = &'a Offering> {
    catalogue.offerings.iter().filter(move |o| {
        o.topology == offer.topology
            && o.nodes == offer.nodes
            && o.processor.id == offer.processor.id
    })
}

/// The CPUs per node `SetProcessor` accepts for the machine of `offer`.
pub(crate) fn cpu_counts(catalogue: &Catalogue, offer: &Offering) -> Vec<u32> {
    let mut counts: Vec<u32> = same_module(catalogue, offer)
        .map(|o| o.cpus_per_node[0])
        .collect();
    counts.sort_unstable();
    counts.dedup();
    counts
}

/// Apply `change` to the stopped machine in `dir`.
pub fn apply_change(engine: &dyn Engine, dir: &Path, change: &Change) -> Result<Report> {
    apply_changes(engine, dir, std::slice::from_ref(change))
}

/// Apply `changes` in order to the stopped machine in `dir`. Either all of
/// them take effect, or the machine file and its stores are untouched.
pub fn apply_changes(engine: &dyn Engine, dir: &Path, changes: &[Change]) -> Result<Report> {
    let Some(first) = changes.first() else {
        return Ok(Report::default());
    };
    let _lock = control::lock_for_edit(dir, first.action())?;
    let mut staged = Staged {
        engine,
        dir,
        file: read_machine(dir)?,
        dirty: false,
        store: None,
        created: vec![],
        locks: vec![],
        report: Report::default(),
    };
    let result = (|| -> Result<()> {
        for change in changes {
            staged.step(change)?;
        }
        staged.commit()
    })();
    match result {
        Ok(()) => Ok(staged.report),
        Err(error) => {
            for path in &staged.created {
                let _ = fs::remove_file(path);
            }
            let _ = fs::remove_file(staged.scratch());
            Err(error)
        }
    }
}

/// The variables of the NVRAM of the stopped machine in `dir`, or the one
/// named. Takes the edit lock, as changing them does.
pub fn nvram_read(engine: &dyn Engine, dir: &Path, name: Option<&str>) -> Result<Report> {
    let _lock = control::lock_for_edit(dir, "reading or changing its NVRAM")?;
    let loaded = crate::inspect::open(engine, dir)?;
    let store = nvram::open(dir, loaded.offering()?)?;
    let mut report = Report::default();
    match name {
        None => {
            for (name, value) in store.variables() {
                report.detail(format!("{name}={value}"));
            }
        }
        Some(name) => {
            let (name, value) = store.get(name)?;
            report.detail(format!("{name}={value}"));
        }
    }
    Ok(report)
}

struct Staged<'a> {
    engine: &'a dyn Engine,
    dir: &'a Path,
    file: MachineFile,
    dirty: bool,
    /// The NVRAM, opened by the first step that changes it and saved last.
    store: Option<nvram::Store>,
    /// Files made on the way, removed if a later step fails.
    created: Vec<PathBuf>,
    /// Held until the machine file is written.
    locks: Vec<fs::File>,
    report: Report,
}

impl Staged<'_> {
    fn check(&self) -> Result<Arc<Catalogue>> {
        check(self.engine, self.dir, &self.file)
    }

    fn offering<'c>(&self, catalogue: &'c Catalogue) -> Result<&'c Offering> {
        origami_machine::offering(catalogue, &self.file.machine)
    }

    fn scratch(&self) -> PathBuf {
        self.dir.join("machine.toml.tmp")
    }

    fn step(&mut self, change: &Change) -> Result<()> {
        match change {
            Change::SetGraphics(request) => self.set_graphics(request),
            Change::SetConsole(port) => {
                self.file.machine.console = port.clone();
                let catalogue = self.check()?;
                let offer = self.offering(&catalogue)?;
                let console = self.file.machine.console.as_deref();
                let line = format!(
                    "console: {}",
                    console.map_or_else(|| serial::default_console(offer), Ok)?
                );
                self.report.result(line);
                self.dirty = true;
                Ok(())
            }
            Change::SetNvram(settings) => {
                if self.store.is_none() {
                    let catalogue = self.check()?;
                    let offer = self.offering(&catalogue)?;
                    self.store = Some(nvram::open(self.dir, offer)?);
                }
                let store = self.store.as_mut().expect("store just opened");
                let mut names = Vec::new();
                for (name, value) in settings {
                    names.push(store.set(name, value)?);
                }
                for name in names {
                    let line = format!("{name}={}", store.get(name)?.1);
                    self.report.detail(line);
                }
                Ok(())
            }
            Change::SetNetwork(network) => self.set_network(network),
            Change::AddForward(forward) => {
                self.file.network.forward.push(forward.clone());
                self.check()?;
                self.report
                    .result(format!("forward added: {}", forward.name));
                self.dirty = true;
                Ok(())
            }
            Change::RemoveForward(name) => {
                let count = self.file.network.forward.len();
                self.file
                    .network
                    .forward
                    .retain(|forward| &forward.name != name);
                if self.file.network.forward.len() == count {
                    return Err(format!("unknown forward: {name}").into());
                }
                self.check()?;
                self.report.result(format!("forward removed: {name}"));
                self.dirty = true;
                Ok(())
            }
            Change::AddShare { name, host_dir } => self.add_share(name, host_dir),
            Change::RemoveShare(name) => {
                let count = self.file.share.len();
                self.file.share.retain(|share| &share.name != name);
                if self.file.share.len() == count {
                    return Err(format!("unknown share: {name}").into());
                }
                self.check()?;
                self.report.result(format!("share removed: {name}"));
                self.dirty = true;
                Ok(())
            }
            Change::AttachDrive(request) => {
                let path = dunce::canonicalize(&request.file)?;
                let kind = request.kind.as_str();
                self.file.drive.push(Drive {
                    name: format!("{kind}{}", request.target),
                    kind: kind.into(),
                    bus: 0,
                    target: request.target,
                    image: path.display().to_string(),
                    read_only: request.kind == crate::DriveKind::Cdrom,
                    overlay: request.overlay,
                });
                self.check()?;
                self.report.result(format!("attached {}", path.display()));
                self.dirty = true;
                Ok(())
            }
            Change::DetachDrive(name) => {
                let matches: Vec<_> = self
                    .file
                    .drive
                    .iter()
                    .enumerate()
                    .filter(|(_, drive)| &drive.name == name)
                    .map(|(index, _)| index)
                    .collect();
                if matches.len() != 1 {
                    return Err(format!(
                        "expected one drive named {name}, found {}",
                        matches.len()
                    )
                    .into());
                }
                self.file.drive.remove(matches[0]);
                self.check()?;
                self.report.result(format!("detached {name}"));
                self.dirty = true;
                Ok(())
            }
            Change::CreateDisk { size_mib } => self.create_disk(*size_mib),
            Change::SetProcessor {
                module,
                cpus_per_node,
            } => self.set_processor(module.as_deref(), *cpus_per_node),
            Change::SetMemory(mib) => self.set_memory(*mib),
            Change::SetSettings(settings) => self.set_settings(settings),
        }
    }

    fn set_processor(&mut self, module: Option<&str>, cpus_per_node: Option<u32>) -> Result<()> {
        let catalogue = self.check()?;
        let offer = self.offering(&catalogue)?;
        if let Some(module) = module.filter(|module| *module != offer.processor.id) {
            return Err(format!(
                "this machine was made with {}, and its node flash records the processor \
                 module, so {module} needs a new machine",
                offer.processor.id
            )
            .into());
        }
        let Some(cpus) = cpus_per_node.filter(|cpus| [*cpus] != offer.cpus_per_node[..1]) else {
            self.report.result("processor unchanged");
            return Ok(());
        };
        let population = vec![cpus; offer.nodes as usize];
        if !same_module(&catalogue, offer).any(|o| o.cpus_per_node == population) {
            let offered = cpu_counts(&catalogue, offer);
            return Err(format!(
                "{} with {} takes {} CPUs per node, not {cpus}",
                offer.topology,
                offer.processor.id,
                offered
                    .iter()
                    .map(u32::to_string)
                    .collect::<Vec<_>>()
                    .join(" or ")
            )
            .into());
        }
        self.file.machine.cpus_per_node = cpus;
        if !self.file.machine.population.is_empty() {
            self.file.machine.population = population;
        }
        self.check()?;
        self.report
            .result(format!("processor: {cpus} CPUs per node"));
        self.dirty = true;
        Ok(())
    }

    fn set_memory(&mut self, mib: u32) -> Result<()> {
        if self.file.machine.memory_per_node == format!("{mib}MiB") {
            self.report.result("memory unchanged");
            return Ok(());
        }
        self.file.machine.memory_per_node = format!("{mib}MiB");
        self.check()?;
        self.report.result(format!(
            "memory: {mib}MiB per node; the guest sees it at its next boot"
        ));
        self.dirty = true;
        Ok(())
    }

    fn set_settings(&mut self, settings: &BTreeMap<String, String>) -> Result<()> {
        let catalogue = self.check()?;
        let offer = self.offering(&catalogue)?;
        let mut inputs = self.file.machine.inputs.clone();
        inputs.extend(settings.clone());
        let inputs = origami_machine::settings::resolve(&catalogue, offer, inputs)?;
        if inputs == self.file.machine.inputs {
            self.report.result("settings unchanged");
            return Ok(());
        }
        self.file.machine.inputs = inputs;
        self.check()?;
        for (name, value) in settings {
            self.report.detail(format!("{name}={value}"));
        }
        self.report
            .result("settings changed; the guest sees them at its next boot");
        self.dirty = true;
        Ok(())
    }

    fn set_graphics(&mut self, args: &GraphicsRequest) -> Result<()> {
        let catalogue = self.check()?;
        let offer = self.offering(&catalogue)?;
        if args.product.is_none() {
            return Err("name the graphics with --graphics PRODUCT or --graphics none".into());
        }
        let mut request = args.request();
        // Changing a setting of the pipe it has keeps that pipe's other values.
        if let Some(current) = self.file.pipe.iter().find(|pipe| {
            request.product == Some(pipe.product.as_str())
                && request.slot.is_none_or(|slot| slot == pipe.slot)
        }) {
            let mut settings = current.settings.clone();
            settings.append(&mut request.settings);
            request.settings = settings;
            request.slot = Some(&current.slot);
        }
        let pipes = graphics::select(&catalogue, &offer.topology, offer.nodes, &request)?;
        if pipes == self.file.pipe {
            self.report.result("graphics unchanged");
            return Ok(());
        }
        graphics::check_change(&catalogue, &offer.topology, &self.file.pipe, &pipes)?;
        self.file.pipe = pipes;
        self.check()?;
        self.report
            .result("graphics changed; the guest sees them at its next boot");
        self.dirty = true;
        Ok(())
    }

    fn set_network(&mut self, change: &NetworkChange) -> Result<()> {
        use NetworkMode::{Private, Tap, User, Vmnet};
        let mode = change.mode;
        let old_subnet = addressing::Addresses::of(&self.file)?;
        self.file.network = Network {
            mode: mode.as_str().into(),
            endpoint: change.endpoint.clone(),
            interface: change.interface.clone(),
            forward: self.file.network.forward.clone(),
            share_address: change.share_address.clone(),
            subnet: change
                .subnet
                .clone()
                .or_else(|| self.file.network.subnet.clone()),
        };
        let catalogue = self.engine.catalogue()?;
        // Held until machine.toml is written, so no other mint rescans
        // the siblings before this emu-id is on disk.
        self.locks.push(origami_machine::emu_id::lock(self.dir)?);
        // User and none networking run without one; the rest need it.
        let assigned = if matches!(mode, Private | Tap | Vmnet) {
            origami_machine::ensure_machine_mac(
                &catalogue,
                self.dir,
                &mut self.file,
                change.mac.as_deref(),
            )?
        } else {
            origami_machine::set_machine_mac(
                &catalogue,
                self.dir,
                &mut self.file,
                change.mac.as_deref(),
            )?;
            None
        };
        self.check()?;
        if let Some(mac) = assigned {
            self.report.result(format!("assigned MAC {mac}"));
        }
        self.report.result(format!("network: {}", mode.as_str()));
        if matches!(mode, User | Private) {
            let subnet = addressing::Addresses::of(&self.file)?;
            self.report.detail(format!("subnet: {}", subnet.cidr()));
            if subnet != old_subnet {
                self.report.push(
                    crate::Kind::Advice,
                    format!(
                        "A guest that booted on {} keeps its old address until, as root in IRIX, \
                         you run rm /var/adm/proclaim.lease_info* /etc/resolv.conf and restart it.",
                        old_subnet.cidr()
                    ),
                );
            }
        }
        if mode != User && !self.file.network.forward.is_empty() {
            self.report
                .detail(format!("forwards inactive in {} mode", mode.as_str()));
        }
        self.dirty = true;
        Ok(())
    }

    fn add_share(&mut self, name: &str, host_dir: &Path) -> Result<()> {
        let path = std::path::absolute(host_dir)?;
        if !path.is_dir() {
            return Err(format!("{} is not a directory", host_dir.display()).into());
        }
        shares::check_name(name)?;
        if self.file.share.iter().any(|share| share.name == name) {
            return Err(format!(
                "share {name} exists; remove it first: origami share {} {name} --remove",
                self.dir.display()
            )
            .into());
        }
        self.file.share.push(Share {
            name: name.into(),
            path,
        });
        self.check()?;
        let share = self.file.share.last().expect("share just added");
        self.report.result(format!("share added: {name}"));
        for line in shares::mount_lines(&self.file, share)? {
            self.report.detail(line);
        }
        self.dirty = true;
        Ok(())
    }

    fn create_disk(&mut self, size_mib: u64) -> Result<()> {
        let catalogue = self.check()?;
        let offer = self.offering(&catalogue)?;
        if self.file.drive.iter().any(|d| d.name == "system") {
            return Err("system disk already attached".into());
        }
        if self
            .file
            .drive
            .iter()
            .any(|drive| drive.bus == 0 && drive.target == 1)
        {
            return Err("system disk target 1 is occupied".into());
        }
        if !offer.scsi_bus(0).is_some_and(|s| s.targets.contains(&1)) {
            return Err("this machine has no system disk target".into());
        }
        let path = self
            .engine
            .create_disk(&self.dir.join("drives/system"), size_mib)?;
        self.created.push(path.clone());
        self.file.drive.push(Drive {
            name: "system".into(),
            kind: "disk".into(),
            bus: 0,
            target: 1,
            image: format!("drives/{}", path.file_name().unwrap().to_string_lossy()),
            read_only: false,
            overlay: false,
        });
        self.report.result(format!("created {}", path.display()));
        self.dirty = true;
        Ok(())
    }

    /// Write the stores and the machine file. The file is written beside
    /// its target first and moved into place last, so a failure on the way
    /// leaves the old one.
    fn commit(&mut self) -> Result<()> {
        let staged = self.dirty.then(|| self.scratch());
        if let Some(path) = &staged {
            fs::write(path, toml::to_string_pretty(&self.file)?)?;
        }
        if let Some(store) = &self.store {
            nvram::save(self.dir, store)?;
        }
        if let Some(path) = staged {
            origami_machine::keep_older_format(self.dir)?;
            fs::rename(path, self.dir.join("machine.toml"))?;
        }
        Ok(())
    }
}
