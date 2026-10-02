use crate::Result;
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub format: u32,
    pub base_url: String,
    pub proms: Vec<Prom>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Prom {
    pub path: String,
    pub size: u64,
    pub sha256: String,
    pub profiles: Vec<String>,
    #[serde(default)]
    pub version: String,
}

impl Manifest {
    pub fn parse(text: &str) -> Result<Self> {
        let manifest: Self = toml::from_str(text)?;
        if manifest.format != 1 {
            return Err(format!("unsupported PROM manifest format {}", manifest.format).into());
        }
        validate_https_url(&manifest.base_url)?;
        if !manifest.base_url.ends_with('/') {
            return Err("PROM base URL must end with /".into());
        }
        let mut profiles = std::collections::HashSet::new();
        for prom in &manifest.proms {
            validate_prom(prom)?;
            for profile in &prom.profiles {
                if profile.is_empty() || !profiles.insert(profile) {
                    return Err(format!("empty or duplicate PROM profile: {profile}").into());
                }
            }
        }
        Ok(manifest)
    }
}

pub fn manifest() -> Result<Manifest> {
    Manifest::parse(include_str!("../resources/proms.toml"))
}

pub fn validate_https_url(value: &str) -> Result<()> {
    if value
        .bytes()
        .any(|c| c.is_ascii_whitespace() || c.is_ascii_control())
    {
        return Err("media URL must not contain whitespace or control characters".into());
    }
    let url = url::Url::parse(value).map_err(|e| format!("invalid media URL: {e}"))?;
    if !value.starts_with("https://")
        || url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("media URL must use HTTPS without credentials, query or fragment".into());
    }
    // Check the original path: URL parsing normalizes traversal components.
    let authority = value.strip_prefix("https://").unwrap();
    if authority.contains('@') || authority.contains('\\') || !authority.is_ascii() {
        return Err("unsafe media URL".into());
    }
    if let Some((_, path)) = authority.split_once('/') {
        let path = path.strip_suffix('/').unwrap_or(path);
        if !path.is_empty() {
            validate_path(path)?;
        }
    }
    Ok(())
}

fn validate_path(path: &str) -> Result<()> {
    if path.is_empty()
        || !path
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"/._-".contains(&c))
        || path
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err(format!("unsafe PROM object path: {path}").into());
    }
    Ok(())
}

fn validate_prom(prom: &Prom) -> Result<()> {
    validate_path(&prom.path)?;
    if prom.size == 0
        || prom.sha256.len() != 64
        || !prom
            .sha256
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        || prom.profiles.is_empty()
    {
        return Err(format!("invalid size, SHA-256 or profiles for PROM {}", prom.path).into());
    }
    Ok(())
}

pub fn acquire(profile_id: &str) -> Result<PathBuf> {
    let manifest = manifest()?;
    let prom = manifest
        .proms
        .iter()
        .find(|prom| prom.profiles.iter().any(|p| p == profile_id))
        .ok_or_else(|| {
            format!("no downloadable PROM for preset {profile_id}; supply --prom FILE")
        })?;
    let cache = dirs::cache_dir()
        .ok_or("cannot determine user cache directory; supply --prom FILE")?
        .join("origami")
        .join("proms");
    let client = ureq::AgentBuilder::new()
        .https_only(true)
        .redirects(0)
        .timeout_connect(std::time::Duration::from_secs(15))
        .timeout(std::time::Duration::from_secs(120))
        .build();
    acquire_from(prom, &manifest.base_url, &cache, &client)
        .map_err(|e| format!("cannot acquire PROM {}: {e}; offline creation requires a verified cache entry or --prom FILE", prom.path).into())
}

fn verified(path: &Path, prom: &Prom) -> Result<bool> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e.into()),
    };
    if file.metadata()?.len() != prom.size {
        return Ok(false);
    }
    let mut hash = Sha256::new();
    let mut bytes = [0; 16384];
    let mut size = 0;
    loop {
        let count = file.read(&mut bytes)?;
        if count == 0 {
            break;
        }
        size += count as u64;
        if size > prom.size {
            return Ok(false);
        }
        hash.update(&bytes[..count]);
    }
    Ok(size == prom.size && format!("{:x}", hash.finalize()) == prom.sha256)
}

fn acquire_from(prom: &Prom, base: &str, cache: &Path, client: &ureq::Agent) -> Result<PathBuf> {
    use fs2::FileExt;
    use sha2::{Digest, Sha256};
    use std::fs::{self, OpenOptions};
    use std::io::{Read, Write};
    validate_prom(prom)?;
    fs::create_dir_all(cache)?;
    let final_path = cache.join(format!("{}.bin", prom.sha256));
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(cache.join(format!("{}.lock", prom.sha256)))?;
    lock.lock_exclusive()?;
    if verified(&final_path, prom)? {
        return Ok(final_path);
    }
    let partial = cache.join(format!("{}.part", prom.sha256));
    for path in [&final_path, &partial] {
        match fs::remove_file(path) {
            Ok(()) => (),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(e) => return Err(e.into()),
        }
    }
    eprintln!("Downloading PROM {} ({} bytes)", prom.path, prom.size);
    let result = (|| -> Result<()> {
        let response = client.get(&format!("{base}{}", prom.path)).call()?;
        if response.status() != 200 {
            return Err(format!("PROM download returned HTTP {}", response.status()).into());
        }
        if let Some(length) = response.header("Content-Length") {
            if length.parse::<u64>()? != prom.size {
                return Err("PROM download byte size does not match manifest".into());
            }
        }
        let mut reader = response.into_reader();
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&partial)?;
        let mut hash = Sha256::new();
        let mut total = 0;
        let mut bytes = [0; 16384];
        loop {
            let limit = (prom.size.saturating_sub(total).saturating_add(1)).min(bytes.len() as u64)
                as usize;
            let count = reader.read(&mut bytes[..limit])?;
            if count == 0 {
                break;
            }
            total += count as u64;
            if total > prom.size {
                return Err("PROM download exceeds manifest byte size".into());
            }
            file.write_all(&bytes[..count])?;
            hash.update(&bytes[..count]);
        }
        if total != prom.size {
            return Err("PROM download is truncated".into());
        }
        if format!("{:x}", hash.finalize()) != prom.sha256 {
            return Err("PROM download SHA-256 does not match manifest".into());
        }
        file.sync_all()?;
        drop(file);
        fs::rename(&partial, &final_path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&partial);
    }
    result?;
    Ok(final_path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};
    use std::fs;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Barrier};
    use std::thread;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct Scratch(PathBuf);
    impl Scratch {
        fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "origami-prom-{}-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn prom() -> Prom {
        Prom {
            path: "prom/test.bin".into(),
            size: 4,
            sha256: format!("{:x}", Sha256::digest(b"test")),
            profiles: vec!["test-profile".into()],
            version: String::new(),
        }
    }
    fn server(response: &'static [u8]) -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}/", listener.local_addr().unwrap());
        let handle = thread::spawn(move || {
            listener.set_nonblocking(true).unwrap();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
            let mut socket = loop {
                match listener.accept() {
                    Ok((socket, _)) => break socket,
                    Err(e)
                        if e.kind() == std::io::ErrorKind::WouldBlock
                            && std::time::Instant::now() < deadline =>
                    {
                        thread::sleep(std::time::Duration::from_millis(10))
                    }
                    Err(e) => panic!("test server did not receive request: {e}"),
                }
            };
            socket.set_nonblocking(false).unwrap();
            socket
                .set_read_timeout(Some(std::time::Duration::from_secs(3)))
                .unwrap();
            let mut request = [0; 4096];
            let count = socket.read(&mut request).unwrap();
            assert!(std::str::from_utf8(&request[..count])
                .unwrap()
                .starts_with("GET /prom/test.bin "));
            thread::sleep(std::time::Duration::from_millis(50));
            let _ = socket.write_all(response);
        });
        (base, handle)
    }
    fn client() -> ureq::Agent {
        ureq::AgentBuilder::new().redirects(0).build()
    }
    #[test]
    fn rejects_unsafe_urls() {
        for value in [
            "http://example.org/prom.bin",
            "https://exam\nple.org/prom.bin",
            "https://example.org\t/prom.bin",
            "https://user:pass@example.org/prom.bin",
            "https://example.org/prom.bin#part",
            "https://example.org/prom.bin?q=1",
            "https://example.org/../prom.bin",
            "https://example.org/%2e%2e/prom.bin",
        ] {
            assert!(validate_https_url(value).is_err(), "{value}");
        }
        validate_https_url("https://origami-dist.irix.fans/prom/test.bin").unwrap();
    }
    #[test]
    fn manifest_rejects_unsafe_paths_and_invalid_pins() {
        for path in [
            "/prom.bin",
            "../prom.bin",
            "prom/../test.bin",
            "prom//test.bin",
            "prom/./test.bin",
            "prom/test%2ebin",
            "https://host/prom.bin",
            "prom/test\\bin",
            "prom/test bin",
            "prom/test.bin?x=1",
        ] {
            let mut entry = prom();
            entry.path = path.into();
            assert!(validate_prom(&entry).is_err(), "{path}");
        }
        let valid = format!("format = 1\nbase_url = \"https://origami-dist.irix.fans/\"\n[[proms]]\npath = \"prom/test.bin\"\nsize = 4\nsha256 = \"{}\"\nprofiles = [\"test-profile\"]\n", prom().sha256);
        for invalid in [
            valid.replace("format = 1", "format = 2"),
            valid.replace("size = 4", "size = 0"),
            valid.replace(&prom().sha256, "not-a-hash"),
            valid.replace(
                "profiles = [\"test-profile\"]",
                "profiles = [\"test-profile\", \"test-profile\"]",
            ),
            valid.replace("https://", "http://"),
            format!("{valid}unknown = true\n"),
        ] {
            assert!(Manifest::parse(&invalid).is_err(), "{invalid}");
        }
    }
    #[test]
    fn embedded_manifest_covers_supported_presets() {
        let manifest = manifest().unwrap();
        for name in [
            "origin200-1",
            "origin200-2",
            "origin200-dual",
            "origin2000-8",
            "origin300-2",
        ] {
            assert_eq!(
                manifest
                    .proms
                    .iter()
                    .filter(|prom| prom.profiles.iter().any(|p| p == name))
                    .count(),
                1,
                "{name}"
            );
        }
    }
    #[test]
    fn failed_download_can_retry_after_stale_partial() {
        let cache = Scratch::new();
        fs::write(cache.0.join(format!("{}.part", prom().sha256)), b"stale").unwrap();
        let (base, handle) =
            server(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\ntest");
        let path = acquire_from(&prom(), &base, &cache.0, &client()).unwrap();
        handle.join().unwrap();
        assert_eq!(fs::read(path).unwrap(), b"test");
    }
    #[test]
    fn process_cache_worker() {
        let Ok(cache) = std::env::var("ORIGAMI_TEST_PROM_CACHE") else {
            return;
        };
        let base = std::env::var("ORIGAMI_TEST_PROM_BASE").unwrap();
        let path = acquire_from(&prom(), &base, Path::new(&cache), &client()).unwrap();
        assert_eq!(fs::read(path).unwrap(), b"test");
    }
    #[test]
    fn separate_processes_share_verified_cache() {
        let cache = Scratch::new();
        let (base, handle) =
            server(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\ntest");
        let mut children: Vec<_> = (0..2)
            .map(|_| {
                std::process::Command::new(std::env::current_exe().unwrap())
                    .args(["--exact", "assets::tests::process_cache_worker"])
                    .env("ORIGAMI_TEST_PROM_CACHE", &cache.0)
                    .env("ORIGAMI_TEST_PROM_BASE", &base)
                    .stdout(std::process::Stdio::null())
                    .spawn()
                    .unwrap()
            })
            .collect();
        for child in &mut children {
            assert!(child.wait().unwrap().success());
        }
        handle.join().unwrap();
    }
    #[test]
    fn parses_profile_mapping() {
        let text = format!("format = 1\nbase_url = \"https://origami-dist.irix.fans/\"\n[[proms]]\npath = \"prom/test.bin\"\nsize = 4\nsha256 = \"{}\"\nprofiles = [\"test-profile\"]\n", prom().sha256);
        let manifest = Manifest::parse(&text).unwrap();
        assert_eq!(manifest.proms[0].profiles, ["test-profile"]);
    }
    #[test]
    fn downloads_and_reuses_verified_cache_offline() {
        let cache = Scratch::new();
        let (base, handle) =
            server(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\ntest");
        let path = acquire_from(&prom(), &base, &cache.0, &client()).unwrap();
        handle.join().unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"test");
        assert_eq!(
            acquire_from(&prom(), "http://127.0.0.1:1/", &cache.0, &client()).unwrap(),
            path
        );
    }
    #[test]
    fn corrupt_cache_is_replaced() {
        let cache = Scratch::new();
        fs::write(cache.0.join(format!("{}.bin", prom().sha256)), b"oops").unwrap();
        let (base, handle) =
            server(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\ntest");
        let path = acquire_from(&prom(), &base, &cache.0, &client()).unwrap();
        handle.join().unwrap();
        assert_eq!(fs::read(path).unwrap(), b"test");
    }
    #[test]
    fn rejects_invalid_downloads_and_removes_partial_files() {
        for response in [
            b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\nte".as_slice(),
            b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\ntestextra".as_slice(),
            b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\noops".as_slice(),
            b"HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:1/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".as_slice(),
        ] {
            let cache = Scratch::new();
            let (base, handle) = server(response);
            assert!(acquire_from(&prom(), &base, &cache.0, &client()).is_err());
            handle.join().unwrap();
            assert!(!cache.0.join(format!("{}.bin", prom().sha256)).exists());
            assert!(!cache.0.join(format!("{}.part", prom().sha256)).exists());
        }
    }
    #[test]
    fn concurrent_acquisitions_share_one_download() {
        let cache = Scratch::new();
        let (base, handle) =
            server(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\ntest");
        let barrier = Arc::new(Barrier::new(2));
        let workers: Vec<_> = (0..2)
            .map(|_| {
                let cache = cache.0.clone();
                let base = base.clone();
                let barrier = barrier.clone();
                thread::spawn(move || {
                    barrier.wait();
                    acquire_from(&prom(), &base, &cache, &client()).map_err(|e| e.to_string())
                })
            })
            .collect();
        let paths: Vec<_> = workers
            .into_iter()
            .map(|w| w.join().unwrap().unwrap())
            .collect();
        handle.join().unwrap();
        assert_eq!(paths[0], paths[1]);
    }
}
