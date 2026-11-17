//! [`IrisEngine`]: iris machines, run by a runner process this engine starts.

use crate::catalogue::{self, catalogue};
use crate::session::{self, IrisSession};
use crate::ENGINE;
use origami_engine::{
    Capabilities, Catalogue, ConsoleTransport, Control, DiskFormat, DisplayKind, DisplayRequest,
    DriveFormats, DriveKind, Engine, EngineInfo, IdentityKind, LaunchPlan, MachineId, Report,
    Result, Session, Wake, Wiring,
};
use origami_machine::dir::MachineDir;
use origami_machine::model::Machine;
use origami_machine::stores::FirmwareSet;
use origami_machine::Offering;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

pub struct IrisEngine {
    /// The `origami` executable that hosts the machine: it is run as
    /// `_serve DIR`, and has iris linked in.
    runner: PathBuf,
    catalogue: OnceLock<Arc<Catalogue>>,
    next_id: AtomicU32,
    /// Holds the stand-in for the PROM image a create is handed: iris runs
    /// its own. Made when a create asks.
    embedded: Mutex<Option<tempfile::TempDir>>,
    /// Whether this process draws the display itself, and with what GPU
    /// import; otherwise the session hands out the runner's socket.
    attach: Mutex<Option<Option<Arc<dyn origami_display::Importer>>>>,
}

impl IrisEngine {
    pub fn new(runner: impl Into<PathBuf>) -> Self {
        Self {
            runner: runner.into(),
            catalogue: OnceLock::new(),
            next_id: AtomicU32::new(1),
            embedded: Mutex::new(None),
            attach: Mutex::new(None),
        }
    }

    /// Draw machines' displays in this process: sessions started after this
    /// hand out frames, taken into the GPU with `importer` where it can.
    pub fn attach_display(&self, importer: Option<Arc<dyn origami_display::Importer>>) {
        *self.attach.lock().unwrap() = Some(importer);
    }
}

/// What a launch needs, as the plan's payload.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct IrisPlan {
    pub runner: PathBuf,
    /// The machine as planned, in machine file form: the runner reads it,
    /// so a launch does not depend on what `machine.toml` says.
    pub machine: String,
    pub name: String,
    pub hold: bool,
    /// Drives that take media, by guest name.
    pub removable: Vec<String>,
    /// Publish the picture on a display protocol 4 socket.
    #[serde(default)]
    pub display: bool,
}

impl Engine for IrisEngine {
    fn info(&self) -> EngineInfo {
        let formats = |kind| DriveFormats {
            kind,
            formats: vec![DiskFormat::Raw, DiskFormat::Chd],
            preferred: DiskFormat::Raw,
        };
        EngineInfo {
            id: ENGINE.into(),
            version: None,
            capabilities: Capabilities {
                display: DisplayKind::Socket,
                max_heads: 1,
                gpu_regions: false,
                // iris models neither a power button nor an NMI.
                controls: vec![Control::Reset, Control::Pause, Control::Resume],
                disk_formats: vec![formats(DriveKind::Disk), formats(DriveKind::Cdrom)],
                console: ConsoleTransport::RawTcp,
                identity: IdentityKind::Iris,
                attach: false,
                audio: vec![],
            },
        }
    }

    fn catalogue(&self) -> Result<Arc<Catalogue>> {
        Ok(self.catalogue.get_or_init(|| Arc::new(catalogue())).clone())
    }

    fn firmware(
        &self,
        _offering: &Offering,
        boot_prom: Option<&Path>,
        io_prom: Option<&Path>,
    ) -> Result<FirmwareSet> {
        if boot_prom.is_some() || io_prom.is_some() {
            return Err(
                "iris machines run iris's built-in PROM; a PROM file is not supported".into(),
            );
        }
        let mut held = self.embedded.lock().unwrap();
        let dir = match &*held {
            Some(dir) => dir.path().to_path_buf(),
            None => held.insert(tempfile::tempdir()?).path().to_path_buf(),
        };
        let marker = dir.join("embedded-prom");
        fs::write(&marker, b"")?;
        Ok(FirmwareSet {
            boot_prom: marker,
            io_prom: None,
        })
    }

    fn validate(&self, m: &Machine, dir: &MachineDir) -> Report {
        let catalogue = match self.catalogue() {
            Ok(catalogue) => catalogue,
            Err(error) => return Report::error(error.to_string()),
        };
        if m.engine.id != ENGINE {
            return Report::error(format!("the machine is for {}, not iris", m.engine.id));
        }
        let file = m.to_file();
        let checked =
            origami_machine::validate(&catalogue, dir.path(), &file).and_then(|offering| {
                crate::config::check(&file)?;
                let data = catalogue::data(offering)?;
                let mib = m.memory.per_node_mib;
                catalogue::banks_for(mib, &data.bank_sizes, data.banks)
                    .map(|_| ())
                    .ok_or_else(|| format!("{mib} MiB cannot be made from iris's banks").into())
            });
        match checked {
            Ok(()) => Report::default(),
            Err(error) => Report::error(error.to_string()),
        }
    }

    fn prepare_stores(&self, o: &Offering, proms: &FirmwareSet, state: &Path) -> Result<()> {
        for prom in std::iter::once(&proms.boot_prom).chain(&proms.io_prom) {
            if !prom.is_file() {
                return Err(format!("cannot read {}", prom.display()).into());
            }
        }
        fs::create_dir(state)?;
        let result = o.storage.iter().try_for_each(|item| -> Result<()> {
            let bytes = match (item.initial.as_str(), crate::nvram::Kind::of(&item.initial)) {
                ("zero", _) => vec![0u8; item.size as usize],
                (_, Some(kind)) => kind.blank(),
                (other, None) => return Err(format!("iris cannot create a {other} store").into()),
            };
            fs::write(state.join(format!("{}.raw", item.name)), bytes)?;
            Ok(())
        });
        if let Err(error) = result {
            let _ = fs::remove_dir_all(state);
            return Err(error);
        }
        Ok(())
    }

    fn create_disk(&self, stem: &Path, size_mib: u64) -> Result<PathBuf> {
        let mut name = stem.as_os_str().to_owned();
        name.push(".");
        name.push(DiskFormat::Raw.extension());
        let path = PathBuf::from(name);
        let file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|error| format!("disk {}: {error}", path.display()))?;
        file.set_len(size_mib << 20)?;
        Ok(path)
    }

    fn plan(&self, m: &Machine, dir: &MachineDir, w: &Wiring) -> Result<LaunchPlan> {
        let mut file = m.to_file();
        // `console=d` for this launch: a new machine's PROM talks on the serial line.
        if w.once.iter().any(|o| o == "console=d") {
            file.firstboot
                .get_or_insert_with(Default::default)
                .nvram
                .insert("console".into(), "d".into());
        }
        let plan = IrisPlan {
            runner: self.runner.clone(),
            machine: toml::to_string_pretty(&file)?,
            name: w.name.clone(),
            hold: w.paused,
            removable: m
                .drives
                .iter()
                .filter(|drive| drive.kind == "cdrom")
                .map(|drive| drive.name.clone())
                .collect(),
            display: matches!(w.display, DisplayRequest::Shared { .. }),
        };
        Ok(LaunchPlan::new(
            ENGINE,
            dir.path().to_path_buf(),
            serde_json::to_value(plan)?,
        ))
    }

    fn launch(&self, plan: LaunchPlan, wake: Wake) -> Result<Arc<dyn Session>> {
        if plan.engine() != ENGINE {
            return Err(format!("a {} plan is not an iris plan", plan.engine()).into());
        }
        let spec: IrisPlan = serde_json::from_value(plan.payload().clone())?;
        let id = MachineId(self.next_id.fetch_add(1, Ordering::Relaxed));
        let attach = self.attach.lock().unwrap().clone();
        let session: Arc<IrisSession> = session::launch(id, plan.dir(), &spec, wake, attach)?;
        Ok(session)
    }
}
