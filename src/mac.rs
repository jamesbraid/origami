//! Ethernet addresses for new machines, and the check that no two machines
//! side by side share one.
//!
//! A machine's identity MAC is also the base of any further onboard NIC the
//! machine list derives from it (the catalogue's `mac.offset`), so a machine
//! claims the whole run of addresses from its MAC to its MAC plus the
//! largest offset. Machines are compared when they sit in the same parent
//! directory, which is where `create` puts them, or in the working directory.

use crate::{offering, MachineFile, Machines, Offering, Result, SGI_MAC_PREFIX};
use std::fs;
use std::path::{Path, PathBuf};

/// The addresses one machine uses, as 48-bit numbers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Claim {
    pub dir: PathBuf,
    pub first: u64,
    pub last: u64,
}

impl Claim {
    fn overlaps(&self, first: u64, last: u64) -> bool {
        self.first <= last && first <= self.last
    }

    fn describe(&self) -> String {
        if self.first == self.last {
            format!("uses {}", text(self.first))
        } else {
            format!("uses {} to {}", text(self.first), text(self.last))
        }
    }
}

/// The largest offset the machine list derives a NIC at from the machine MAC.
pub fn span(offering: &Offering) -> u8 {
    offering
        .networks
        .iter()
        .filter_map(|adapter| adapter.mac.as_ref())
        .map(|mac| mac.offset)
        .max()
        .unwrap_or(0)
}

fn number(mac: &str) -> Option<u64> {
    if !crate::valid_mac(mac) {
        return None;
    }
    let bytes: Vec<u8> = mac
        .split(':')
        .map(|part| u8::from_str_radix(part, 16).unwrap())
        .collect();
    Some(
        bytes
            .iter()
            .fold(0, |sum, byte| sum << 8 | u64::from(*byte)),
    )
}

fn text(number: u64) -> String {
    let bytes = number.to_be_bytes();
    bytes[2..]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<Vec<_>>()
        .join(":")
}

/// What one sibling machine claims, or nothing when it has no readable MAC.
fn claim_of(machines: &Machines, dir: &Path) -> Option<Claim> {
    let text = fs::read_to_string(dir.join("machine.toml")).ok()?;
    let value: toml::Table = text.parse().ok()?;
    let mac = value.get("identity")?.get("mac")?.as_str()?;
    let first = number(mac)?;
    // A file that does not fit this release still claims its own address.
    let reach = toml::from_str::<MachineFile>(&text)
        .ok()
        .and_then(|file| offering(machines, &file.machine).ok().map(span))
        .unwrap_or(0);
    Some(Claim {
        dir: dir.to_path_buf(),
        first,
        last: first + u64::from(reach),
    })
}

/// The claims of every machine in the directories beside `dir`: its parent
/// and the working directory. Unreadable entries are skipped.
pub fn claims(machines: &Machines, dir: &Path) -> Vec<Claim> {
    let own = dir
        .canonicalize()
        .ok()
        .or_else(|| {
            let parent = dir.parent().filter(|p| !p.as_os_str().is_empty());
            let parent = parent.unwrap_or(Path::new(".")).canonicalize().ok()?;
            Some(parent.join(dir.file_name()?))
        })
        .unwrap_or_else(|| dir.to_path_buf());
    let mut scanned = Vec::new();
    let mut roots = Vec::new();
    if let Some(parent) = own.parent() {
        roots.push(parent.to_path_buf());
    }
    if let Some(cwd) = std::env::current_dir()
        .ok()
        .and_then(|cwd| cwd.canonicalize().ok())
    {
        roots.push(cwd);
    }
    roots.dedup();
    let mut found = Vec::new();
    for root in roots {
        let Ok(entries) = fs::read_dir(&root) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(path) = path.canonicalize() else {
                continue;
            };
            if path == own || scanned.contains(&path) || !path.is_dir() {
                continue;
            }
            scanned.push(path.clone());
            found.extend(claim_of(machines, &path));
        }
    }
    found.sort_by(|a, b| a.dir.cmp(&b.dir));
    found
}

fn conflict(claims: &[Claim], first: u64, last: u64) -> Option<&Claim> {
    claims.iter().find(|claim| claim.overlaps(first, last))
}

/// Refuse `mac` when a neighbouring machine already uses an address in the
/// run that `offering` would derive from it.
pub fn check_free(
    machines: &Machines,
    dir: &Path,
    offering: Option<&Offering>,
    mac: &str,
) -> Result<()> {
    let Some(first) = number(mac) else {
        return Ok(());
    };
    let last = first + u64::from(offering.map_or(0, span));
    let claims = claims(machines, dir);
    match conflict(&claims, first, last) {
        Some(other) => Err(format!(
            "{mac} collides with the machine in {}, which {}. Machines on one \
             network need different MACs; choose another with --mac",
            other.dir.display(),
            other.describe()
        )
        .into()),
        None => Ok(()),
    }
}

/// A fresh `08:00:69:XX:XX:XX` whose derived run overlaps no claim. The last
/// byte leaves room for the offering's offsets, so the run never wraps it.
pub fn generate(offering: &Offering, claims: &[Claim]) -> Result<String> {
    generate_from(offering, claims, || {
        let mut random = [0u8; 3];
        getrandom::fill(&mut random).map_err(|error| format!("no random bytes: {error}"))?;
        Ok(random)
    })
}

fn generate_from(
    offering: &Offering,
    claims: &[Claim],
    mut random: impl FnMut() -> Result<[u8; 3]>,
) -> Result<String> {
    let room = 256 - u16::from(span(offering));
    for _ in 0..256 {
        let random = random()?;
        let low = u8::try_from(u16::from(random[2]) % room).unwrap();
        let mac = format!(
            "{SGI_MAC_PREFIX}:{:02x}:{:02x}:{low:02x}",
            random[0], random[1]
        );
        let first = number(&mac).unwrap();
        if conflict(claims, first, first + u64::from(span(offering))).is_none() {
            return Ok(mac);
        }
    }
    Err("could not find an unused MAC beside this machine".into())
}

/// The MAC a new machine in `dir` gets: the one asked for, refused when a
/// neighbour has it, else a generated one. A machine that takes no MAC gets
/// none.
pub fn assign(
    machines: &Machines,
    dir: &Path,
    offering: &Offering,
    requested: Option<&str>,
) -> Result<Option<String>> {
    if let Some(mac) = requested {
        check_free(machines, dir, Some(offering), mac)?;
        return Ok(Some(mac.to_ascii_lowercase()));
    }
    if !crate::profiles::takes_mac(offering) {
        return Ok(None);
    }
    generate(offering, &claims(machines, dir)).map(Some)
}

/// One warning for each neighbouring machine whose addresses overlap `file`'s.
pub fn duplicate_warnings(machines: &Machines, dir: &Path, file: &MachineFile) -> Vec<String> {
    let Some(first) = file.identity.as_ref().and_then(|id| number(&id.mac)) else {
        return vec![];
    };
    let reach = offering(machines, &file.machine).map_or(0, span);
    let last = first + u64::from(reach);
    claims(machines, dir)
        .iter()
        .filter(|claim| claim.overlaps(first, last))
        .map(|other| {
            format!(
                "this machine's MAC {} collides with the machine in {}, which {}",
                text(first),
                other.dir.display(),
                other.describe()
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{preset, test_machines};

    /// A sibling machine directory that holds only an identity.
    fn sibling(root: &Path, name: &str, mac: &str) -> PathBuf {
        let dir = root.join(name);
        fs::create_dir(&dir).unwrap();
        fs::write(
            dir.join("machine.toml"),
            format!("[identity]\nmac = \"{mac}\"\n"),
        )
        .unwrap();
        dir
    }

    fn with_offset(offset: u8) -> Offering {
        let machines = test_machines();
        let mut offer = preset(&machines, "origin200-1").unwrap().clone();
        offer.networks[0].mac.as_mut().unwrap().offset = offset;
        offer
    }

    #[test]
    fn generated_macs_are_sgi_and_leave_room_for_the_offset() {
        let offer = with_offset(200);
        for _ in 0..500 {
            let mac = generate(&offer, &[]).unwrap();
            assert!(mac.starts_with("08:00:69:"), "{mac}");
            assert!(number(&mac).unwrap() & 0xff <= 55, "{mac}");
        }
        let mac = generate_from(&offer, &[], || Ok([1, 2, 255])).unwrap();
        assert_eq!(mac, "08:00:69:01:02:1f");
    }

    #[test]
    fn a_sibling_that_holds_the_address_forces_regeneration() {
        let root = tempfile::tempdir().unwrap();
        let machines = test_machines();
        let other = sibling(root.path(), "other", "08:00:69:01:02:10");
        let claims = claims(&machines, &root.path().join("new"));
        assert_eq!(claims.len(), 1);
        let mut draws = vec![[1, 2, 0x10], [3, 4, 5]].into_iter();
        let mac = generate_from(&with_offset(0), &claims, || Ok(draws.next().unwrap())).unwrap();
        assert_eq!(mac, "08:00:69:03:04:05");
        // A neighbour's derived run counts as taken, not only its base.
        let ranged = [Claim {
            dir: other,
            first: number("08:00:69:01:02:10").unwrap(),
            last: number("08:00:69:01:02:20").unwrap(),
        }];
        let mut draws = vec![[1, 2, 0x18], [3, 4, 5]].into_iter();
        let mac = generate_from(&with_offset(0), &ranged, || Ok(draws.next().unwrap())).unwrap();
        assert_eq!(mac, "08:00:69:03:04:05");
    }

    #[test]
    fn an_explicit_duplicate_is_refused_naming_the_sibling() {
        let root = tempfile::tempdir().unwrap();
        let machines = test_machines();
        let offer = preset(&machines, "origin200-1").unwrap();
        let other = sibling(root.path(), "other", "08:00:69:01:02:10");
        let new = root.path().join("new");
        let error = assign(&machines, &new, offer, Some("08:00:69:01:02:10"))
            .unwrap_err()
            .to_string();
        let canonical = other.canonicalize().unwrap();
        assert!(error.contains(canonical.to_str().unwrap()), "{error}");
        assert!(error.contains("08:00:69:01:02:10"), "{error}");
        assert_eq!(
            assign(&machines, &new, offer, Some("08:00:69:01:02:11")).unwrap(),
            Some("08:00:69:01:02:11".into())
        );
    }
}
