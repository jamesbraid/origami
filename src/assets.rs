use crate::Result;
use serde::Deserialize;
use sha2::Digest;
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

pub fn manifest() -> Result<Manifest> {
    Ok(toml::from_str(include_str!("../resources/proms.toml"))?)
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

fn acquire_from(prom: &Prom, base: &str, cache: &Path, client: &ureq::Agent) -> Result<PathBuf> {
    use std::io::Read;
    use std::sync::atomic::{AtomicU64, Ordering};
    std::fs::create_dir_all(cache)?;
    let final_path = cache.join(format!("{}.bin", prom.sha256));
    if std::fs::metadata(&final_path).is_ok_and(|m| m.len() == prom.size)
        && crate::sha256_file(&final_path)? == prom.sha256
    {
        return Ok(final_path);
    }
    eprintln!("Downloading PROM {} ({} bytes)", prom.path, prom.size);
    let response = client.get(&format!("{base}{}", prom.path)).call()?;
    if response.status() != 200 {
        return Err(format!("PROM download returned HTTP {}", response.status()).into());
    }
    // One byte past the expected size is enough to detect an oversized body.
    let mut body = Vec::new();
    response
        .into_reader()
        .take(prom.size + 1)
        .read_to_end(&mut body)?;
    if body.len() as u64 != prom.size {
        return Err("PROM download byte size does not match manifest".into());
    }
    if format!("{:x}", sha2::Sha256::digest(&body)) != prom.sha256 {
        return Err("PROM download SHA-256 does not match manifest".into());
    }
    // Concurrent creates may both download; identical verified bytes make the
    // rename race harmless.
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let temp = cache.join(format!(
        "{}.{}.{}.tmp",
        prom.sha256,
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::write(&temp, &body)?;
    if let Err(e) = std::fs::rename(&temp, &final_path) {
        let _ = std::fs::remove_file(&temp);
        return Err(e.into());
    }
    Ok(final_path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};
    use std::fs;
    use std::io::{Read, Write};
    use std::net::TcpListener;
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
    fn embedded_manifest_covers_supported_presets() {
        let manifest = manifest().unwrap();
        // The manifest is compile-time data, so check it here rather than at runtime.
        assert_eq!(manifest.format, 1);
        validate_https_url(&manifest.base_url).unwrap();
        assert!(manifest.base_url.ends_with('/'));
        let mut seen = std::collections::HashSet::new();
        for prom in &manifest.proms {
            validate_https_url(&format!("{}{}", manifest.base_url, prom.path)).unwrap();
            assert!(prom.size > 0 && !prom.profiles.is_empty(), "{}", prom.path);
            assert!(
                prom.sha256.len() == 64
                    && prom
                        .sha256
                        .bytes()
                        .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)),
                "{}",
                prom.path
            );
            for profile in &prom.profiles {
                assert!(!profile.is_empty() && seen.insert(profile), "{profile}");
            }
        }
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
    fn rejects_invalid_downloads_and_leaves_nothing_cached() {
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
            assert_eq!(fs::read_dir(&cache.0).unwrap().count(), 0);
        }
    }
}
