//! Ready-made disks: which ones fit a machine, and the download behind Create.

use super::core::Core;
use super::disks::{free_space, size_label};
use super::error::{CoreError, CoreResult, ErrorKind};
use super::operation::Control;
use origami_machine::ready::{self, DownloadError, Event};
use std::path::PathBuf;

/// A disk the server offers for a machine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadyDisk {
    /// Pass to `SystemDisk::Ready`.
    pub name: String,
    /// The IRIX release.
    pub version: String,
    pub bytes: u64,
    /// Already downloaded, so Create needs no download.
    pub cached: bool,
    /// What to show in the list, such as `IRIX 6.5.22, 835 MB`.
    pub label: String,
}

/// A downloaded disk, for Preferences.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CachedDisk {
    /// Pass to `Core::remove_cached_disk`.
    pub key: String,
    /// What to call it, such as `IRIX 6.5.22 (ip30-uber-05d2f7a8.chd)`.
    pub label: String,
    pub bytes: u64,
}

/// Whether `disk` is for a machine of this product and topology. The board
/// matters only for the Indigo2, whose two boards share a product name.
fn fits(disk: &ready::Disk, product: &str, topology: &str) -> bool {
    disk.machines
        .iter()
        .any(|m| m.eq_ignore_ascii_case(product))
        && (product != "indigo2" || topology.to_ascii_lowercase().contains(&disk.board))
}

fn failure(error: DownloadError) -> CoreError {
    match error {
        DownloadError::Cancelled => {
            CoreError::new(ErrorKind::Cancelled, "the download was cancelled")
        }
        DownloadError::Failed(text) => CoreError::new(ErrorKind::Io, text),
    }
}

/// Whether the volumes of `a` and `b` are the same one, where that can be told.
fn same_volume(a: &std::path::Path, b: &std::path::Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        matches!((std::fs::metadata(a), std::fs::metadata(b)), (Ok(a), Ok(b)) if a.dev() == b.dev())
    }
    #[cfg(not(unix))]
    {
        let _ = (a, b);
        false
    }
}

/// The cached file for `name`, downloading and checking it first when needed,
/// and a hold on it that the caller keeps while the machine copies it. The
/// disk must fit the offering, and both volumes must have room before a byte
/// is fetched. `library` is where the machine's folder will be made.
pub(crate) fn acquire(
    name: &str,
    product: &str,
    topology: &str,
    library: &std::path::Path,
    control: &Control,
) -> CoreResult<(PathBuf, ready::Lock)> {
    let disk = ready::cached_index()
        .map_err(CoreError::machine)?
        .into_iter()
        .find(|d| d.name == name)
        .ok_or_else(|| CoreError::new(ErrorKind::NotFound, format!("no ready-made disk {name}")))?;
    if !fits(&disk, product, topology) {
        return Err(CoreError::new(
            ErrorKind::Invalid,
            "That disk is not for the machine you chose. Pick the disk again.",
        ));
    }
    let hold = |disk: &ready::Disk| ready::hold(disk).map_err(failure);
    if ready::cached_path(&disk).is_none() {
        let cache = ready::dir_of(&disk).map_err(CoreError::machine)?;
        std::fs::create_dir_all(&cache)?;
        let mut room = library.to_path_buf();
        while !room.exists() && room.pop() {}
        let (cache_free, library_free) = (free_space(&cache), free_space(&room));
        let (fetch, copy) = (ready::bytes_left(&disk), disk.size);
        let shared = same_volume(&cache, &room);
        if fetch > cache_free || (shared && fetch + copy > cache_free) {
            return Err(CoreError::new(
                ErrorKind::Io,
                format!(
                    "The download needs {} and the download folder has {} free",
                    size_label(if shared { fetch + copy } else { fetch }),
                    size_label(cache_free)
                ),
            ));
        }
        if copy > library_free {
            return Err(CoreError::new(
                ErrorKind::Io,
                format!(
                    "The machine's copy needs {} and the library folder has {} free",
                    size_label(copy),
                    size_label(library_free)
                ),
            ));
        }
        ready::download(&disk, &mut |event| {
            match event {
                Event::Downloading { done, total } => {
                    control.progress(format!(
                        "Downloading {} of {}",
                        size_label(done),
                        size_label(total)
                    ));
                    control.fraction(done as f32 / total.max(1) as f32);
                }
                Event::Checking => control.progress("Checking the download"),
            }
            !control.is_cancelled()
        })
        .map_err(failure)?;
    }
    let held = hold(&disk)?;
    let path = ready::cached_path(&disk).ok_or_else(|| {
        CoreError::new(
            ErrorKind::NotFound,
            "The downloaded disk is gone. Try again.",
        )
    })?;
    Ok((path, held))
}

impl Core {
    /// The disks for the offering, in the server's order. Asks the server, so
    /// not on a UI thread; the last list is used when it cannot be reached,
    /// and an error says there is none.
    pub fn ready_disks(&self, offering: &str) -> CoreResult<Vec<ReadyDisk>> {
        let (_, offering) = self.inner.find_offering(offering, None)?;
        Ok(ready::index()
            .map_err(CoreError::machine)?
            .iter()
            .filter(|d| fits(d, &offering.product, &offering.topology))
            .map(|d| {
                let cached = ready::cached_path(d).is_some();
                ReadyDisk {
                    name: d.name.clone(),
                    version: d.version.clone(),
                    bytes: d.size,
                    cached,
                    label: format!(
                        "IRIX {}, {}{}",
                        d.version,
                        size_label(d.size),
                        if cached { ", downloaded" } else { "" }
                    ),
                }
            })
            .collect())
    }

    pub fn cached_disks(&self) -> CoreResult<Vec<CachedDisk>> {
        let index = ready::cached_index().unwrap_or_default();
        Ok(ready::cached()
            .map_err(CoreError::machine)?
            .into_iter()
            .map(|c| {
                let label = match index.iter().find(|d| d.sha256 == c.key) {
                    Some(d) => format!("IRIX {} ({})", d.version, c.file),
                    None => c.file,
                };
                CachedDisk {
                    key: c.key,
                    label,
                    bytes: c.bytes,
                }
            })
            .collect())
    }

    pub fn remove_cached_disk(&self, key: &str) -> CoreResult<()> {
        ready::remove(key).map_err(CoreError::machine)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn disk(board: &str, machines: &[&str]) -> ready::Disk {
        ready::Disk {
            name: "irix/x.chd".into(),
            url: String::new(),
            sha256: String::new(),
            size: 1,
            version: "6.5.22".into(),
            board: board.into(),
            machines: machines.iter().map(|m| m.to_string()).collect(),
        }
    }

    #[test]
    fn a_disk_fits_its_products_and_an_indigo2_disk_its_board() {
        let ip30 = disk("ip30", &["octane2", "octane"]);
        assert!(fits(&ip30, "octane", "octane"));
        assert!(!fits(&ip30, "indy", "indy_ip24"));
        let ip28 = disk("ip28", &["indigo2"]);
        assert!(fits(&ip28, "indigo2", "indigo2_ip28"));
        assert!(!fits(&ip28, "indigo2", "indigo2_ip22"));
    }
}
