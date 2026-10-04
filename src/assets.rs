use crate::Result;
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Deserialize)]
pub struct Manifest {
    pub base_url: String,
    pub proms: Vec<Prom>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Prom {
    pub id: String,
    pub path: String,
    pub size: u64,
    pub sha256: String,
    #[serde(default)]
    pub version: String,
}

pub fn manifest() -> Result<Manifest> {
    Ok(toml::from_str(include_str!("../resources/proms.toml"))?)
}

impl Manifest {
    pub fn get(&self, id: &str) -> Result<&Prom> {
        self.proms
            .iter()
            .find(|prom| prom.id == id)
            .ok_or_else(|| format!("no PROM {id} in the registry").into())
    }
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

/// Download the registry's PROM `id`, or reuse a verified cached copy.
pub fn acquire(id: &str) -> Result<PathBuf> {
    use std::io::{Read, Write};
    let manifest = manifest()?;
    let prom = manifest.get(id)?;
    let name = prom.path.rsplit('/').next().unwrap_or(&prom.path);
    // Each image keeps SGI's file name, under a directory named by its digest.
    let cache = dirs::cache_dir()
        .ok_or("cannot determine user cache directory; supply local PROM files")?
        .join("origami")
        .join("proms")
        .join(&prom.sha256);
    let fetch = || -> Result<PathBuf> {
        std::fs::create_dir_all(&cache)?;
        let path = cache.join(name);
        if std::fs::metadata(&path).is_ok_and(|m| m.len() == prom.size)
            && crate::sha256_file(&path)? == prom.sha256
        {
            return Ok(path);
        }
        eprintln!(
            "Downloading PROM {name} (version {}, {} bytes)",
            prom.version, prom.size
        );
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .https_only(true)
            .max_redirects(0)
            .timeout_connect(Some(std::time::Duration::from_secs(15)))
            .timeout_global(Some(std::time::Duration::from_secs(120)))
            .build()
            .into();
        let mut response = agent
            .get(&format!("{}{}", manifest.base_url, prom.path))
            .call()?;
        // Redirects are refused, so a 3xx response arrives here rather than as an error.
        if response.status() != 200 {
            return Err(format!("PROM download returned HTTP {}", response.status()).into());
        }
        let mut body = Vec::new();
        response.body_mut().as_reader().read_to_end(&mut body)?;
        if crate::sha256_hex(&body) != prom.sha256 {
            return Err("PROM download does not match the manifest's SHA-256".into());
        }
        // Concurrent creates may both download; identical verified bytes make
        // the rename race harmless.
        let mut temp = tempfile::NamedTempFile::new_in(&cache)?;
        temp.write_all(&body)?;
        temp.persist(&path).map_err(|e| e.error)?;
        Ok(path)
    };
    fetch().map_err(|e| format!("cannot acquire PROM {name}: {e}; offline creation requires a verified cache entry or a local PROM file").into())
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
    fn embedded_registry_resolves_every_preset_selection() {
        let manifest = manifest().unwrap();
        for prom in &manifest.proms {
            validate_https_url(&format!("{}{}", manifest.base_url, prom.path)).unwrap();
        }
        for profile in crate::profiles::STARTERS {
            for id in std::iter::once(profile.boot_prom).chain(profile.io_prom) {
                assert!(manifest.get(id).is_ok(), "{}", profile.id);
            }
        }
    }
}
