//! The emu-id of an emulated machine, and the check that no two machines side
//! by side share one.
//!
//! QEMU's `emu-id` machine property, 0 to 4095, mints the machine's system
//! serial text, module serials and station address, so it is what makes a
//! guest's host id its own. It is not QEMU's `serial` property, which is the
//! system serial as text. `create` takes the next free emu-id from the
//! machines beside the new one, the way `mac` finds a free address, and
//! records it in the machine's `[identity]`. A copied machine directory keeps
//! its emu-id.

use crate::{mac, offering, MachineFile, Machines, Offering, Result};
use std::fs;
use std::path::{Path, PathBuf};

/// The largest emu-id QEMU takes. 0 means "not configured" and is never
/// recorded.
pub const MAX: u32 = 4095;

/// The last emu-id `create` hands out by itself. The rest, up to [`MAX`], are
/// kept for the test harness and CI guests, which ask for theirs with
/// `--emu-id`.
pub const LAST_MINTED: u32 = 2999;

/// Refuse a `--emu-id` the CLI does not hand out: 1 to [`LAST_MINTED`] only.
/// 0 means "not configured" and is valid only in a hand-edited machine.toml.
pub fn check_range(emu_id: u32) -> Result<()> {
    match emu_id {
        1..=LAST_MINTED => Ok(()),
        s if s <= MAX => Err(format!(
            "--emu-id {s} is reserved for harness and CI machines; choose from 1 to {LAST_MINTED}"
        )
        .into()),
        _ => Err(format!("--emu-id must be from 1 to {LAST_MINTED}; 0 means no emu-id").into()),
    }
}

/// Hold the exclusive lock that serializes emu-id minting among the machines
/// in `dir`'s parent directory. A second `create`, `network-set`,
/// `install-init` or `upgrade` in that directory waits here, so it rescans the
/// siblings after the first has written its machine.toml. Drop the returned
/// file to release it.
pub fn lock(dir: &Path) -> Result<fs::File> {
    let parent = dir
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent)?;
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(parent.join(".origami-serials.lock"))?;
    file.lock()?;
    Ok(file)
}

/// A warning for each running machine beside `dir` that holds the same
/// non-zero emu-id as `file`. Starting is allowed: a copied directory keeps
/// its emu-id, and the copy may be wanted on a private network. But the two
/// guests then share a host id and an Ethernet address, which clash on a
/// network they share. A running machine holds its edit lock for its whole
/// life, the probe `origami status` uses.
pub fn running_twin_warnings(dir: &Path, file: &MachineFile) -> Vec<String> {
    let Some(emu_id) = file
        .identity
        .as_ref()
        .and_then(|id| id.emu_id)
        .filter(|emu_id| *emu_id != 0)
    else {
        return vec![];
    };
    taken(dir)
        .into_iter()
        .filter(|(other, held)| {
            *held == emu_id && crate::control::is_locked(other).unwrap_or(false)
        })
        .map(|(other, _)| {
            format!(
                "emu-id {emu_id} is also the emu-id of the running machine in {}, so the two \
                 have the same host id and Ethernet address, which clash on a network they \
                 share. Give this one its own emu-id in [identity] of {}",
                other.display(),
                dir.join("machine.toml").display()
            )
        })
        .collect()
}

/// A warning for a machine with emu-id 0 on a network other machines share.
pub fn unconfigured_warnings(file: &MachineFile) -> Vec<String> {
    let zero = file.identity.as_ref().and_then(|id| id.emu_id) == Some(0);
    if zero && matches!(file.network.mode.as_str(), "tap" | "vmnet") {
        vec![format!(
            "emu-id 0 means no emu-id is configured, so on {} networking this machine's \
             address and host id may collide with other emulated machines on the LAN",
            file.network.mode
        )]
    } else {
        vec![]
    }
}

/// The emu-id the machine in `dir` records, when it has a readable one.
fn emu_id_of(dir: &Path) -> Option<u32> {
    let text = fs::read_to_string(dir.join("machine.toml")).ok()?;
    let value: toml::Table = text.parse().ok()?;
    let emu_id = value.get("identity")?.get("emu-id")?.as_integer()?;
    u32::try_from(emu_id).ok()
}

/// The emu-id each machine beside `dir` holds.
pub fn taken(dir: &Path) -> Vec<(PathBuf, u32)> {
    let mut found: Vec<_> = mac::siblings(dir)
        .into_iter()
        .filter_map(|path| emu_id_of(&path).map(|emu_id| (path, emu_id)))
        .collect();
    found.sort();
    found
}

/// Refuse `emu-id` when a neighbouring machine holds it.
pub fn check_free(dir: &Path, emu_id: u32) -> Result<()> {
    match taken(dir).iter().find(|(_, other)| *other == emu_id) {
        Some((other, _)) => Err(format!(
            "emu-id {emu_id} is already the emu-id of the machine in {}. Machines need different \
             emu-ids; choose another with --emu-id",
            other.display()
        )
        .into()),
        None => Ok(()),
    }
}

/// The emu-id a new machine in `dir` gets: the one asked for, refused when
/// a neighbour holds it, else the lowest in 1 to [`LAST_MINTED`] that no
/// neighbour holds. With `derive_mac` it must also give a MAC no neighbour
/// uses, so a neighbour with an old random address in the same block does
/// not stop a create.
pub fn assign(
    machines: &Machines,
    dir: &Path,
    offering: &Offering,
    requested: Option<u32>,
    derive_mac: bool,
) -> Result<u32> {
    if let Some(emu_id) = requested {
        check_range(emu_id)?;
        check_free(dir, emu_id)?;
        return Ok(emu_id);
    }
    let held: std::collections::HashSet<u32> = taken(dir).into_iter().map(|(_, s)| s).collect();
    let claims = mac::claims(machines, dir);
    for emu_id in (1..=LAST_MINTED).filter(|emu_id| !held.contains(emu_id)) {
        if derive_mac {
            // A machine that cannot take a derived MAC fails the same way
            // for every emu-id, so say why now.
            let mac = mac::from_emu_id(offering, emu_id)?;
            if mac::check_claims(&claims, mac::span(offering), &mac).is_err() {
                continue;
            }
        }
        return Ok(emu_id);
    }
    Err(format!(
        "emu-ids 1 to {LAST_MINTED} are all in use beside this machine; choose one with --emu-id"
    )
    .into())
}

/// One warning for each neighbouring machine that holds `file`'s emu-id.
pub fn duplicate_warnings(dir: &Path, file: &MachineFile) -> Vec<String> {
    // 0 means no emu-id, so two machines at 0 do not hold the same one.
    let Some(emu_id) = file
        .identity
        .as_ref()
        .and_then(|id| id.emu_id)
        .filter(|emu_id| *emu_id != 0)
    else {
        return vec![];
    };
    taken(dir)
        .iter()
        .filter(|(_, other)| *other == emu_id)
        .map(|(other, _)| {
            format!(
                "this machine's emu-id {emu_id} is also the emu-id of the machine in {}",
                other.display()
            )
        })
        .collect()
}

/// Whether the host id of `offering` comes from its station address rather
/// than a serial text: the Octane, Octane2, Fuel, Origin 200 and O2. Their
/// guests read the last four bytes of the address, so a kept MAC keeps an
/// unmarked host id.
pub fn host_id_follows_mac(offering: &Offering) -> bool {
    matches!(offering.hostid_source.as_str(), "address" | "o2")
}

/// Record an emu-id for the stopped machine in `dir` when it has none, keeping
/// its MAC. Returns what the upgrade tells the user, or None when the
/// machine has an emu-id already or takes no identity.
pub fn add_to_machine(dir: &Path, machines: &Machines) -> Result<Option<Vec<String>>> {
    let _lock = crate::control::lock_for_edit(dir, "adding an emu-id")?;
    let _emu_ids = lock(dir)?;
    let path = dir.join("machine.toml");
    let mut file = crate::read_machine(dir)?;
    let Some(identity) = file.identity.as_mut().filter(|id| id.emu_id.is_none()) else {
        return Ok(None);
    };
    let offer = offering(machines, &file.machine)?;
    let emu_id = assign(machines, dir, offer, None, false)?;
    identity.emu_id = Some(emu_id);
    let mut notes = vec![format!("emu-id {emu_id} added")];
    if host_id_follows_mac(offer) {
        notes.push(
            "its MAC was kept, so its host id, which comes from the station address, stays \
             unmarked"
                .into(),
        );
    } else {
        // Before emu-ids QEMU built the serial text from emu-id 0.
        notes.push(format!(
            "its host id comes from a system serial that the emu-id sets, so the guest sees a \
             new host id at its next boot; to keep the old one, set emu-id = 0 in [identity] of \
             {} (unless the machine sets serial or hostid)",
            path.display()
        ));
    }
    let temporary = dir.join("machine.toml.upgrade");
    fs::write(&temporary, toml::to_string_pretty(&file)?)?;
    fs::rename(temporary, path)?;
    Ok(Some(notes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{preset, test_machines};

    fn sibling(root: &Path, name: &str, emu_id: u32) {
        let dir = root.join(name);
        fs::create_dir(&dir).unwrap();
        fs::write(
            dir.join("machine.toml"),
            format!("[identity]\nmac = \"08:00:69:12:34:5{emu_id:x}\"\nemu-id = {emu_id}\n"),
        )
        .unwrap();
    }

    fn bare_file() -> MachineFile {
        toml::from_str(
            "format = 4\n[machine]\nmodel = \"origin200\"\nnodes = 1\ncpus_per_node = 1\n\
             memory_per_node = \"64MiB\"\n",
        )
        .unwrap()
    }

    fn with_emu_id(emu_id: u32, mode: &str) -> MachineFile {
        let mut file = bare_file();
        file.identity = Some(crate::Identity::new("08:00:69:12:34:56", Some(emu_id)));
        file.network.mode = mode.into();
        file
    }

    #[test]
    fn the_lowest_free_emu_id_is_minted_past_what_siblings_hold() {
        let root = tempfile::tempdir().unwrap();
        let machines = test_machines();
        let offer = preset(&machines, "origin200-1").unwrap();
        let new = root.path().join("new");
        assert_eq!(assign(&machines, &new, offer, None, true).unwrap(), 1);
        sibling(root.path(), "a", 1);
        sibling(root.path(), "b", 2);
        sibling(root.path(), "c", 4);
        assert_eq!(assign(&machines, &new, offer, None, true).unwrap(), 3);
    }

    #[test]
    fn a_sibling_with_the_derived_mac_makes_minting_skip_that_emu_id() {
        let root = tempfile::tempdir().unwrap();
        let machines = test_machines();
        let offer = preset(&machines, "origin200-1").unwrap();
        let dir = root.path().join("x");
        fs::create_dir(&dir).unwrap();
        // An old machine whose random MAC is emu-id 1's derived one.
        fs::write(
            dir.join("machine.toml"),
            "[identity]\nmac = \"08:00:69:e1:00:10\"\n",
        )
        .unwrap();
        let new = root.path().join("new");
        assert_eq!(assign(&machines, &new, offer, None, true).unwrap(), 2);
        assert_eq!(assign(&machines, &new, offer, None, false).unwrap(), 1);
    }

    #[test]
    fn explicit_emu_ids_are_range_checked_and_refused_when_held() {
        let root = tempfile::tempdir().unwrap();
        let machines = test_machines();
        let offer = preset(&machines, "origin200-1").unwrap();
        let new = root.path().join("new");
        sibling(root.path(), "a", 7);
        assert_eq!(
            assign(&machines, &new, offer, Some(2999), true).unwrap(),
            2999
        );
        for bad in [0, 4096] {
            assert!(assign(&machines, &new, offer, Some(bad), true).is_err());
        }
        for reserved in [3000, 4095] {
            let error = assign(&machines, &new, offer, Some(reserved), true)
                .unwrap_err()
                .to_string();
            assert!(
                error.contains("reserved for harness and CI machines"),
                "{error}"
            );
        }
        let error = assign(&machines, &new, offer, Some(7), true)
            .unwrap_err()
            .to_string();
        assert!(error.contains("emu-id 7 is already"), "{error}");
    }

    #[test]
    fn the_host_id_follows_the_mac_where_the_machine_list_says_the_address_is_its_source() {
        let machines = test_machines();
        for (name, follows) in [
            ("origin200-1", true),
            ("octane-impact", true),
            ("fuel-1", true),
            ("origin2000-8", false),
            ("origin3200", false),
            ("tezro", false),
        ] {
            assert_eq!(
                host_id_follows_mac(preset(&machines, name).unwrap()),
                follows,
                "{name}"
            );
        }
    }

    #[test]
    fn the_minting_lock_excludes_a_second_holder_until_dropped() {
        let root = tempfile::tempdir().unwrap();
        let first = lock(&root.path().join("a")).unwrap();
        let other = fs::OpenOptions::new()
            .write(true)
            .open(root.path().join(".origami-serials.lock"))
            .unwrap();
        assert!(other.try_lock().is_err());
        drop(first);
        assert!(other.try_lock().is_ok());
    }

    #[test]
    fn a_clone_warns_while_its_twin_runs_and_is_not_refused() {
        let root = tempfile::tempdir().unwrap();
        sibling(root.path(), "twin", 5);
        let dir = root.path().join("clone");
        let file = |emu_id: u32| with_emu_id(emu_id, "private");
        assert!(running_twin_warnings(&dir, &file(5)).is_empty());
        // The running twin holds its edit lock.
        let _running = crate::control::lock_for_edit(&root.path().join("twin"), "t").unwrap();
        let warnings = running_twin_warnings(&dir, &file(5));
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("emu-id 5 is also"), "{warnings:?}");
        assert!(warnings[0].contains("twin"), "{warnings:?}");
        assert!(warnings[0].contains("clash"), "{warnings:?}");
        assert!(running_twin_warnings(&dir, &file(6)).is_empty());
        assert!(running_twin_warnings(&dir, &file(0)).is_empty());
    }

    #[test]
    fn emu_id_zero_is_not_a_duplicate() {
        let root = tempfile::tempdir().unwrap();
        sibling(root.path(), "a", 0);
        sibling(root.path(), "b", 7);
        let dir = root.path().join("new");
        assert!(duplicate_warnings(&dir, &with_emu_id(0, "private")).is_empty());
        assert_eq!(
            duplicate_warnings(&dir, &with_emu_id(7, "private")).len(),
            1
        );
    }

    #[test]
    fn a_stopped_twin_does_not_warn() {
        let root = tempfile::tempdir().unwrap();
        sibling(root.path(), "twin", 5);
        let dir = root.path().join("clone");
        assert!(running_twin_warnings(&dir, &with_emu_id(5, "tap")).is_empty());
    }

    #[test]
    fn emu_id_zero_on_a_shared_lan_warns() {
        assert_eq!(unconfigured_warnings(&with_emu_id(0, "tap")).len(), 1);
        assert_eq!(unconfigured_warnings(&with_emu_id(0, "vmnet")).len(), 1);
        assert!(unconfigured_warnings(&with_emu_id(0, "private")).is_empty());
        assert!(unconfigured_warnings(&with_emu_id(7, "vmnet")).is_empty());
    }
}
