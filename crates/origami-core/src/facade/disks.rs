//! Disk images: what a file holds, whether it can be attached, and copying
//! one into a machine's folder.

use super::error::{CoreError, CoreResult, ErrorKind};
use super::operation::Control;
use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

/// What `Core::check_disk_image` says about a file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiskImageCheck {
    pub bytes: u64,
    pub is_chd: bool,
    /// Bytes free where a copy would go.
    pub free_bytes: u64,
    /// The name of another machine that already uses this file.
    pub used_by: Option<String>,
    /// Why the file cannot be attached, in the core's words.
    pub problem: Option<String>,
}

/// What to say under the choices for an image, and whether it can be added.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageVerdict {
    pub note: Option<String>,
    /// The note is a warning rather than reassurance.
    pub warning: bool,
    pub ready: bool,
}

/// The same words and the same rules in every app, given whether the image is copied in.
pub fn image_verdict(check: &DiskImageCheck, copy: bool) -> ImageVerdict {
    let no_room = copy && check.bytes > check.free_bytes;
    let ready = check.problem.is_none() && !no_room;
    let (note, warning) = if let Some(p) = &check.problem {
        (Some(p.clone()), true)
    } else if let Some(name) = &check.used_by {
        let text = format!(
            "{name} already uses this file. Two machines writing to one disk will corrupt it."
        );
        (Some(text), true)
    } else if no_room {
        let text = format!(
            "A copy needs {} and {} is free. Use the disk where it is.",
            size_label(check.bytes),
            size_label(check.free_bytes)
        );
        (Some(text), true)
    } else if copy {
        (
            Some("IRIX writes to this disk. The copy keeps your original safe.".into()),
            false,
        )
    } else if !check.is_chd {
        (
            Some("IRIX will write into this file. Keep a copy of the original.".into()),
            true,
        )
    } else {
        (None, false)
    };
    ImageVerdict {
        note,
        warning,
        ready,
    }
}

/// The choices New Machine offers and how an image is kept. Index 0 of `place` is the
/// copy and index 0 of `keep` keeps a CHD's changes apart.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiskLabels {
    /// No disk, an empty disk, your own image.
    pub choices: Vec<String>,
    pub place: Vec<String>,
    pub keep: Vec<String>,
    pub same_kind: String,
    /// The choice that a ready-made disk adds after the others, and the title
    /// of the list of releases under it.
    pub ready: String,
    pub release: String,
    /// The empty disk's size to start with, in GB.
    pub default_gb: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiskChoice {
    Ready,
    Install,
    Image,
    NoDisk,
}

/// One way to give a new machine its disk, with the words for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiskOption {
    pub choice: DiskChoice,
    pub title: String,
    pub caption: String,
    /// Tagged "Recommended".
    pub recommended: bool,
}

/// The disk choices in the order to list them; the first is the default. A
/// ready-made disk leads when the catalogue offers one for the model and engine.
pub fn disk_options(ready_available: bool) -> Vec<DiskOption> {
    let option = |choice, title: &str, caption: &str, recommended| DiskOption {
        choice,
        title: title.into(),
        caption: caption.into(),
        recommended,
    };
    let all = [
        option(
            DiskChoice::Ready,
            "Ready-made IRIX disk",
            "IRIX installed and ready to start.",
            true,
        ),
        option(
            DiskChoice::Install,
            "Install IRIX from discs",
            "Start with an empty disk and run the install assistant.",
            false,
        ),
        option(
            DiskChoice::Image,
            "Use a disk image I already have",
            "A .chd, .qcow2, .img or .raw file.",
            false,
        ),
        option(
            DiskChoice::NoDisk,
            "No disk",
            "Add one later in Settings.",
            false,
        ),
    ];
    all.into_iter()
        .filter(|o| ready_available || o.choice != DiskChoice::Ready)
        .collect()
}

/// What to say under a ready-made disk: its download size, or that it is here.
pub fn ready_caption(bytes: u64, cached: bool) -> String {
    if cached {
        "Downloaded".into()
    } else {
        format!("{} download", size_label(bytes))
    }
}

pub fn disk_labels() -> DiskLabels {
    let list = |items: &[&str]| items.iter().map(|s| s.to_string()).collect();
    DiskLabels {
        choices: list(&["No disk", "Empty disk", "Your own image"]),
        place: list(&[
            "Copy it into the machine's folder (recommended)",
            "Use it where it is",
        ]),
        keep: list(&["Keep changes apart", "Save into the image"]),
        same_kind: "Use a disk made for the same kind of machine.".into(),
        ready: "Ready-made disk".into(),
        release: "IRIX release".into(),
        default_gb: 4,
    }
}

type Key = (SystemTime, u64);

fn cache() -> &'static Mutex<HashMap<PathBuf, (Key, String)>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, (Key, String)>>> = OnceLock::new();
    CACHE.get_or_init(Mutex::default)
}

fn key_of(path: &Path) -> Option<Key> {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.modified().ok()?, meta.len()))
}

/// The last label read for `path`, whatever the file looks like now.
pub(crate) fn last_label(path: &Path) -> Option<String> {
    cache()
        .lock()
        .unwrap()
        .get(path)
        .map(|(_, label)| label.clone())
}

/// The badge text for `path` if it was read since the file last changed.
pub(crate) fn cached_label(path: &Path) -> Option<String> {
    let key = key_of(path)?;
    let cache = cache().lock().unwrap();
    cache
        .get(path)
        .filter(|(seen, _)| *seen == key)
        .map(|(_, label)| label.clone())
}

const UNKNOWN: &str = "Unknown";

/// A label for people, or none when the disk was not recognised.
pub(crate) fn recognised(label: String) -> Option<String> {
    (label != UNKNOWN).then_some(label)
}

/// The release without its stream letter: `6.5.30m` is `6.5.30`.
fn release(version: &str) -> &str {
    version.trim_end_matches(|c: char| c.is_ascii_alphabetic())
}

pub(super) fn label_of(detected: &disk_detect::Detected) -> String {
    use disk_detect::Detected::*;
    match detected {
        Irix { version } => format!("IRIX {}", release(version)),
        Linux {
            name,
            version: Some(v),
        } => format!("Linux ({} {v})", name.trim_end_matches(" GNU/Linux")),
        Linux {
            name,
            version: None,
        } => format!("Linux ({})", name.trim_end_matches(" GNU/Linux")),
        OpenBsd { version } => format!("OpenBSD {version}"),
        NetBsd { version } => format!("NetBSD {version}"),
        Unknown => UNKNOWN.into(),
    }
}

/// The OS and release on the image, such as `IRIX 6.5.30`, or `Unknown`
/// when it cannot be told. Reads the file once per size and change time.
pub(crate) fn label(path: &Path) -> CoreResult<String> {
    if let Some(label) = cached_label(path) {
        return Ok(label);
    }
    let key = key_of(path).ok_or_else(|| {
        CoreError::new(
            ErrorKind::NotFound,
            format!("cannot read {}", path.display()),
        )
    })?;
    let label = disk_detect::detect_path(path).map_or_else(|_| UNKNOWN.into(), |d| label_of(&d));
    cache()
        .lock()
        .unwrap()
        .insert(path.to_path_buf(), (key, label.clone()));
    Ok(label)
}

/// Files being read for the library listing, so each is read once.
static READING: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();

/// Held while a file is read, even through a panic; None when it is unreadable or already being read.
pub(crate) struct Reading(PathBuf);

pub(crate) fn begin_read(path: &Path) -> Option<Reading> {
    key_of(path)?;
    let fresh = READING
        .get_or_init(Mutex::default)
        .lock()
        .unwrap()
        .insert(path.to_path_buf());
    fresh.then(|| Reading(path.to_path_buf()))
}

impl Drop for Reading {
    fn drop(&mut self) {
        if let Some(reading) = READING.get() {
            reading
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&self.0);
        }
    }
}

/// Bytes available to this user on the volume holding `path`.
pub(crate) fn free_space(path: &Path) -> u64 {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let Ok(c) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
            return 0;
        };
        let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
        // SAFETY: `c` is a valid C string and `s` a valid out-pointer.
        if unsafe { libc::statvfs(c.as_ptr(), &mut s) } != 0 {
            return 0;
        }
        (s.f_bavail as u64).saturating_mul(s.f_frsize as u64)
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
        let mut free = 0u64;
        // SAFETY: `wide` is NUL-terminated and `free` a valid out-pointer.
        let ok = unsafe {
            windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW(
                wide.as_ptr(),
                &mut free,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            0
        } else {
            free
        }
    }
}

/// Decimal gigabytes or megabytes, such as `3.3 GB`, the same in every app.
pub fn size_label(bytes: u64) -> String {
    if bytes >= 1_000_000_000 {
        format!("{:.1} GB", bytes as f64 / 1e9)
    } else {
        format!("{} MB", bytes / 1_000_000)
    }
}

/// The size the guest sees: the logical size of a qcow2 or CHD, else `file_bytes`.
pub(crate) fn guest_size(path: &Path, file_bytes: u64) -> u64 {
    disk_detect::image::open(path).map_or(file_bytes, |(_, size)| size)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DropKind {
    CdImage,
    DiskImage,
    Other,
}

/// What a dropped file is, by its header and then its extension: a CD image
/// changes the CD, a disk image makes a machine.
pub fn classify_drop(path: &Path) -> DropKind {
    let file = path
        .is_file()
        .then(|| std::fs::File::open(path).ok())
        .flatten();
    let Some(mut file) = file else {
        return DropKind::Other;
    };
    let mut magic = [0u8; 8];
    let container =
        file.read_exact(&mut magic).is_ok() && (magic == *b"MComprHD" || magic[..4] == *b"QFI\xfb");
    let iso9660 = (|| {
        use std::io::{Seek, SeekFrom};
        file.seek(SeekFrom::Start(0x8001)).ok()?;
        let mut id = [0u8; 5];
        file.read_exact(&mut id).ok()?;
        Some(&id == b"CD001")
    })()
    .unwrap_or(false);
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase());
    match ext.as_deref() {
        _ if container => DropKind::DiskImage,
        _ if iso9660 => DropKind::CdImage,
        Some("iso") => DropKind::CdImage,
        Some("chd" | "qcow2" | "img" | "raw") => DropKind::DiskImage,
        _ => DropKind::Other,
    }
}

/// The words for a drop target: what dropping that kind of file does.
pub fn drop_caption(kind: DropKind) -> Option<&'static str> {
    match kind {
        DropKind::CdImage => Some("Change CD"),
        DropKind::DiskImage => Some("New machine from this disk"),
        DropKind::Other => None,
    }
}

/// The size of a regular file, after following links; a pipe, device or folder would
/// block a copy or never end.
pub(crate) fn regular_size(path: &Path) -> CoreResult<u64> {
    let meta = std::fs::metadata(path)?;
    if !meta.is_file() {
        return Err(CoreError::new(
            ErrorKind::Invalid,
            format!("{} is not a regular file", path.display()),
        ));
    }
    Ok(meta.len())
}

/// Copy `src` to a new file in `dir/drives`, reporting progress and stopping
/// when cancelled. A failed or cancelled copy leaves no file behind.
pub(crate) fn copy_into(src: &Path, dir: &Path, control: &Control) -> CoreResult<PathBuf> {
    let total = regular_size(src)?;
    let drives = dir.join("drives");
    std::fs::create_dir_all(&drives)?;
    let free = free_space(&drives);
    if total > free {
        return Err(CoreError::new(
            ErrorKind::Io,
            format!(
                "A copy needs {} and {} is free",
                size_label(total),
                size_label(free)
            ),
        ));
    }
    let stem = src
        .file_stem()
        .map_or_else(String::new, |s| s.to_string_lossy().into_owned());
    let ext = src
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    let mut dest = drives.join(format!("{stem}{ext}"));
    for n in 2.. {
        if !dest.exists() {
            break;
        }
        dest = drives.join(format!("{stem}-{n}{ext}"));
    }
    let from = std::fs::File::open(src)?;
    // A file that was not made here is not ours to delete.
    let to = std::fs::File::create_new(&dest)?;
    let result = copy_file(from, to, total, control);
    if result.is_err() {
        let _ = std::fs::remove_file(&dest);
    }
    result.map(|()| dest)
}

fn copy_file(
    mut from: std::fs::File,
    mut to: std::fs::File,
    total: u64,
    control: &Control,
) -> CoreResult<()> {
    let full = |e: &std::io::Error| e.kind() == std::io::ErrorKind::StorageFull;
    let mut buf = vec![0u8; 4 << 20];
    let mut done = 0u64;
    loop {
        if control.is_cancelled() {
            return Err(CoreError::new(
                ErrorKind::Cancelled,
                "the copy was cancelled",
            ));
        }
        let n = from.read(&mut buf)?;
        if n == 0 {
            break;
        }
        to.write_all(&buf[..n]).map_err(|e| {
            if full(&e) {
                CoreError::new(
                    ErrorKind::Io,
                    "The copy stopped because the disk is full. The partial file was deleted.",
                )
            } else {
                e.into()
            }
        })?;
        done += n as u64;
        control.progress(format!("{} of {}", size_label(done), size_label(total)));
        control.fraction(done as f32 / total.max(1) as f32);
    }
    to.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dropped_file_is_a_cd_or_a_disk_by_its_header_then_its_extension() {
        let dir = tempfile::tempdir().unwrap();
        let write = |name: &str, bytes: &[u8]| {
            let path = dir.path().join(name);
            std::fs::write(&path, bytes).unwrap();
            path
        };
        let mut iso = vec![0u8; 0x8006];
        iso[0x8001..0x8006].copy_from_slice(b"CD001");
        let mut qcow = vec![0u8; 64];
        qcow[..4].copy_from_slice(b"QFI\xfb");
        qcow[24..32].copy_from_slice(&(4u64 << 30).to_be_bytes());
        assert_eq!(classify_drop(&write("any.img", &iso)), DropKind::CdImage);
        assert_eq!(
            classify_drop(&write("disc.iso", b"short")),
            DropKind::CdImage
        );
        // A CD image named like a disk, and a disk with no recognisable header.
        assert_eq!(
            classify_drop(&write("disk.qcow2", &qcow)),
            DropKind::DiskImage
        );
        assert_eq!(
            classify_drop(&write("blank.raw", &[0u8; 64])),
            DropKind::DiskImage
        );
        assert_eq!(
            classify_drop(&write("notes.txt", b"hello")),
            DropKind::Other
        );
        assert_eq!(
            classify_drop(&dir.path().join("missing.iso")),
            DropKind::Other
        );
        assert_eq!(classify_drop(dir.path()), DropKind::Other);
        assert_eq!(drop_caption(DropKind::CdImage), Some("Change CD"));
        assert_eq!(drop_caption(DropKind::Other), None);
        assert_eq!(guest_size(&write("plain.img", &[0u8; 64]), 64), 64);
    }

    #[test]
    fn a_ready_made_disk_leads_only_when_one_is_offered() {
        let with = disk_options(true);
        assert_eq!(with[0].choice, DiskChoice::Ready);
        assert!(with[0].recommended && with.iter().filter(|o| o.recommended).count() == 1);
        let without = disk_options(false);
        assert_eq!(without[0].choice, DiskChoice::Install);
        assert!(without.iter().all(|o| !o.recommended));
        assert_eq!(ready_caption(835_000_000, false), "835 MB download");
        assert_eq!(ready_caption(835_000_000, true), "Downloaded");
    }

    #[test]
    fn a_release_has_no_stream_letter() {
        assert_eq!(release("6.5.30m"), "6.5.30");
        assert_eq!(release("6.5.22"), "6.5.22");
    }
}
