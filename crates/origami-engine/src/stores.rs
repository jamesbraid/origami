use crate::Engine;
use origami_machine::stores::{FirmwareSet, StorePreparer};
use origami_machine::{Offering, Result};
use std::path::{Path, PathBuf};

/// An engine as the machine library's [`StorePreparer`], so creating a
/// machine prepares its stores through the engine.
pub struct EngineStores<'a>(pub &'a dyn Engine);

impl StorePreparer for EngineStores<'_> {
    fn firmware(
        &self,
        offering: &Offering,
        boot_prom: Option<&Path>,
        io_prom: Option<&Path>,
    ) -> Result<(PathBuf, Option<PathBuf>)> {
        let set = self.0.firmware(offering, boot_prom, io_prom)?;
        Ok((set.boot_prom, set.io_prom))
    }

    /// The engine stages the image itself, inside `prepare_stores`.
    fn stage_boot_prom(
        &self,
        _offering: &Offering,
        _boot_prom: &Path,
    ) -> Result<Option<tempfile::TempPath>> {
        Ok(None)
    }

    fn create_stores(
        &self,
        offering: &Offering,
        boot_prom: &Path,
        io_prom: Option<&Path>,
        state: &Path,
    ) -> Result<()> {
        let proms = FirmwareSet {
            boot_prom: boot_prom.into(),
            io_prom: io_prom.map(Into::into),
        };
        self.0.prepare_stores(offering, &proms, state)
    }
}
