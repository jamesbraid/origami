//! Ready-made disks: the published index, the shared download cache and the
//! checked download. Machines never use a cached file directly; they copy it.

use crate::assets::{cache_dir, https_agent, manifest, validate_https_url};
use crate::Result;
use serde::Deserialize;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// One disk of `disks/index.json`. Fields the app does not use are ignored.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct Disk {
    /// The disk's path on the server, which is also how the app names it.
    pub name: String,
    pub url: String,
    pub sha256: String,
    pub size: u64,
    /// The IRIX release.
    pub version: String,
    /// The board, which tells apart disks for one product such as the Indigo2.
    pub board: String,
    /// SGI product names, lowercase.
    pub machines: Vec<String>,
}

#[derive(Deserialize)]
struct Index {
    format: u32,
    disks: Vec<serde_json::Value>,
}

/// What the download reports.
pub enum Event {
    Downloading { done: u64, total: u64 },
    Checking,
}

#[derive(Debug)]
pub enum DownloadError {
    Cancelled,
    Failed(String),
}

impl From<std::io::Error> for DownloadError {
    fn from(e: std::io::Error) -> Self {
        DownloadError::Failed(e.to_string())
    }
}

impl From<ureq::Error> for DownloadError {
    fn from(e: ureq::Error) -> Self {
        DownloadError::Failed(e.to_string())
    }
}

impl From<crate::Error> for DownloadError {
    fn from(e: crate::Error) -> Self {
        DownloadError::Failed(e.to_string())
    }
}

fn root() -> Result<PathBuf> {
    Ok(cache_dir()?.join("disks"))
}

fn is_sha256(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f'))
}

/// Only disks on the server the app's firmware comes from, under safe names.
/// An entry that does not read or does not qualify is skipped, not fatal.
fn parse(text: &str, base: &str) -> Result<Vec<Disk>> {
    let index: Index = serde_json::from_str(text)?;
    if index.format != 1 {
        return Err(format!("disk index format {} is not understood", index.format).into());
    }
    Ok(index
        .disks
        .into_iter()
        .filter_map(|d| serde_json::from_value::<Disk>(d).ok())
        .filter(|d| {
            is_sha256(&d.sha256)
                && d.size > 0
                && d.url == format!("{base}{}", d.name)
                && validate_https_url(&d.url).is_ok()
                && d.name.rsplit('/').next().is_some_and(|f| !f.is_empty())
        })
        .collect())
}

/// The last index fetched, without touching the network.
pub fn cached_index() -> Result<Vec<Disk>> {
    parse(
        &std::fs::read_to_string(root()?.join("index.json"))?,
        &manifest()?.base_url,
    )
}

/// The index from the server, which is kept for next time; the last kept copy
/// when the server cannot be reached.
pub fn index() -> Result<Vec<Disk>> {
    let base = manifest()?.base_url;
    let fetched = https_agent(Some(std::time::Duration::from_secs(20)))
        .get(&format!("{base}disks/index.json"))
        .call()
        .map_err(crate::Error::from)
        .and_then(|mut r| Ok(r.body_mut().read_to_string()?));
    let Ok(text) = fetched else {
        return cached_index();
    };
    let disks = parse(&text, &base)?;
    let dir = root()?;
    std::fs::create_dir_all(&dir)?;
    let mut temp = tempfile::NamedTempFile::new_in(&dir)?;
    temp.write_all(text.as_bytes())?;
    temp.persist(dir.join("index.json")).map_err(|e| e.error)?;
    Ok(disks)
}

fn file_name(disk: &Disk) -> &str {
    disk.name.rsplit('/').next().unwrap_or(&disk.name)
}

/// The folder a disk is cached in, named by its digest as firmware is.
pub fn dir_of(disk: &Disk) -> Result<PathBuf> {
    Ok(root()?.join(&disk.sha256))
}

/// The cached file, once the download finished and was checked.
pub fn cached_path(disk: &Disk) -> Option<PathBuf> {
    let path = dir_of(disk).ok()?.join(file_name(disk));
    std::fs::metadata(&path)
        .is_ok_and(|m| m.len() == disk.size)
        .then_some(path)
}

/// A hold on a disk's cache folder, released when dropped, by the operating
/// system too if the process dies. A download holds it alone; a copy holds it
/// with other copies; `remove` refuses while anyone holds it.
pub struct Lock(#[allow(dead_code)] std::fs::File);

const BUSY: &str = "This disk is being downloaded or copied. Try again when that has finished.";

fn lock_in(dir: &Path, exclusive: bool) -> std::result::Result<Lock, DownloadError> {
    std::fs::create_dir_all(dir)?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join("lock"))?;
    let got = if exclusive {
        file.try_lock()
    } else {
        file.try_lock_shared()
    };
    got.map_err(|_| DownloadError::Failed(BUSY.into()))?;
    Ok(Lock(file))
}

/// Hold the cached disk while it is copied.
pub fn hold(disk: &Disk) -> std::result::Result<Lock, DownloadError> {
    lock_in(&dir_of(disk)?, false)
}

/// Bytes still to download: the size less what a stopped download kept.
pub fn bytes_left(disk: &Disk) -> u64 {
    let part = dir_of(disk)
        .ok()
        .and_then(|d| std::fs::metadata(d.join(format!("{}.part", file_name(disk)))).ok())
        .map_or(0, |m| m.len());
    disk.size.saturating_sub(part)
}

/// Download `disk` into the cache, resuming a part left by a stopped download.
/// `progress` returns false to stop. A file that fails the check is deleted.
pub fn download(
    disk: &Disk,
    progress: &mut dyn FnMut(Event) -> bool,
) -> std::result::Result<PathBuf, DownloadError> {
    let dir = dir_of(disk)?;
    let _lock = lock_in(&dir, true)?;
    if let Some(path) = cached_path(disk) {
        return Ok(path);
    }
    let part = dir.join(format!("{}.part", file_name(disk)));
    fetch(disk, &part, progress)?;
    finish(disk, &part, &dir.join(file_name(disk)), progress)
}

/// The response body as an owned reader, so it can move to the reading thread.
struct BodyReader(ureq::http::Response<ureq::Body>);

impl Read for BodyReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.0.body_mut().as_reader().read(buf)
    }
}

fn fetch(
    disk: &Disk,
    part: &Path,
    progress: &mut dyn FnMut(Event) -> bool,
) -> std::result::Result<(), DownloadError> {
    let mut have = std::fs::metadata(part).map_or(0, |m| m.len());
    if have > disk.size {
        std::fs::remove_file(part)?;
        have = 0;
    }
    if have == disk.size {
        return Ok(());
    }
    // No overall limit: the file is large; `stream` ends a stalled one.
    let agent = https_agent(None);
    let mut request = agent.get(&disk.url);
    if have > 0 {
        request = request.header("Range", format!("bytes={have}-"));
    }
    let response = request.call()?;
    let status = response.status().as_u16();
    // A server that ignores the range sends the whole file again.
    if status == 200 {
        have = 0;
    } else if status != 206 {
        return Err(DownloadError::Failed(format!(
            "the disk server answered HTTP {status}"
        )));
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(part)?;
    file.set_len(have)?;
    file.seek(SeekFrom::Start(have))?;
    let reader = spawn_reader(BodyReader(response));
    stream(reader, &mut file, have, disk.size, IDLE, progress)
}

/// How long a download may go without a byte before it is given up.
const IDLE: std::time::Duration = std::time::Duration::from_secs(60);

/// Write `chunks` to `file` from offset `have`. The reads run on their own thread
/// because ureq has no idle timeout; one that stalls is abandoned and never writes.
/// `progress` is asked every 250 ms, so a cancel never waits for the idle limit.
fn stream(
    chunks: std::sync::mpsc::Receiver<std::io::Result<Vec<u8>>>,
    file: &mut std::fs::File,
    mut have: u64,
    total: u64,
    idle: std::time::Duration,
    progress: &mut dyn FnMut(Event) -> bool,
) -> std::result::Result<(), DownloadError> {
    let again = |why: &str| {
        DownloadError::Failed(format!(
            "{why} Try again: it carries on from where it stopped."
        ))
    };
    let mut last = std::time::Instant::now();
    loop {
        if !progress(Event::Downloading { done: have, total }) {
            return Err(DownloadError::Cancelled);
        }
        match chunks.recv_timeout(std::time::Duration::from_millis(250)) {
            Ok(chunk) => {
                let chunk = chunk?;
                if chunk.is_empty() {
                    break;
                }
                file.write_all(&chunk)?;
                have += chunk.len() as u64;
                last = std::time::Instant::now();
                if have > total {
                    break;
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) if last.elapsed() < idle => {}
            Err(_) => {
                file.sync_all()?;
                return Err(again("The download stalled."));
            }
        }
    }
    file.sync_all()?;
    if have < total {
        return Err(again("The download ended early."));
    }
    Ok(())
}

/// Read `reader` in 1 MiB chunks on a new thread; an empty chunk ends it.
fn spawn_reader(
    mut reader: impl Read + Send + 'static,
) -> std::sync::mpsc::Receiver<std::io::Result<Vec<u8>>> {
    let (tx, rx) = std::sync::mpsc::sync_channel(4);
    std::thread::spawn(move || loop {
        let mut buf = vec![0u8; 1 << 20];
        let sent = match reader.read(&mut buf) {
            Ok(n) => {
                buf.truncate(n);
                let end = n == 0;
                (tx.send(Ok(buf)).is_err(), end)
            }
            Err(e) => (tx.send(Err(e)).is_err(), true),
        };
        if sent.0 || sent.1 {
            break;
        }
    });
    rx
}

/// Check `part` against the index's digest and move it into place.
fn finish(
    disk: &Disk,
    part: &Path,
    dest: &Path,
    progress: &mut dyn FnMut(Event) -> bool,
) -> std::result::Result<PathBuf, DownloadError> {
    if !progress(Event::Checking) {
        return Err(DownloadError::Cancelled);
    }
    let digest = crate::sha256_file(part)?;
    if digest != disk.sha256 || std::fs::metadata(part)?.len() != disk.size {
        std::fs::remove_file(part)?;
        return Err(DownloadError::Failed(
            "The download is damaged. It was deleted. Try again.".into(),
        ));
    }
    std::fs::rename(part, dest)?;
    Ok(dest.to_path_buf())
}

/// A finished download in the cache.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cached {
    /// The folder name, which `remove` takes.
    pub key: String,
    pub file: String,
    pub bytes: u64,
}

pub fn cached() -> Result<Vec<Cached>> {
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir(root()?) else {
        return Ok(found);
    };
    for dir in entries
        .flatten()
        .filter(|e| is_sha256(&e.file_name().to_string_lossy()))
    {
        let Ok(files) = std::fs::read_dir(dir.path()) else {
            continue;
        };
        for f in files.flatten() {
            let file = f.file_name().to_string_lossy().into_owned();
            let bytes = f.metadata().map_or(0, |m| m.len());
            if !file.ends_with(".part") && bytes > 0 {
                found.push(Cached {
                    key: dir.file_name().to_string_lossy().into_owned(),
                    file,
                    bytes,
                });
            }
        }
    }
    found.sort_by(|a, b| a.file.cmp(&b.file));
    Ok(found)
}

/// Delete a cached disk, or the part of one, by `Cached::key`. Refused while
/// it is being downloaded or copied.
pub fn remove(key: &str) -> Result<()> {
    remove_in(&root()?, key)
}

fn remove_in(root: &Path, key: &str) -> Result<()> {
    if !is_sha256(key) {
        return Err("not a cached disk".into());
    }
    let dir = root.join(key);
    if dir.join("lock").exists() {
        drop(lock_in(&dir, true).map_err(|_| BUSY)?);
    }
    match std::fs::remove_dir_all(dir) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = "https://origami-dist.irix.fans/";

    fn entry(name: &str, sha: &str) -> String {
        format!(
            r#"{{"name":"{name}","url":"{BASE}{name}","sha256":"{sha}","size":3,"version":"6.5.22",
            "board":"ip30","machines":["octane"],"format":"chd","contents":["anything"]}}"#
        )
    }

    #[test]
    fn the_index_keeps_only_disks_on_our_server_and_ignores_contents() {
        let good = "a".repeat(64);
        let foreign = entry("x/y.chd", &good).replace(BASE, "https://example.org/");
        let short = entry("x/z.chd", "abc");
        let text = format!(
            r#"{{"format":1,"disks":[{},{foreign},{short}]}}"#,
            entry("irix/a.chd", &good)
        );
        let disks = parse(&text, BASE).unwrap();
        assert_eq!(disks.len(), 1);
        assert_eq!(disks[0].name, "irix/a.chd");
        assert!(parse(r#"{"format":2,"disks":[]}"#, BASE).is_err());
        let text = format!(
            r#"{{"format":1,"disks":[{{"name":3}},{}]}}"#,
            entry("irix/a.chd", &good)
        );
        assert_eq!(parse(&text, BASE).unwrap().len(), 1);
        let text = format!(
            r#"{{"format":1,"disks":[{{"name":3}},{}]}}"#,
            entry("irix/a.chd", &good)
        );
        assert_eq!(parse(&text, BASE).unwrap().len(), 1);
    }

    fn disk(sha: &str) -> Disk {
        Disk {
            name: "irix/a.chd".into(),
            url: format!("{BASE}irix/a.chd"),
            sha256: sha.into(),
            size: 3,
            version: "6.5.22".into(),
            board: "ip30".into(),
            machines: vec!["octane".into()],
        }
    }

    #[test]
    fn a_download_that_fails_its_check_is_deleted_and_a_good_one_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let (part, dest) = (dir.path().join("a.part"), dir.path().join("a.chd"));
        let mut go = |_: Event| true;
        std::fs::write(&part, b"abc").unwrap();
        let bad = disk(&"0".repeat(64));
        assert!(matches!(
            finish(&bad, &part, &dest, &mut go),
            Err(DownloadError::Failed(_))
        ));
        assert!(!part.exists() && !dest.exists());
        std::fs::write(&part, b"abc").unwrap();
        let sha = crate::sha256_hex(b"abc");
        finish(&disk(&sha), &part, &dest, &mut go).unwrap();
        assert!(dest.exists() && !part.exists());
    }

    #[test]
    fn a_stalled_download_fails_with_a_retry_message_and_keeps_its_part() {
        struct Stall(bool);
        impl Read for Stall {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                if !self.0 {
                    self.0 = true;
                    buf[..3].copy_from_slice(b"abc");
                    return Ok(3);
                }
                std::thread::sleep(std::time::Duration::from_secs(5));
                Ok(0)
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.part");
        let mut file = std::fs::File::create(&path).unwrap();
        let rx = spawn_reader(Stall(false));
        let idle = std::time::Duration::from_millis(200);
        let r = stream(rx, &mut file, 0, 10, idle, &mut |_| true);
        let Err(DownloadError::Failed(text)) = r else {
            panic!("{r:?}")
        };
        assert!(text.contains("Try again"), "{text}");
        assert_eq!(std::fs::read(&path).unwrap(), b"abc");
        // A cancel is heard well before the idle limit.
        let rx = spawn_reader(Stall(true));
        let start = std::time::Instant::now();
        let long = std::time::Duration::from_secs(60);
        let r = stream(rx, &mut file, 0, 10, long, &mut |_| {
            start.elapsed() < std::time::Duration::from_millis(300)
        });
        assert!(matches!(r, Err(DownloadError::Cancelled)));
        assert!(start.elapsed() < std::time::Duration::from_secs(3));
    }

    #[test]
    fn a_body_that_ends_early_keeps_its_part_and_says_to_try_again() {
        let dir = tempfile::tempdir().unwrap();
        let mut file = std::fs::File::create(dir.path().join("a.part")).unwrap();
        let rx = spawn_reader(std::io::Cursor::new(b"abc".to_vec()));
        let idle = std::time::Duration::from_secs(5);
        let r = stream(rx, &mut file, 0, 10, idle, &mut |_| true);
        let Err(DownloadError::Failed(text)) = r else {
            panic!("{r:?}")
        };
        assert!(
            text.contains("ended early") && text.contains("Try again"),
            "{text}"
        );
        assert_eq!(std::fs::read(dir.path().join("a.part")).unwrap(), b"abc");
    }

    #[test]
    fn a_disk_is_not_removed_while_it_is_downloaded_or_copied() {
        let root = tempfile::tempdir().unwrap();
        let key = "b".repeat(64);
        let dir = root.path().join(&key);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.chd"), b"abc").unwrap();
        let copying = lock_in(&dir, false).unwrap();
        // A second copy may share it; a download or a removal may not.
        drop(lock_in(&dir, false).unwrap());
        assert!(lock_in(&dir, true).is_err());
        let refused = remove_in(root.path(), &key).unwrap_err().to_string();
        assert!(refused.contains("being downloaded"), "{refused}");
        assert!(dir.exists());
        drop(copying);
        remove_in(root.path(), &key).unwrap();
        assert!(!dir.exists());
    }

    #[test]
    fn only_a_digest_names_a_cached_disk_to_remove() {
        assert!(remove("../..").is_err());
        assert!(remove("").is_err());
    }
}
