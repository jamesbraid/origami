//! A machine's stores: the files under `state/` that hold its flash and
//! NVRAM, and what an engine does to create them.

use crate::{Offering, Result, StorageItem};
use std::path::{Path, PathBuf};

/// What creating or rebuilding a machine's stores needs from its engine.
pub trait StorePreparer {
    /// The boot and IO PROM images that create `offering`'s stores: the
    /// files given, else verified downloads of the kinds the offering reads.
    fn firmware(
        &self,
        offering: &Offering,
        boot_prom: Option<&Path>,
        io_prom: Option<&Path>,
    ) -> Result<(PathBuf, Option<PathBuf>)>;

    /// A copy of `boot_prom` in the form the engine's creation step reads,
    /// for an offering whose published image is not that form; None when the
    /// image is used as it is.
    fn stage_boot_prom(
        &self,
        offering: &Offering,
        boot_prom: &Path,
    ) -> Result<Option<tempfile::TempPath>>;

    /// Write every store of `offering` into `state`, or none.
    fn create_stores(
        &self,
        offering: &Offering,
        boot_prom: &Path,
        io_prom: Option<&Path>,
        state: &Path,
    ) -> Result<()>;
}

pub fn state_path(dir: &Path, item: &StorageItem) -> PathBuf {
    dir.join("state").join(format!("{}.raw", item.name))
}

/// Origami never rebuilds missing storage: a flash file holds the guest's
/// own PROM updates and settings, which the original images cannot restore.
pub fn check_state(dir: &Path, offering: &Offering) -> Result<()> {
    for item in &offering.storage {
        let path = state_path(dir, item);
        if !path.is_file() {
            // A pipe's flash holds nothing a new one cannot start with.
            if item.initial == "kona-flash" {
                return Err(format!(
                    "missing machine state {}. Add it with origami upgrade {}",
                    path.display(),
                    dir.display()
                )
                .into());
            }
            return Err(format!(
                "missing machine state {}. Restore it from a backup, or create a new machine \
                 with origami create and attach this machine's disks",
                path.display()
            )
            .into());
        }
    }
    Ok(())
}
