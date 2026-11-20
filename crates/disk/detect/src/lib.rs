//! Which operating system and version is on a disk image.

pub mod image;

use disk_dvh as dvh;
use disk_efs::Efs;
use disk_rofs::{Dev, Part, ReadFs, Volumes};
use disk_xfs::Xfs;
use std::collections::HashMap;
use std::io;

/// Opens the file system on one partition, whichever of EFS and XFS it is.
fn open_fs(part: Part) -> Option<Box<dyn ReadFs>> {
    if let Ok(fs) = Efs::open(part.clone()) {
        return Some(Box::new(fs));
    }
    Xfs::open(part)
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
            Detected::Unknown => write!(f, "unknown"),
        }
    }
}

/// Opens the image at `path` (raw, CHD or qcow2) and detects what is on it.
pub fn detect_path(path: &std::path::Path) -> io::Result<Detected> {
    let (dev, size) = image::open(path)?;
    Ok(detect(&dev, size))
}

/// Detects the system on an SGI disk. Read errors count as "not found".
pub fn detect(dev: &Dev, size: u64) -> Detected {
    if let Ok(vols) = mount_sgi(dev, size) {
        if let Some(version) = irix_version(&vols) {
            return Detected::Irix { version };
        }
    }
    linux(dev, size).unwrap_or(Detected::Unknown)
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

struct Ext4Dev(Part);

impl ext4_view::Ext4Read for Ext4Dev {
    fn read(
        &mut self,
        start: u64,
        dst: &mut [u8],
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync + 'static>> {
        disk_rofs::ReadAt::read_exact_at(&self.0, start, dst).map_err(Box::new)?;
        Ok(())
    }
}

/// Linux on an SGI disk: an ext2/3/4 partition (type 0x83) with an os-release.
fn linux(dev: &Dev, size: u64) -> Option<Detected> {
    let mut head = [0u8; 512];
    dev.read_exact_at(0, &mut head).ok()?;
    let hdr = dvh::parse(&head).ok()?;
    hdr.partitions
        .iter()
        .filter(|p| !p.is_empty() && p.kind == 0x83 && p.start() + p.len() <= size)
        .find_map(|p| {
            let fs = ext4_view::Ext4::load(Box::new(Ext4Dev(Part {
                dev: dev.clone(),
                start: p.start(),
                len: p.len(),
            })))
            .ok()?;
            let text = ["/etc/os-release", "/usr/lib/os-release"]
                .iter()
                .find_map(|f| read_capped(&fs, f))?;
            parse_os_release(&text)
        })
}

/// Reads up to 64 KiB of a file, so a hostile size can't make us allocate.
fn read_capped(fs: &ext4_view::Ext4, path: &str) -> Option<String> {
    let mut file = fs.open(path).ok()?;
    let mut buf = vec![0u8; 64 << 10];
    let mut len = 0;
    while len < buf.len() {
        match file.read_bytes(&mut buf[len..]).ok()? {
            0 => break,
            n => len += n,
        }
    }
    buf.truncate(len);
    String::from_utf8(buf).ok()
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
