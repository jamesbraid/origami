//! Downloading, verifying, unpacking and writing release archives.

use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

pub fn sha256_file(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; 1 << 20];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

pub fn http_get(url: &str) -> Result<Vec<u8>> {
    let mut response = ureq::get(url)
        .header("User-Agent", "sgi-emulator-xtask")
        .call()
        .map_err(|error| format!("GET {url}: {error}"))?;
    let mut body = Vec::new();
    response
        .body_mut()
        .with_config()
        .limit(1 << 30)
        .reader()
        .read_to_end(&mut body)?;
    Ok(body)
}

/// Downloads `url` into `cache` unless a file with the wanted hash is
/// already there, and returns its path.
pub fn fetch(url: &str, sha256: &str, cache: &Path) -> Result<PathBuf> {
    fs::create_dir_all(cache)?;
    let name = url.rsplit('/').next().unwrap_or("download");
    let path = cache.join(format!("{}-{name}", &sha256[..12]));
    if path.is_file() && sha256_file(&path)? == sha256 {
        return Ok(path);
    }
    eprintln!("downloading {url}");
    let body = http_get(url)?;
    let actual = sha256_hex(&body);
    if actual != sha256 {
        return Err(format!("{url}: sha256 is {actual}, runtime.lock says {sha256}").into());
    }
    let partial = path.with_extension("part");
    fs::write(&partial, &body)?;
    fs::rename(&partial, &path)?;
    Ok(path)
}

/// Unpacks a `.tar.gz` or `.zip` into the empty or missing `dest`.
pub fn unpack(archive: &Path, dest: &Path) -> Result<()> {
    if dest.exists() {
        fs::remove_dir_all(dest)?;
    }
    fs::create_dir_all(dest)?;
    let name = archive.to_string_lossy();
    if name.ends_with(".zip") {
        zip::ZipArchive::new(File::open(archive)?)?.extract(dest)?;
    } else if name.ends_with(".tar.gz") {
        let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(File::open(archive)?));
        tar.set_preserve_permissions(true);
        tar.unpack(dest)?;
    } else {
        return Err(format!("{name}: not a .tar.gz or .zip").into());
    }
    Ok(())
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) -> io::Result<()> {
    let mut entries: Vec<_> = fs::read_dir(dir)?.collect::<io::Result<_>>()?;
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let path = entry.path();
        out.push(path.strip_prefix(root).unwrap().to_path_buf());
        if entry.file_type()?.is_dir() {
            walk(root, &path, out)?;
        }
    }
    Ok(())
}

/// Every file and directory under `root`, relative, parents first.
pub fn list(root: &Path) -> io::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    walk(root, root, &mut out)?;
    Ok(out)
}

fn unix_mode(path: &Path) -> Option<u32> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::metadata(path)
            .ok()
            .map(|m| m.permissions().mode() & 0o7777)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}

/// Writes `root`'s contents under a `top/` directory into `archive`, as a
/// `.tar.gz` or `.zip` by its extension. Times are fixed so identical
/// trees give identical archives.
pub fn pack(root: &Path, top: &str, archive: &Path) -> Result<()> {
    let files = list(root)?;
    let out = File::create(archive)?;
    if archive.extension().is_some_and(|e| e == "zip") {
        let mut zip = zip::ZipWriter::new(out);
        let mut dir_options = zip::write::SimpleFileOptions::default();
        dir_options = dir_options.last_modified_time(zip::DateTime::default());
        zip.add_directory(format!("{top}/"), dir_options)?;
        for rel in files {
            let path = root.join(&rel);
            let name = format!("{top}/{}", rel.to_string_lossy().replace('\\', "/"));
            let mut options = dir_options.compression_method(zip::CompressionMethod::Deflated);
            if let Some(mode) = unix_mode(&path) {
                options = options.unix_permissions(mode);
            }
            if path.is_dir() {
                zip.add_directory(format!("{name}/"), options)?;
            } else {
                zip.start_file(name, options)?;
                io::copy(&mut File::open(&path)?, &mut zip)?;
            }
        }
        zip.finish()?;
    } else {
        let gz = flate2::write::GzEncoder::new(out, flate2::Compression::best());
        let mut tar = tar::Builder::new(gz);
        tar.mode(tar::HeaderMode::Deterministic);
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Directory);
        header.set_mode(0o755);
        header.set_size(0);
        header.set_mtime(0);
        tar.append_data(&mut header, format!("{top}/"), io::empty())?;
        for rel in files {
            let path = root.join(&rel);
            let name = format!("{top}/{}", rel.to_string_lossy().replace('\\', "/"));
            if path.is_dir() {
                tar.append_dir(&name, &path)?;
            } else {
                tar.append_path_with_name(&path, &name)?;
            }
        }
        tar.into_inner()?.finish()?.flush()?;
    }
    Ok(())
}

/// Copies `from` to `to`, creating parents and keeping the mode.
pub fn copy_file(from: &Path, to: &Path) -> io::Result<()> {
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::copy(from, to).map(|_| ())
}

pub fn copy_tree(from: &Path, to: &Path) -> io::Result<()> {
    fs::create_dir_all(to)?;
    for rel in list(from)? {
        let source = from.join(&rel);
        if source.is_dir() {
            fs::create_dir_all(to.join(&rel))?;
        } else {
            copy_file(&source, &to.join(&rel))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("xtask-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn archives_round_trip_with_a_top_directory() {
        for ext in ["tar.gz", "zip"] {
            let dir = scratch(&format!("pack-{ext}"));
            let tree = dir.join("tree");
            fs::create_dir_all(tree.join("bin")).unwrap();
            fs::write(tree.join("bin/tool"), b"#!/bin/sh\n").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(tree.join("bin/tool"), fs::Permissions::from_mode(0o755))
                    .unwrap();
            }
            let archive = dir.join(format!("out.{ext}"));
            pack(&tree, "origami-1", &archive).unwrap();
            let back = dir.join("back");
            unpack(&archive, &back).unwrap();
            assert_eq!(
                fs::read(back.join("origami-1/bin/tool")).unwrap(),
                b"#!/bin/sh\n"
            );
            #[cfg(unix)]
            assert_eq!(unix_mode(&back.join("origami-1/bin/tool")), Some(0o755));
            fs::remove_dir_all(dir).unwrap();
        }
    }
}
