//! Stand-in stores for the unit tests, which cannot reach an engine.

use crate::stores::StorePreparer;
use crate::{Offering, Result};
use std::fs;
use std::path::{Path, PathBuf};

/// Creates zero-filled storage for each of an offering's stores, or fails.
pub struct ZeroStores {
    pub fail: bool,
}

impl StorePreparer for ZeroStores {
    fn firmware(
        &self,
        _: &Offering,
        boot_prom: Option<&Path>,
        io_prom: Option<&Path>,
    ) -> Result<(PathBuf, Option<PathBuf>)> {
        Ok((
            boot_prom.ok_or("no boot PROM given")?.into(),
            io_prom.map(Into::into),
        ))
    }

    fn stage_boot_prom(&self, _: &Offering, _: &Path) -> Result<Option<tempfile::TempPath>> {
        Ok(None)
    }

    fn create_stores(
        &self,
        offering: &Offering,
        _: &Path,
        _: Option<&Path>,
        state: &Path,
    ) -> Result<()> {
        if self.fail {
            return Err("the init tool failed".into());
        }
        fs::create_dir(state)?;
        for item in &offering.storage {
            fs::File::create(state.join(format!("{}.raw", item.name)))?.set_len(item.size)?;
        }
        Ok(())
    }
}
