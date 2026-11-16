//! The install operations that only read or write a machine's records.
//! Applying an install and serving its media stream to the terminal and stay
//! with the front end.

use crate::Report;
use origami_engine::Engine;
use origami_machine::model::Machine;
use origami_machine::{control, install, read_machine, Result};
use std::path::{Path, PathBuf};

/// What `install-init` configures.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstallInit {
    /// Local media in place of the public archive.
    pub media_root: Option<PathBuf>,
    /// The machine's MAC, when it has none yet.
    pub mac: Option<String>,
    pub release: String,
    pub profile: String,
}

/// An add-on to configure: from `source`, or one Origami knows by name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstallAddon {
    pub name: String,
    pub source: Option<String>,
    pub install: Vec<String>,
    pub base: Option<String>,
    pub dist: Option<String>,
    /// Local media for a named add-on.
    pub media_root: Option<PathBuf>,
}

/// Configure install media and private networking for the machine in `dir`.
pub fn install_init(engine: &dyn Engine, dir: &Path, init: &InstallInit) -> Result<Report> {
    let _lock = control::lock_for_edit(dir, "changing its network")?;
    let mut file = read_machine(dir)?;
    let catalogue = engine.catalogue()?;
    engine.check_options(&Machine::from_file(&catalogue, &file)?)?;
    let initialised = if let Some(root) = &init.media_root {
        install::init_profile(
            &catalogue,
            dir,
            root,
            init.mac.as_deref(),
            &mut file,
            &init.release,
            &init.profile,
        )?
    } else {
        install::init_remote_profile(
            &catalogue,
            dir,
            init.mac.as_deref(),
            &mut file,
            &init.release,
            &init.profile,
        )?
    };
    let mut report = Report::default();
    if let Some(mac) = &initialised.assigned_mac {
        report.result(format!("assigned MAC {mac}"));
    }
    report.result(format!("created {}", initialised.manifest.display()));
    if install::needs_rad4_driver(&file) {
        report.detail("configured add-on rad4 for the RAD4 graphics");
    }
    Ok(report)
}

/// Add an optional package source to the installation of the machine in
/// `dir`.
pub fn install_addon(engine: &dyn Engine, dir: &Path, addon: &InstallAddon) -> Result<Report> {
    let _lock = control::lock_for_edit(dir, "changing its install add-ons")?;
    let file = read_machine(dir)?;
    crate::check(engine, dir, &file)?;
    let path = match &addon.source {
        Some(source) => install::add_addon(
            dir,
            &addon.name,
            source,
            addon.base.as_deref(),
            addon.dist.as_deref(),
            &addon.install,
        )?,
        None => install::add_known_addon(dir, &addon.name, addon.media_root.as_deref())?,
    };
    let mut report = Report::default();
    report.result(format!(
        "configured add-on {} in {}",
        addon.name,
        path.display()
    ));
    Ok(report)
}

/// Check the install media of the machine in `dir` with Instigator, found
/// by `instigator` once the machine has passed its own checks.
pub fn install_check(
    engine: &dyn Engine,
    dir: &Path,
    full: bool,
    instigator: impl FnOnce() -> Result<PathBuf>,
) -> Result<Report> {
    let loaded = crate::open(engine, dir)?;
    install::check(instigator, dir, &loaded.file, full)?;
    let mut report = Report::default();
    report.result("install media validated");
    Ok(report)
}
