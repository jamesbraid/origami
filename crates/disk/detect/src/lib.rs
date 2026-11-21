//! Which operating system and version is on a disk image.

pub mod image;
mod label;

use disk_dvh as dvh;
use disk_efs::Efs;
use disk_ext::Ext;
use disk_ffs::Ffs;
use disk_rofs::{Dev, Part, ReadFs, Volumes};
use disk_xfs::Xfs;
use std::collections::{HashMap, HashSet};
use std::io;

/// Opens the file system on one partition, whichever kind it is.
fn open_fs(part: Part) -> Option<Box<dyn ReadFs>> {
    if let Ok(fs) = Efs::open(part.clone()) {
        return Some(Box::new(fs));
    }
    if let Ok(fs) = Xfs::open(part.clone()) {
        return Some(Box::new(fs));
    }
    if let Ok(fs) = Ffs::open(part.clone()) {
        return Some(Box::new(fs));
    }
    Ext::open(part)
        .ok()
        .map(|fs| Box::new(fs) as Box<dyn ReadFs>)
}

/// Maps an fstab device to a partition slot: `/dev/root` is slot 0,
/// `/dev/usr` slot 6 and `/dev/dsk/dks0d1sN` slot N.
fn fstab_slot(dev: &str) -> Option<usize> {
    match dev {
        "/dev/root" => Some(0),
        "/dev/usr" => Some(6),
        _ => dev
            .strip_prefix("/dev/dsk/")?
            .rsplit_once('s')?
            .1
            .parse()
            .ok(),
    }
}

/// Mounts the file systems of an SGI disk: slot 0 (or the whole-disk slot 7)
/// as `/`, then the others where the root's `/etc/fstab` puts them.
pub fn mount_sgi(dev: &Dev, size: u64) -> io::Result<Volumes> {
    let mut head = [0u8; 512];
    dev.read_exact_at(0, &mut head)?;
    let hdr = dvh::parse(&head).map_err(disk_rofs::invalid)?;
    let mut fss: HashMap<usize, Box<dyn ReadFs>> = HashMap::new();
    for (slot, p) in hdr.partitions.iter().enumerate() {
        let is_fs = p.is_efs() || p.kind == dvh::TYPE_XFS || p.kind == dvh::TYPE_RAW;
        if p.is_empty() || !is_fs || p.start() + p.len() > size {
            continue;
        }
        if let Some(fs) = open_fs(Part {
            dev: dev.clone(),
            start: p.start(),
            len: p.len(),
        }) {
            fss.insert(slot, fs);
        }
    }
    let mut vols = Volumes::new();
    let root = fss.remove(&0).or_else(|| fss.remove(&7));
    let Some(root) = root else { return Ok(vols) };
    let fstab = root.stat("etc/fstab").and_then(|st| {
        disk_rofs::read_limited(root.open("etc/fstab")?.as_ref(), st.size, 64 << 10)
    });
    vols.mount("/", root);
    for line in String::from_utf8_lossy(&fstab.unwrap_or_default()).lines() {
        let mut f = line.split_whitespace();
        if let (Some(dev), Some(at)) = (f.next(), f.next()) {
            if let Some(fs) = fstab_slot(dev)
                .filter(|&s| s != 0)
                .and_then(|s| fss.remove(&s))
            {
                vols.mount(at, fs);
            }
        }
    }
    Ok(vols)
}

/// The result of looking at an image.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Detected {
    Irix {
        version: String,
    },
    Linux {
        name: String,
        version: Option<String>,
    },
    OpenBsd {
        version: String,
    },
    NetBsd {
        version: String,
    },
    Unknown,
}

impl std::fmt::Display for Detected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Detected::Irix { version } => write!(f, "IRIX {version}"),
            Detected::Linux {
                name,
                version: Some(v),
            } => write!(f, "{name} {v}"),
            Detected::Linux {
                name,
                version: None,
            } => write!(f, "{name}"),
            Detected::OpenBsd { version } => write!(f, "OpenBSD {version}"),
            Detected::NetBsd { version } => write!(f, "NetBSD {version}"),
            Detected::Unknown => write!(f, "unknown"),
        }
    }
}

/// Opens the image at `path` (raw, CHD or qcow2) and detects what is on it.
pub fn detect_path(path: &std::path::Path) -> io::Result<Detected> {
    let (dev, size) = image::open(path)?;
    Ok(detect(&dev, size))
}

/// Detects the system on a disk: an SGI volume header, a BSD disklabel, or
/// a bare file system. Read errors, and a panic in any reader, count as "not found".
pub fn detect(dev: &Dev, size: u64) -> Detected {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| detect_disk(dev, size)))
        .unwrap_or(Detected::Unknown)
}

fn detect_disk(dev: &Dev, size: u64) -> Detected {
    if let Ok(vols) = mount_sgi(dev, size) {
        if let Some(version) = irix_version(&vols) {
            return Detected::Irix { version };
        }
    }
    for part in candidates(dev, size) {
        let Some(fs) = open_fs(part) else { continue };
        let mut vols = Volumes::new();
        vols.mount("/", fs);
        if let Some(found) = identify(&vols) {
            return found;
        }
    }
    Detected::Unknown
}

/// Byte ranges that may hold a file system: volume header partitions, BSD
/// disklabel slots, then the whole disk for a bare file system.
fn candidates(dev: &Dev, size: u64) -> Vec<Part> {
    let mut head = vec![0u8; 1024];
    let n = dev.read_at(0, &mut head).unwrap_or(0);
    head.truncate(n);
    let part = |start: u64, len: u64| {
        (len > 0 && start.checked_add(len).is_some_and(|e| e <= size)).then(|| Part {
            dev: dev.clone(),
            start,
            len,
        })
    };
    let mut out = vec![];
    let add_label = |at: u64, out: &mut Vec<Part>| {
        let mut b = vec![0u8; 1024];
        let n = dev.read_at(at, &mut b).unwrap_or(0);
        if let Some(l) = label::find(&b[..n]) {
            for s in l.slots {
                if matches!(s.fstype, label::FS_BSDFFS | label::FS_EXT2FS) {
                    out.extend(part(s.offset * l.sector, s.size * l.sector));
                }
            }
        }
    };
    match dvh::parse(&head) {
        Ok(hdr) => {
            for p in hdr.partitions.iter().filter(|p| !p.is_empty()) {
                if matches!(p.kind, dvh::TYPE_VOLHDR | dvh::TYPE_VOLUME) {
                    continue;
                }
                out.extend(part(p.start(), p.len()));
                if p.kind == dvh::TYPE_BSD && p.start() < size {
                    add_label(p.start(), &mut out);
                }
            }
        }
        Err(_) => add_label(0, &mut out),
    }
    out.extend(part(0, size));
    let mut seen = HashSet::new();
    out.retain(|p| seen.insert((p.start, p.len)));
    out
}

/// The system a mounted root file system belongs to.
fn identify(vols: &Volumes) -> Option<Detected> {
    if let Some(version) = irix_version(vols) {
        return Some(Detected::Irix { version });
    }
    if let Some(found) = ["/etc/os-release", "/usr/lib/os-release"]
        .iter()
        .find_map(|f| vols.read_file(f, 64 << 10).ok())
        .and_then(|text| parse_os_release(&String::from_utf8_lossy(&text)))
    {
        return Some(found);
    }
    bsd_release(vols)
}

/// The release is the kernel's version banner ("OpenBSD 6.9 (GENERIC) #1"), in
/// /bsd or /netbsd: every install has it there and it cannot be edited like /etc/motd.
fn bsd_release(vols: &Volumes) -> Option<Detected> {
    let (os, kernel) = [("OpenBSD", "/bsd"), ("NetBSD", "/netbsd")]
        .into_iter()
        .find(|(_, k)| {
            vols.stat(k)
                .is_ok_and(|st| st.file_type == disk_rofs::FileType::File)
        })?;
    let version = banner_version(&vols.read_file(kernel, 32 << 20).ok()?, os)?;
    Some(if os == "OpenBSD" {
        Detected::OpenBsd { version }
    } else {
        Detected::NetBsd { version }
    })
}

/// The release in the first "<os> X.Y... (" banner of a kernel image.
fn banner_version(kernel: &[u8], os: &str) -> Option<String> {
    let tag = format!("{os} ");
    kernel.windows(tag.len()).enumerate().find_map(|(i, w)| {
        if w != tag.as_bytes() {
            return None;
        }
        let rest = &kernel[i + tag.len()..];
        let end = rest
            .iter()
            .position(|c| !(c.is_ascii_alphanumeric() || b"._-".contains(c)))?;
        let token = std::str::from_utf8(&rest[..end]).ok()?;
        (rest[end..].starts_with(b" (")
            && token.len() <= 24
            && token.starts_with(|c: char| c.is_ascii_digit())
            && token.contains('.'))
        .then(|| token.to_string())
    })
}

/// The release the kernel carries. IRIX keeps its installed-product history in
/// a binary database this does not decode; the kernel names its release as a
/// stand-alone string ("5.3", or "6.5" then "6.5.22m").
fn irix_version(vols: &Volumes) -> Option<String> {
    let unix = vols.read_file("/unix", 16 << 20).ok()?;
    if !unix.windows(12).any(|w| w == b"IRIX Release") {
        return None;
    }
    kernel_release(&unix)
}

fn kernel_release(kernel: &[u8]) -> Option<String> {
    let is_release = |t: &str| {
        let mut parts = t.split('.');
        let (Some(major), Some(minor)) = (parts.next(), parts.next()) else {
            return false;
        };
        let patch = parts.next();
        let digits =
            |s: &str| !s.is_empty() && s.len() <= 2 && s.bytes().all(|c| c.is_ascii_digit());
        let patch_ok =
            patch.is_none_or(|p| digits(p.trim_end_matches(|c: char| c.is_ascii_lowercase())));
        matches!(major, "5" | "6") && digits(minor) && patch_ok && parts.next().is_none()
    };
    // The most specific release wins: "6.5.22m" over "6.5".
    kernel
        .split(|&c| c == 0)
        .filter_map(|t| std::str::from_utf8(t).ok())
        .filter(|t| is_release(t))
        .max_by_key(|t| t.matches('.').count())
        .map(String::from)
}

fn parse_os_release(text: &str) -> Option<Detected> {
    let field = |key: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(key)?.strip_prefix('='))
            .map(|v| v.trim().trim_matches(['"', '\'']).to_string())
    };
    let name = field("NAME")?;
    let version = field("VERSION_ID")
        .or_else(|| field("VERSION"))
        .filter(|v| !v.is_empty());
    Some(Detected::Linux { name, version })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kernel_release_prefers_the_specific_string() {
        assert_eq!(
            kernel_release(b"x\x006.5\x00\x006.5.22m\x00\x00y 5.0 z\x00"),
            Some("6.5.22m".into())
        );
        assert_eq!(kernel_release(b"\0\x005.3\0\0"), Some("5.3".into()));
        assert_eq!(kernel_release(b"nothing 5.3 here\0"), None);
    }

    #[test]
    fn os_release_falls_back_to_version() {
        let d = parse_os_release("NAME=\"Gentoo\"\nVERSION=\"2.15\"\n").unwrap();
        assert_eq!(
            d,
            Detected::Linux {
                name: "Gentoo".into(),
                version: Some("2.15".into())
            }
        );
    }
}
