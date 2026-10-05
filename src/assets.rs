use crate::Result;
use serde::Deserialize;
use sha2::Digest;
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize)]
pub struct Manifest {
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
    use std::io::{Read, Write};
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
    let mut temp = tempfile::NamedTempFile::new_in(cache)?;
    temp.write_all(&body)?;
    temp.persist(&final_path).map_err(|e| e.error)?;
    Ok(final_path)
}

#[cfg(test)]
mod tests {
    use super::*;
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
        for prom in &manifest.proms {
            validate_https_url(&format!("{}{}", manifest.base_url, prom.path)).unwrap();
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
}
