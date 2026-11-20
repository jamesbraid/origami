//! The entry point: a [`Core`] owns the engines it was given, the event
//! dispatcher and the machines opened through it.

use super::commands::{self, Command};
use super::error::{CoreError, CoreResult, ErrorKind};
use super::event::{Dispatcher, EventKind, Observer, Slot};
use super::machine::{Machine, MachineState, Shared};
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
}

impl Config {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            console_ring_bytes: 1 << 20,
            stop_patience: Duration::from_secs(30),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct EngineSummary {
    pub id: String,
    pub version: Option<String>,
    pub capabilities: Capabilities,
}

/// A machine an engine offers to create, with nothing engine-specific.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OfferingSummary {
    pub id: String,
    pub engine: String,
    pub product: String,
    pub topology: String,
    /// What a person calls the topology, such as `Origin 200`.
    pub topology_name: String,
    /// Whether a machine made with the defaults has a graphics head, as
    /// against a serial console only.
    pub has_display: bool,
    pub nodes: u32,
    pub cpus: u32,
    pub memory_default_mib: u32,
    pub memory_accepted_mib: Vec<u32>,
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
    /// Whether the machine has a graphics head, from its file.
    pub has_display: bool,
    pub nodes: u32,
    pub memory_per_node_mib: u32,
    /// Known only for a machine opened through this core.
    pub state: MachineState,
    /// Why the machine file cannot be used, when it cannot.
    pub problem: Option<String>,
    /// Made by an older Origami: [`Core::upgrade_machine`] first, then it opens.
    pub needs_upgrade: bool,
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
    inner: Arc<CoreInner>,
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
                    id: info.id,
                    version: info.version,
                    capabilities: info.capabilities,
                }
            })
            .collect()
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
                    engine: id.clone(),
                    product: o.product.clone(),
                    topology: o.topology.clone(),
                    topology_name: origami_machine::listing::machine_name(&o.topology).into(),
                    has_display,
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
            let state = self
                .inner
                .machines
                .lock()
                .unwrap()
                .get(&key)
                .map_or(MachineState::NotRunning, |m| m.view().state);
            let state = if state == MachineState::NotRunning && left_running(&path) {
                MachineState::LeftRunning
            } else {
                state
            };
            let key = key.to_string_lossy().into_owned();
            let legacy = is_legacy(&path);
            out.push(match origami_machine::read_machine(&path) {
                Ok(file) => MachineSummary {
                    key,
                    name,
                    product_label: origami_machine::product_label(
                        &file.machine.model,
                        file.machine.topology.as_deref(),
                    ),
                    product: file.machine.model,
                    topology_name: file
                        .machine
                        .topology
                        .as_deref()
                        .map(|t| origami_machine::listing::machine_name(t).into()),
                    has_display: !file.pipe.is_empty(),
                    topology: file.machine.topology,
                    nodes: file.machine.nodes,
                    memory_per_node_mib: file
                        .machine
                        .memory_per_node
                        .trim_end_matches("MiB")
                        .parse()
                        .unwrap_or(0),
                    state,
                    problem: None,
                    needs_upgrade: legacy,
                },
                Err(error) => {
                    let error = CoreError::machine(error);
                    MachineSummary {
                        key,
                        name,
                        product: String::new(),
                        product_label: String::new(),
                        topology: None,
                        topology_name: None,
                        has_display: false,
                        nodes: 0,
                        memory_per_node_mib: 0,
                        state,
                        needs_upgrade: error.kind == ErrorKind::NeedsUpgrade,
                        problem: Some(error.message),
                    }
                }
            });
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
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
                // The same create the CLI runs, so graphics, the serial
                // console of a headless machine, identity and settings agree.
                let machines = inner.catalogue(index)?;
                let mut spec = crate::MachineSpec::new(&dir, preset_of(&machines, &offering));
                spec.memory_per_node_mib = request.memory_per_node_mib;
                spec.graphics.product = request.graphics.clone();
                spec.firmware = crate::FirmwareRequest {
                    boot_prom: request.boot_prom.clone(),
                    io_prom: request.io_prom.clone(),
                };
                crate::create::create_from(engine.as_ref(), &machines, &spec, &offering)
                    .map_err(CoreError::machine)?;
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
        let done = crate::upgrade(engine.as_ref(), &path, None, None);
        // A failure after the file was rewritten still changes the listing.
        self.inner.slot.raise(&[EventKind::Library]);
        done.map_err(|incomplete| {
            let error = CoreError::machine(incomplete.error);
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

    /// Start an install operation on `machine`, and remember it so
    /// dropping the core cancels it.
    pub(crate) fn install_operation<F>(&self, machine: &Machine, title: &str, work: F) -> Operation
    where
        F: FnOnce(&operation::Control) -> CoreResult<Option<String>> + Send + 'static,
    {
        let operation = self.inner.operation(
            title,
            Some(machine.key().to_owned()),
            true,
            machine.slot(),
            work,
        );
        let mut installs = self.inner.installs.lock().unwrap();
        installs.retain(|install| !install.state().is_finished());
        installs.push(operation.clone());
        operation
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

    fn find_offering(
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
