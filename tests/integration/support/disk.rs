//! The prepared IRIX disk the tests boot: a pinned download from
//! origami-dist, verified by SHA-256 and unpacked once.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use sha2::{Digest, Sha256};

use super::product::run_limited;
use super::{pause, secs, Result};

pub struct Pin {
    pub name: &'static str,
    pub url: &'static str,
    pub sha256: &'static str,
}

/// An Origin 200 (preset origin200-1) with IRIX 6.5.30, the minimal install
/// profile, a 4096 MB system disk, and the install network's DHCP lease and
/// resolver file removed so it takes an address on any network.
pub const O200_MINIMAL: Pin = Pin {
    name: "o200-irix-6.5.30-minimal-7bfddb0f.tar.xz",
    url: "https://origami-dist.irix.fans/irix/6.5.30/o200-irix-6.5.30-minimal-7bfddb0f.tar.xz",
    sha256: "7bfddb0f43787df4f428e1df21efa553158b2fe33ccaf83cd93a069fd69f9efb",
};

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut h = Sha256::new();
    let mut f = fs::File::open(path)?;
    let mut block = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut block)?;
        if n == 0 {
            return Ok(hex(&h.finalize()));
        }
        h.update(&block[..n]);
    }
}

/// Download `pin` into `dir` unless a verified copy is already there.
pub fn fetch(pin: &Pin, dir: &Path) -> Result<PathBuf> {
    fs::create_dir_all(dir)?;
    let path = dir.join(pin.name);
    if path.exists() && sha256_file(&path)? == pin.sha256 {
        return Ok(path);
    }
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .https_only(true)
        .timeout_connect(Some(secs(30)))
        .timeout_global(Some(Duration::from_secs(3600)))
        .build()
        .into();
    let mut last = String::new();
    for attempt in 1..=4u64 {
        println!("downloading {} (attempt {attempt})", pin.url);
        let part = dir.join(format!("{}.part", pin.name));
        let result: Result<()> = (|| {
            let response = agent.get(pin.url).call()?;
            let mut body = response.into_body().into_reader();
            let mut out = fs::File::create(&part)?;
            let mut h = Sha256::new();
            let mut block = vec![0u8; 1 << 20];
            loop {
                let n = body.read(&mut block)?;
                if n == 0 {
                    break;
                }
                h.update(&block[..n]);
                out.write_all(&block[..n])?;
            }
            let got = hex(&h.finalize());
            if got != pin.sha256 {
                return Err(format!("sha256 {got} is not the pinned {}", pin.sha256).into());
            }
            Ok(())
        })();
        match result {
            Ok(()) => {
                fs::rename(&part, &path)?;
                return Ok(path);
            }
            Err(e) => {
                last = e.to_string();
                println!("download failed: {last}");
                pause(secs(5 * attempt));
            }
        }
    }
    Err(format!("download failed: {last}").into())
}

/// The prepared Origin 200 machine directory, downloaded and unpacked into
/// `cache` the first time. Guests build copy-on-write overlays on its disk,
/// which is never written.
pub fn prepared_machine(cache: &Path) -> Result<PathBuf> {
    let unpacked = cache.join(format!("unpacked-{}", &O200_MINIMAL.sha256[..8]));
    if unpacked.join("DONE").exists() {
        return Ok(unpacked.join("machine"));
    }
    let archive = fetch(&O200_MINIMAL, cache)?;
    let _ = fs::remove_dir_all(&unpacked);
    fs::create_dir_all(&unpacked)?;
    // Relative paths and no drive letters: GNU tar reads "C:" as a host.
    // On Windows, Git's bash puts GNU tar first, and it needs an xz binary
    // Git does not always ship. System32's tar is bsdtar, which has xz built in.
    let tar = if cfg!(windows) {
        PathBuf::from(std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into()))
            .join("System32/tar.exe")
    } else {
        PathBuf::from("tar")
    };
    let mut cmd = Command::new(tar);
    cmd.current_dir(cache)
        .arg("-xJf")
        .arg(archive.file_name().unwrap())
        .arg("-C")
        .arg(unpacked.file_name().unwrap());
    let out = run_limited(cmd, secs(1200))?;
    if out.code != 0 {
        return Err(format!("tar: {}", out.text()).into());
    }
    let machine = unpacked.join("machine");
    if !machine.join("machine.toml").exists() {
        return Err(format!("{} holds no machine/machine.toml", O200_MINIMAL.name).into());
    }
    fs::write(unpacked.join("DONE"), "ok\n")?;
    Ok(machine)
}

/// Where downloads and unpacked disks are kept between tests.
pub fn cache_dir() -> PathBuf {
    match std::env::var_os("ORIGAMI_DISK_CACHE") {
        Some(d) => PathBuf::from(d),
        None => {
            PathBuf::from(std::env::var_os("ORIGAMI_WORK").expect("ORIGAMI_WORK")).join("disks")
        }
    }
}
