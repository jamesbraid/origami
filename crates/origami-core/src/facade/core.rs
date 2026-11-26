//! The entry point: a [`Core`] owns the engines it was given, the event
//! dispatcher and the machines opened through it.

use super::commands::{self, Command};
use super::disks::DiskImageCheck;
use super::error::{CoreError, CoreResult, ErrorKind};
use super::event::{Dispatcher, EventKind, Observer, Slot};
use super::machine::{ExitReason, Machine, MachineState, Shared};
use super::operation::{self, Operation};
use crate::Report;
use origami_engine::{Capabilities, Catalogue, Engine};
use origami_machine::dir::MachineDir;
use origami_machine::model;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

#[derive(Clone, Debug)]
pub struct Config {
    /// Where the machine library lives: every child directory holding a
    /// `machine.toml` is a machine.
    pub root: PathBuf,
    /// How much console output a machine keeps for late readers.
    pub console_ring_bytes: usize,
    /// How long a clean stop waits for the machine to end before its
    /// operation fails. The machine is not killed; Force Quit ends it.
    pub stop_patience: Duration,
    /// The install media server's program. Without it the install calls
    /// that run it fail.
    pub instigator: Option<PathBuf>,
    /// Moves a folder to the platform's trash; tests swap in a recorder.
    pub trash: fn(&Path) -> Result<(), String>,
}

#[cfg_attr(feature = "selftest", allow(dead_code))]
fn system_trash(path: &Path) -> Result<(), String> {
    trash::delete(path).map_err(|e| e.to_string())
}

/// With the `selftest` feature, "trashing" deletes the folder and records it,
/// so an app's self-test never reaches the host's real trash.
#[cfg(feature = "selftest")]
static TRASHED: Mutex<Vec<String>> = Mutex::new(Vec::new());

#[cfg(feature = "selftest")]
fn selftest_trash(path: &Path) -> Result<(), String> {
    TRASHED
        .lock()
        .unwrap()
        .push(path.to_string_lossy().into_owned());
    std::fs::remove_dir_all(path).map_err(|e| e.to_string())
}

/// The library entries removed so far under the `selftest` feature, in order.
#[cfg(feature = "selftest")]
pub fn selftest_trashed() -> Vec<String> {
    TRASHED.lock().unwrap().clone()
}

impl Config {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            console_ring_bytes: 1 << 20,
            stop_patience: Duration::from_secs(30),
            instigator: None,
            #[cfg(feature = "selftest")]
            trash: selftest_trash,
            #[cfg(not(feature = "selftest"))]
            trash: system_trash,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct EngineSummary {
    pub id: String,
    /// What a person calls the engine, such as `QEMU`.
    pub name: String,
    pub version: Option<String>,
    pub capabilities: Capabilities,
}

/// A machine an engine offers to create, with nothing engine-specific.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OfferingSummary {
    pub id: String,
    /// What the New Machine picker shows, such as `Fuel, 1 CPU`.
    pub label: String,
    pub engine: String,
    pub product: String,
    pub topology: String,
    /// What a person calls the topology, such as `Origin 200`.
    pub topology_name: String,
    /// Whether a machine made with the defaults has a graphics head, as
    /// against a serial console only.
    pub has_display: bool,
    /// Whether the machine runs IRIX, so Install IRIX applies.
    pub runs_irix: bool,
    pub nodes: u32,
    pub cpus: u32,
    pub memory_default_mib: u32,
    pub memory_accepted_mib: Vec<u32>,
}

/// The index, among `(id, product)` pairs, of the model New Machine selects:
/// `octane2-vpro`, else the first Octane2, else the first.
pub fn default_offering<'a>(offers: impl IntoIterator<Item = (&'a str, &'a str)>) -> Option<usize> {
    let offers: Vec<_> = offers.into_iter().collect();
    let at = |f: &dyn Fn(&(&str, &str)) -> bool| offers.iter().position(f);
    at(&|o| o.0 == "octane2-vpro")
        .or_else(|| at(&|o| o.1 == "Octane2"))
        .or(if offers.is_empty() { None } else { Some(0) })
}

#[cfg(test)]
mod default_offering_tests {
    use super::default_offering;

    #[test]
    fn prefers_the_vpro_then_any_octane2_then_the_first() {
        let fuel = ("fuel-1", "Fuel");
        let plain = ("octane2-v12", "Octane2");
        let vpro = ("octane2-vpro", "Octane2");
        assert_eq!(default_offering([fuel, plain, vpro]), Some(2));
        assert_eq!(default_offering([fuel, plain]), Some(1));
        assert_eq!(default_offering([fuel]), Some(0));
        assert_eq!(default_offering([]), None);
    }
}

/// One entry of the machine library.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MachineSummary {
    /// Pass to [`Core::open_machine`].
    pub key: String,
    pub name: String,
    pub product: String,
    /// The product's SGI name, such as "Origin 200", for showing to people.
    pub product_label: String,
    pub topology: Option<String>,
    /// What a person calls the topology; the topology itself where the
    /// catalogue has no name for it.
    pub topology_name: Option<String>,
    /// The tiles under the machine's name: memory, nodes where several, model.
    pub cards: Vec<super::SummaryCard>,
    /// Whether the machine has a graphics head, from its file.
    pub has_display: bool,
    /// Whether the machine runs IRIX, so Install IRIX applies.
    pub runs_irix: bool,
    pub nodes: u32,
    pub memory_per_node_mib: u32,
    /// Known only for a machine opened through this core.
    pub state: MachineState,
    /// Why the machine file cannot be used, when it cannot.
    pub problem: Option<String>,
    /// Made by an older Origami: [`Core::upgrade_machine`] first, then it opens.
    pub needs_upgrade: bool,
    /// What the system disk holds, such as `IRIX 6.5.30`, once read. A first
    /// listing starts the read and a library event follows it.
    pub system_disk: Option<String>,
}

/// What [`Core::remove_machine`] left behind.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Removed {
    /// The library entry that was moved: what a toast names.
    pub name: String,
    /// Disk images the machine used in place, outside its folder.
    pub disk_left: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub struct CreateRequest {
    /// The directory name under the library root.
    pub name: String,
    /// An [`OfferingSummary::id`].
    pub offering: String,
    /// An [`EngineSummary::id`] that lists the offering; without it the
    /// first engine that does. The machine keeps this engine for life.
    pub engine: Option<String>,
    pub memory_per_node_mib: Option<u32>,
    /// A graphics product, or `none`; without it the topology's default pipe.
    pub graphics: Option<String>,
    pub boot_prom: Option<PathBuf>,
    pub io_prom: Option<PathBuf>,
    pub system_disk: Option<SystemDisk>,
    /// The machine's ID as typed, 1 to 2999; empty or none takes the lowest free one.
    pub emu_id: Option<String>,
    /// The Ethernet address; without it the one the ID makes.
    pub mac: Option<String>,
}

/// The system disk a new machine starts with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SystemDisk {
    /// A new empty disk of this many GB.
    Empty { gb: u32 },
    /// A disk image the user has: used where it is, or copied into the
    /// machine's folder first. `overlay` keeps a CHD's changes apart.
    Image {
        file: PathBuf,
        copy: bool,
        overlay: bool,
    },
    /// A ready-made disk, by `ReadyDisk::name`: downloaded and checked first
    /// when it is not cached, then copied into the machine's folder with its
    /// changes kept apart. The cached file is never written.
    Ready { name: String },
}

pub(crate) fn attach_request(file: &Path, target: u32, overlay: bool) -> crate::DriveRequest {
    crate::DriveRequest {
        file: file.to_path_buf(),
        kind: crate::DriveKind::Disk,
        target,
        overlay: overlay && origami_machine::is_chd(file),
    }
}

/// The preset `offering` is the only one of, or none: with several presets
/// on one offering (Octane2 IMPACT and VPro) the catalogue's defaults apply.
fn preset_of(machines: &origami_machine::Machines, offering: &origami_machine::Offering) -> String {
    let mut found = origami_machine::profiles::presets(machines)
        .into_iter()
        .filter(|(_, o)| o.id == offering.id);
    match (found.next(), found.next()) {
        (Some((name, _)), None) => name,
        _ => String::new(),
    }
}

pub(crate) struct CoreInner {
    config: Config,
    /// The live library root; `config.root` is only where it started.
    root: RwLock<PathBuf>,
    engines: Vec<Arc<dyn Engine>>,
    catalogues: Mutex<Vec<Option<Arc<Catalogue>>>>,
    dispatcher: Dispatcher,
    slot: Arc<Slot>,
    machines: Mutex<HashMap<PathBuf, Machine>>,
    next_operation: Arc<AtomicU64>,
    /// Install operations, which own child processes and the console lease.
    installs: Mutex<Vec<Operation>>,
    /// Machines an automated install halted: their media server then ends
    /// when the guest's network goes away, which is not an error.
    halted: Arc<Mutex<std::collections::HashSet<String>>>,
}

impl Drop for CoreInner {
    fn drop(&mut self) {
        let installs = std::mem::take(&mut *self.installs.lock().unwrap());
        for install in &installs {
            install.cancel();
        }
        for install in &installs {
            install.wait(Some(std::time::Duration::from_secs(10)));
        }
    }
}

/// Clones share one core.
#[derive(Clone)]
pub struct Core {
    pub(super) inner: Arc<CoreInner>,
}

impl Core {
    /// A core over `engines`, in order of preference. `observer` hears events
    /// on a thread the core owns.
    pub fn new(
        config: Config,
        engines: Vec<Arc<dyn Engine>>,
        observer: Option<Arc<dyn Observer>>,
    ) -> CoreResult<Core> {
        if engines.is_empty() {
            return Err(CoreError::new(
                ErrorKind::Invalid,
                "a core needs at least one engine",
            ));
        }
        let dispatcher = Dispatcher::new(observer);
        let slot = dispatcher.slot(None);
        Ok(Core {
            inner: Arc::new(CoreInner {
                root: RwLock::new(config.root.clone()),
                config,
                catalogues: Mutex::new(vec![None; engines.len()]),
                engines,
                dispatcher,
                slot,
                machines: Mutex::default(),
                next_operation: Arc::new(AtomicU64::new(1)),
                installs: Mutex::default(),
                halted: Arc::default(),
            }),
        })
    }

    /// Stop delivering events and wait for a callback in flight to return, so
    /// do not call it while holding a lock the observer takes. Machines keep
    /// running; calls still work.
    pub fn shutdown(&self) {
        self.inner.dispatcher.shutdown();
    }

    pub fn root(&self) -> PathBuf {
        self.inner.root()
    }

    /// Point the library at another folder and tell the observer it changed.
    /// Open and running machines are untouched: they keep their own paths.
    pub fn set_root(&self, root: impl Into<PathBuf>) {
        *self.inner.root.write().unwrap() = root.into();
        self.inner.slot.raise(&[EventKind::Library]);
    }

    pub fn engines(&self) -> Vec<EngineSummary> {
        self.inner
            .engines
            .iter()
            .map(|engine| {
                let info = engine.info();
                EngineSummary {
                    name: super::engine_name(&info.id),
                    id: info.id,
                    version: info.version,
                    capabilities: info.capabilities,
                }
            })
            .collect()
    }

    /// The machine ID and Ethernet address a new machine of `offering` would
    /// get with the choices so far (`emu_id` as typed), and why they cannot be
    /// used. Reads the library folder; off the UI thread.
    pub fn preview_new_identity(
        &self,
        offering: &str,
        engine: Option<&str>,
        emu_id: &str,
        mac: Option<&str>,
    ) -> CoreResult<super::IdentityPreview> {
        let (index, offer) = self.inner.find_offering(offering, engine)?;
        let machines = self.inner.catalogue(index)?;
        // Machines are compared by the folder they sit in, so any new name will do.
        let dir = self.inner.root().join(".new-machine");
        Ok(super::network::preview(
            &machines, &dir, &offer, emu_id, mac,
        ))
    }

    /// What can be created: the engines' offerings, neutral fields only.
    /// The first call may start an engine's program.
    pub fn catalogue(&self) -> CoreResult<Vec<OfferingSummary>> {
        let mut out = Vec::new();
        for index in 0..self.inner.engines.len() {
            let id = self.inner.engines[index].info().id;
            let catalogue = self.inner.catalogue(index)?;
            for o in &catalogue.offerings {
                // An engine with no graphics records says so in its own data.
                let declared = o.engine.get(&id).and_then(|data| data["display"].as_bool());
                // One offering the catalogue cannot describe must not hide the rest.
                let has_display = match declared.map_or_else(
                    || {
                        origami_machine::profiles::has_default_display(
                            &catalogue,
                            &preset_of(&catalogue, o),
                            o,
                        )
                    },
                    Ok,
                ) {
                    Ok(shown) => shown,
                    Err(error) => {
                        eprintln!("origami: skipping offering {}: {error}", o.id);
                        continue;
                    }
                };
                out.push(OfferingSummary {
                    id: o.id.clone(),
                    label: origami_machine::listing::offering_label(o),
                    engine: id.clone(),
                    product: o.product.clone(),
                    topology: o.topology.clone(),
                    topology_name: origami_machine::listing::machine_name(&o.topology).into(),
                    has_display,
                    runs_irix: origami_machine::install::runs_irix(&o.product, &o.topology),
                    nodes: o.nodes,
                    cpus: o.cpus,
                    memory_default_mib: origami_machine::profiles::default_memory(o),
                    memory_accepted_mib: o.memory.accepted.clone(),
                });
            }
        }
        Ok(out)
    }

    /// The machines under the library root, by name. A machine whose file
    /// cannot be read is listed with a `problem`.
    pub fn list_machines(&self) -> CoreResult<Vec<MachineSummary>> {
        let entries = match std::fs::read_dir(self.inner.root()) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
            Err(error) => return Err(error.into()),
        };
        let mut out = Vec::new();
        for entry in entries {
            let path = entry?.path();
            if !path.join("machine.toml").is_file() {
                continue;
            }
            let name = entry_name(&path);
            let key = path.canonicalize().unwrap_or_else(|_| path.clone());
            let state = self.inner.machines.lock().unwrap().get(&key).map_or(
                MachineState::NotRunning,
                |m| {
                    let view = m.view();
                    // A guest that powered off or was stopped by us is simply stopped.
                    match (view.state, view.exit_reason) {
                        (
                            MachineState::Exited,
                            Some(ExitReason::GuestPoweredOff | ExitReason::HostStopped),
                        ) => MachineState::NotRunning,
                        (state, _) => state,
                    }
                },
            );
            let state = if state == MachineState::NotRunning && left_running(&path) {
                MachineState::LeftRunning
            } else {
                state
            };
            let key = key.to_string_lossy().into_owned();
            let legacy = is_legacy(&path);
            out.push(match origami_machine::read_machine(&path) {
                Ok(file) => {
                    let topology_name = file
                        .machine
                        .topology
                        .as_deref()
                        .map(|t| origami_machine::listing::machine_name(t).to_string());
                    let product_label = origami_machine::product_label(
                        &file.machine.model,
                        file.machine.topology.as_deref(),
                    );
                    let memory_per_node_mib = file
                        .machine
                        .memory_per_node
                        .trim_end_matches("MiB")
                        .parse()
                        .unwrap_or(0);
                    let system_disk = self.system_disk_label(&path, &file, state);
                    MachineSummary {
                        runs_irix: origami_machine::install::runs_irix(
                            &file.machine.model,
                            file.machine.topology.as_deref().unwrap_or_default(),
                        ),
                        key,
                        name,
                        cards: super::summary_cards(
                            topology_name.as_deref().unwrap_or(&product_label),
                            file.machine.nodes,
                            memory_per_node_mib,
                            system_disk.as_deref(),
                        ),
                        system_disk,
                        product_label,
                        product: file.machine.model,
                        topology_name,
                        has_display: !file.pipe.is_empty(),
                        topology: file.machine.topology,
                        nodes: file.machine.nodes,
                        memory_per_node_mib,
                        state,
                        problem: None,
                        needs_upgrade: legacy,
                    }
                }
                Err(error) => {
                    let error = CoreError::machine(error);
                    MachineSummary {
                        key,
                        name,
                        product: String::new(),
                        product_label: String::new(),
                        topology: None,
                        topology_name: None,
                        cards: Vec::new(),
                        has_display: false,
                        runs_irix: false,
                        nodes: 0,
                        memory_per_node_mib: 0,
                        state,
                        needs_upgrade: error.kind == ErrorKind::NeedsUpgrade,
                        system_disk: None,
                        problem: Some(error.message),
                    }
                }
            });
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    /// The label of the system disk if it is known. Otherwise a read of it
    /// starts, unless the machine runs, and a library event follows it.
    fn system_disk_label(
        &self,
        dir: &Path,
        file: &origami_machine::MachineFile,
        state: MachineState,
    ) -> Option<String> {
        let drive = system_drive(file)?;
        let image = origami_machine::resolve(dir, &drive.image);
        if let Some(label) = super::disks::cached_label(&image) {
            return super::disks::recognised(label);
        }
        let idle = matches!(state, MachineState::NotRunning | MachineState::Exited);
        // A running guest changes its disk, so keep the last label while it runs.
        if !idle {
            return super::disks::last_label(&image).and_then(super::disks::recognised);
        }
        if let Some(reading) = super::disks::begin_read(&image) {
            let slot = self.inner.slot.clone();
            std::thread::spawn(move || {
                // Only a stored label is news; a failed read would make the apps relist forever.
                if super::disks::label(&image).is_ok() {
                    slot.raise(&[EventKind::Library]);
                }
                drop(reading);
            });
        }
        None
    }

    /// The OS and release on a disk image, such as `IRIX 6.5.30`, or none when
    /// the disk is not recognised.
    pub fn disk_os(&self, path: &Path) -> CoreResult<Option<String>> {
        super::disks::label(path).map(super::disks::recognised)
    }

    /// The size the guest sees for the disk image at `path`: the virtual size
    /// of a qcow2 or CHD, else the file's.
    pub fn disk_size(&self, path: &Path) -> CoreResult<u64> {
        let bytes = super::disks::regular_size(path)?;
        Ok(super::disks::guest_size(path, bytes))
    }

    /// Whether `path` can be attached, and what a copy of it would need.
    /// `machine` is the one it is for; without it the library folder stands in.
    pub fn check_disk_image(
        &self,
        path: &Path,
        machine: Option<&str>,
    ) -> CoreResult<DiskImageCheck> {
        let bytes = super::disks::regular_size(path)?;
        let is_chd = origami_machine::is_chd(path);
        let mine = machine.map(|key| self.resolve(key)).transpose()?;
        let mut room = mine.clone().unwrap_or_else(|| self.inner.root());
        while !room.exists() && room.pop() {}
        let target = dunce::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        let used_by = std::fs::read_dir(self.inner.root())
            .into_iter()
            .flatten()
            .filter_map(|e| Some(e.ok()?.path()))
            .filter(|dir| mine.as_deref() != dir.canonicalize().ok().as_deref())
            .find(|dir| {
                origami_machine::read_machine(dir).is_ok_and(|file| {
                    file.drive.iter().any(|d| {
                        dunce::canonicalize(origami_machine::resolve(dir, &d.image))
                            .is_ok_and(|image| image == target)
                    })
                })
            })
            .map(|dir| entry_name(&dir));
        let free_bytes = super::disks::free_space(&room);
        Ok(DiskImageCheck {
            bytes,
            is_chd,
            free_bytes,
            used_by,
            problem: is_chd
                .then(|| {
                    origami_machine::check_chd(path)
                        .err()
                        .map(|e| e.to_string())
                })
                .flatten(),
        })
    }

    /// Make a machine in the library. Returns at once; the finished
    /// operation's `result_machine` is the new machine's key.
    pub fn create_machine(&self, request: CreateRequest) -> CoreResult<Operation> {
        let inner = self.inner.clone();
        let name = request.name.clone();
        if name.is_empty() || name.contains(['/', '\\']) || name == "." || name == ".." {
            return Err(CoreError::new(
                ErrorKind::Invalid,
                "a machine name is one plain directory name",
            )
            .with_detail("name"));
        }
        let taken = self.inner.root().join(&name);
        if taken.exists() {
            return Err(CoreError::new(
                ErrorKind::Invalid,
                format!("A machine named {name} already exists. Choose another name."),
            )
            .with_detail(taken.display().to_string()));
        }
        let emu_id = match request
            .emu_id
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
        {
            Some(text) => Some(
                text.parse::<u32>()
                    .ok()
                    .filter(|id| origami_machine::emu_id::check_range(*id).is_ok())
                    .ok_or_else(|| {
                        CoreError::new(ErrorKind::Invalid, super::network::not_an_id())
                    })?,
            ),
            None => None,
        };
        let slot = self.inner.slot.clone();
        // Read now: a switch made while this waits must not move the machine.
        let dir = inner.root().join(&request.name);
        Ok(self
            .inner
            .operation("Create machine", None, true, slot, move |control| {
                let (index, offering) =
                    inner.find_offering(&request.offering, request.engine.as_deref())?;
                let engine = inner.engines[index].clone();
                if control.is_cancelled() {
                    return Err(CoreError::new(
                        ErrorKind::Cancelled,
                        "creation was cancelled",
                    ));
                }
                // Before the machine exists, so a cancelled or failed download leaves nothing.
                let mut _hold = None;
                let system_disk = match &request.system_disk {
                    Some(SystemDisk::Ready { name }) => {
                        let (file, held) = super::ready::acquire(
                            name,
                            &offering.product,
                            &offering.topology,
                            &dir,
                            control,
                        )?;
                        _hold = Some(held);
                        Some(SystemDisk::Image {
                            file,
                            copy: true,
                            overlay: true,
                        })
                    }
                    other => other.clone(),
                };
                // The same create the CLI runs, so graphics, the serial
                // console of a headless machine, identity and settings agree.
                let machines = inner.catalogue(index)?;
                let mut spec = crate::MachineSpec::new(&dir, preset_of(&machines, &offering));
                spec.memory_per_node_mib = request.memory_per_node_mib;
                spec.graphics.product = request.graphics.clone();
                spec.identity.emu_id = emu_id;
                spec.identity.mac = request.mac.clone();
                spec.firmware = crate::FirmwareRequest {
                    boot_prom: request.boot_prom.clone(),
                    io_prom: request.io_prom.clone(),
                };
                if let Some(SystemDisk::Image {
                    file,
                    copy: false,
                    overlay,
                }) = &system_disk
                {
                    spec.drives.push(attach_request(file, 1, *overlay));
                }
                crate::create::create_from(engine.as_ref(), &machines, &spec, &offering)
                    .map_err(CoreError::machine)?;
                inner.slot.raise(&[EventKind::Library]);
                // A machine whose disk could not be made is removed, so Create works again;
                // a cancelled copy keeps it.
                let disk = (|| -> CoreResult<()> {
                    match &system_disk {
                        Some(SystemDisk::Empty { gb }) => {
                            crate::apply_change(
                                engine.as_ref(),
                                &dir,
                                &crate::Change::CreateDisk {
                                    size_mib: u64::from(*gb) * 1024,
                                },
                            )
                            .map_err(CoreError::machine)?;
                            Ok(())
                        }
                        // A cancelled copy leaves the machine without a disk.
                        Some(SystemDisk::Image {
                            file,
                            copy: true,
                            overlay,
                        }) => {
                            let copied = super::disks::copy_into(file, &dir, control)?;
                            crate::apply_change(
                                engine.as_ref(),
                                &dir,
                                &crate::Change::AttachDrive(attach_request(&copied, 1, *overlay)),
                            )
                            .map_err(CoreError::machine)?;
                            Ok(())
                        }
                        _ => Ok(()),
                    }
                })();
                if let Err(error) = disk {
                    if error.kind == ErrorKind::Cancelled {
                        control.kept(
                            dir.canonicalize()
                                .unwrap_or_else(|_| dir.clone())
                                .to_string_lossy()
                                .into_owned(),
                        );
                    } else {
                        let _ = std::fs::remove_dir_all(&dir);
                    }
                    inner.slot.raise(&[EventKind::Library]);
                    return Err(error);
                }
                inner.slot.raise(&[EventKind::Library]);
                let key = dir.canonicalize().unwrap_or(dir);
                Ok(Some(key.to_string_lossy().into_owned()))
            }))
    }

    /// The directory a key from [`Core::list_machines`], a path or a name under the root names.
    fn resolve(&self, key: &str) -> CoreResult<PathBuf> {
        let given = Path::new(key);
        let path = if given.is_absolute() || given.exists() {
            given.to_path_buf()
        } else {
            self.inner.root().join(given)
        };
        path.canonicalize()
            .map_err(|_| CoreError::new(ErrorKind::NotFound, format!("there is no machine {key}")))
    }

    /// Upgrade the stopped machine at `key` as `origami upgrade` does, on the
    /// engine its file names or else the first. May download: off the UI thread.
    pub fn upgrade_machine(&self, key: &str) -> CoreResult<Report> {
        self.upgrade_machine_with(key, None, None)
    }

    /// As [`Core::upgrade_machine`], with firmware files the person chose
    /// instead of a download: the boot PROM and the IO PROM.
    pub fn upgrade_machine_with(
        &self,
        key: &str,
        prom: Option<&Path>,
        io_prom: Option<&Path>,
    ) -> CoreResult<Report> {
        let path = self.resolve(key)?;
        // The CLI's choice: the engine the file names, else the first.
        let named = origami_machine::read_machine(&path)
            .ok()
            .map(|file| file.engine.id);
        let engine = self
            .inner
            .engines
            .iter()
            .find(|e| Some(e.info().id) == named)
            .unwrap_or(&self.inner.engines[0]);
        let done = crate::upgrade(engine.as_ref(), &path, prom, io_prom);
        // A failure after the file was rewritten still changes the listing.
        self.inner.slot.raise(&[EventKind::Library]);
        done.map_err(|incomplete| {
            let error = super::error::plain_upgrade(
                &entry_name(&path),
                CoreError::machine(incomplete.error),
            );
            // The notes so far, for a failure after the rewrite.
            let notes: Vec<_> = incomplete
                .report
                .lines
                .iter()
                .map(|l| l.to_string())
                .collect();
            if notes.is_empty() {
                error
            } else {
                error.with_detail(notes.join("; "))
            }
        })
    }

    /// The library entry for the machine at `key`, its real folder, and what a
    /// removal would leave behind. Only a machine the library lists, never a
    /// folder a path happens to name.
    fn removal(&self, key: &str) -> CoreResult<(PathBuf, PathBuf, Removed)> {
        let target = self.resolve(key)?;
        let entry = std::fs::read_dir(self.inner.root())
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .find(|p| {
                p.join("machine.toml").is_file()
                    && p.canonicalize().is_ok_and(|real| real == target)
            })
            .ok_or_else(|| {
                CoreError::new(ErrorKind::NotFound, format!("there is no machine {key}"))
            })?;
        let mut disk_left = Vec::new();
        for drive in origami_machine::read_machine(&target).map_or(vec![], |f| f.drive) {
            let image = origami_machine::resolve(&target, &drive.image);
            let image = dunce::canonicalize(&image).unwrap_or(image);
            let image = image.to_string_lossy().into_owned();
            if !Path::new(&image).starts_with(&target) && !disk_left.contains(&image) {
                disk_left.push(image);
            }
        }
        let name = entry_name(&entry);
        Ok((entry, target, Removed { name, disk_left }))
    }

    /// What [`Core::remove_machine`] would move and leave, for the alert that
    /// asks first. Changes nothing.
    pub fn remove_plan(&self, key: &str) -> CoreResult<Removed> {
        self.removal(key).map(|(_, _, plan)| plan)
    }

    /// Move the stopped machine at `key` to the platform's trash: its library
    /// entry (the link, for a symlinked folder) and everything in it. Disk
    /// images used in place outside the folder stay, and are reported.
    pub fn remove_machine(&self, key: &str) -> CoreResult<Removed> {
        let (entry, target, removed) = self.removal(key)?;
        let name = &removed.name;
        let held = self
            .inner
            .machines
            .lock()
            .unwrap()
            .get(&target)
            .map(|m| m.view().state);
        let running = !matches!(
            held,
            None | Some(MachineState::NotRunning | MachineState::Exited)
        ) || left_running(&target);
        if running {
            return Err(CoreError::new(
                ErrorKind::Busy,
                format!("{name} is running. Power off to remove it."),
            ));
        }
        (self.inner.config.trash)(&entry).map_err(|error| {
            CoreError::new(
                ErrorKind::Io,
                format!("{name} could not be moved to the trash."),
            )
            .with_detail(error)
        })?;
        self.inner.machines.lock().unwrap().remove(&target);
        self.inner.slot.raise(&[EventKind::Library]);
        Ok(removed)
    }

    /// The machine at `key`: a key from [`Core::list_machines`], a directory
    /// path, or a name under the library root. The same machine opened twice
    /// is the same machine.
    pub fn open_machine(&self, key: &str) -> CoreResult<Machine> {
        let path = self.resolve(key)?;
        if let Some(machine) = self.inner.machines.lock().unwrap().get(&path) {
            return Ok(machine.clone());
        }
        if is_legacy(&path) {
            return Err(CoreError::new(
                ErrorKind::NeedsUpgrade,
                format!(
                    "{} is from an older Origami; upgrade it once with origami upgrade {}",
                    path.display(),
                    path.display()
                ),
            ));
        }
        let file = origami_machine::read_machine(&path).map_err(CoreError::machine)?;
        let (index, mut machine_model) = self.inner.model_of(&file)?;
        let engine = self.inner.engines[index].clone();
        machine_model.engine.id = engine.info().id;
        let key = path.to_string_lossy().into_owned();
        let machine = Machine::new(
            key.clone(),
            MachineDir::new(&path),
            machine_model,
            engine,
            Shared {
                slot: self.inner.dispatcher.slot(Some(key)),
                next_operation: self.inner.next_operation.clone(),
                ring_bytes: self.inner.config.console_ring_bytes,
                stop_patience: self.inner.config.stop_patience,
            },
        );
        // Two threads may open at once; the first one in wins.
        let mut machines = self.inner.machines.lock().unwrap();
        Ok(machines.entry(path).or_insert(machine).clone())
    }

    /// Open `key` and start it, waiting until it runs. Blocks: not for a UI
    /// thread; [`Machine::start`] is the non-blocking form.
    pub fn start_machine(&self, key: &str) -> CoreResult<Machine> {
        let machine = self.open_machine(key)?;
        match machine.start()?.wait(None) {
            super::operation::OperationState::Failed(error) => Err(error),
            super::operation::OperationState::Cancelled => Err(CoreError::new(
                ErrorKind::Cancelled,
                "the start was cancelled",
            )),
            _ => Ok(machine),
        }
    }

    pub(crate) fn instigator(&self) -> CoreResult<PathBuf> {
        self.inner.config.instigator.clone().ok_or_else(|| {
            CoreError::new(
                ErrorKind::Unsupported,
                "This copy of Origami has no install server.",
            )
        })
    }

    pub(crate) fn halted(&self) -> Arc<Mutex<std::collections::HashSet<String>>> {
        self.inner.halted.clone()
    }

    /// Run `work` with no install operation live on `key`, and none able to
    /// start meanwhile.
    pub(crate) fn while_no_install<T>(
        &self,
        key: &str,
        work: impl FnOnce() -> CoreResult<T>,
    ) -> CoreResult<T> {
        let mut installs = self.inner.installs.lock().unwrap();
        installs.retain(|install| !install.state().is_finished());
        if installs.iter().any(|op| op.machine() == Some(key)) {
            return Err(CoreError::new(
                ErrorKind::Busy,
                "stop the install server before changing the install setup",
            ));
        }
        work()
    }

    /// Start an install operation on `machine` and remember it so dropping
    /// the core cancels it; refused while another of the kind runs there.
    /// The check and the registration share one lock.
    pub(crate) fn install_operation<F>(
        &self,
        machine: &Machine,
        title: &str,
        work: F,
    ) -> CoreResult<Operation>
    where
        F: FnOnce(&operation::Control) -> CoreResult<Option<String>> + Send + 'static,
    {
        let mut installs = self.inner.installs.lock().unwrap();
        installs.retain(|install| !install.state().is_finished());
        // A second ask for what already runs is the same work, not a failure.
        if let Some(running) = installs
            .iter()
            .find(|op| op.title() == title && op.machine() == Some(machine.key()))
        {
            return Ok(running.clone());
        }
        let operation = self.inner.operation(
            title,
            Some(machine.key().to_owned()),
            true,
            machine.slot(),
            work,
        );
        installs.push(operation.clone());
        Ok(operation)
    }

    /// The menu and toolbar commands.
    pub fn commands(&self) -> Vec<Command> {
        commands::table()
    }
}

/// Another process's runner holds the lock and has a run record; an edit
/// holds the lock with no record.
fn left_running(dir: &Path) -> bool {
    origami_machine::control::is_locked(dir).unwrap_or(false)
        && origami_machine::control::read(dir).is_ok()
}

/// A format 4 file reads, but its machine should be upgraded first.
fn is_legacy(dir: &Path) -> bool {
    std::fs::read_to_string(dir.join("machine.toml"))
        .ok()
        .and_then(|text| origami_machine::migrate::format(&text))
        == Some(i64::from(origami_machine::FORMAT_WITH_QEMU_TABLE))
}

/// The disk a machine boots from: the one on target 1, else the first disk.
pub(crate) fn system_drive(file: &origami_machine::MachineFile) -> Option<&origami_machine::Drive> {
    let disks = || file.drive.iter().filter(|d| d.kind == "disk");
    disks()
        .find(|d| d.bus == 0 && d.target == 1)
        .or_else(|| disks().next())
}

fn entry_name(path: &Path) -> String {
    path.file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned())
}

impl CoreInner {
    fn root(&self) -> PathBuf {
        self.root.read().unwrap().clone()
    }

    fn catalogue(&self, index: usize) -> CoreResult<Arc<Catalogue>> {
        let mut cache = self.catalogues.lock().unwrap();
        if let Some(found) = &cache[index] {
            return Ok(found.clone());
        }
        // The first call may start a program, so do not hold the lock for it.
        drop(cache);
        let loaded = self.engines[index].catalogue().map_err(CoreError::engine)?;
        cache = self.catalogues.lock().unwrap();
        Ok(cache[index].get_or_insert(loaded).clone())
    }

    pub(super) fn find_offering(
        &self,
        id: &str,
        engine: Option<&str>,
    ) -> CoreResult<(usize, origami_machine::Offering)> {
        let indexes: Vec<usize> = match engine {
            None => (0..self.engines.len()).collect(),
            Some(wanted) => match self.engines.iter().position(|e| e.info().id == wanted) {
                Some(index) => vec![index],
                None => {
                    return Err(CoreError::new(
                        ErrorKind::NotFound,
                        format!("there is no engine {wanted}"),
                    )
                    .with_detail("engine"))
                }
            },
        };
        for index in indexes {
            if let Some(o) = self.catalogue(index)?.offerings.iter().find(|o| o.id == id) {
                return Ok((index, o.clone()));
            }
        }
        let message = match engine {
            Some(engine) => format!("{engine} does not offer {id}"),
            None => format!("no engine offers {id}"),
        };
        Err(CoreError::new(ErrorKind::NotFound, message).with_detail("offering"))
    }

    /// The model of `file` and the index of the first engine that offers it.
    fn model_of(&self, file: &origami_machine::MachineFile) -> CoreResult<(usize, model::Machine)> {
        let mut last = None;
        // The engine the file names goes first: two engines may list the
        // same product and topology.
        let mut order: Vec<usize> = (0..self.engines.len()).collect();
        order.sort_by_key(|&index| self.engines[index].info().id != file.engine.id);
        for index in order {
            let catalogue = self.catalogue(index)?;
            match model::Machine::from_file(&catalogue, file) {
                Ok(machine) => return Ok((index, machine)),
                Err(error) => last = Some(CoreError::machine(error)),
            }
        }
        Err(last.expect("a core has an engine"))
    }

    fn operation<F>(
        &self,
        title: &str,
        machine: Option<String>,
        cancellable: bool,
        slot: Arc<Slot>,
        work: F,
    ) -> Operation
    where
        F: FnOnce(&operation::Control) -> CoreResult<Option<String>> + Send + 'static,
    {
        let id = self
            .next_operation
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        operation::spawn(id, title, machine, cancellable, slot, work)
    }
}
