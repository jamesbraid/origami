//! Driving the built product: its CLI, the disk downloads and the evidence a
//! boot leaves behind.

use std::ffi::OsStr;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

/// An extracted product tree (`bin/origami`, `libexec/origami/`), with a home
/// of its own so its PROM cache and settings stay out of the runner's profile.
pub struct Product {
    cli: PathBuf,
    env: Vec<(&'static str, PathBuf)>,
    work: PathBuf,
}

impl Product {
    pub fn new(root: &Path, work: &Path) -> Self {
        let exe = if cfg!(windows) {
            "origami.exe"
        } else {
            "origami"
        };
        let home = work.join("home");
        fs::create_dir_all(&home).unwrap();
        Self {
            // Not canonicalize: on Windows that gives a \\?\ path QEMU's data lookup may not follow.
            cli: std::path::absolute(root)
                .unwrap_or_else(|_| root.into())
                .join("bin")
                .join(exe),
            env: vec![
                ("HOME", home.clone()),
                ("USERPROFILE", home.clone()),
                ("XDG_CACHE_HOME", work.join("xdg")),
                ("LOCALAPPDATA", home.join("local")),
                ("APPDATA", home.join("roaming")),
            ],
            work: work.into(),
        }
    }

    pub fn origami<I, S>(&self, args: I) -> Result<Output, String>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let args: Vec<_> = args.into_iter().collect();
        let mut command = Command::new(&self.cli);
        command.args(&args).current_dir(&self.work);
        for (name, value) in &self.env {
            command.env(name, value);
        }
        // The archive is self-contained; a stray override would test something else.
        for name in [
            "ORIGAMI_RUNTIME_DIR",
            "LD_LIBRARY_PATH",
            "DYLD_LIBRARY_PATH",
        ] {
            command.env_remove(name);
        }
        let output = command
            .output()
            .map_err(|e| format!("cannot run {}: {e}", self.cli.display()))?;
        let words: Vec<_> = args
            .iter()
            .map(|a| a.as_ref().to_string_lossy().into_owned())
            .collect();
        let text = String::from_utf8_lossy(&output.stdout).into_owned()
            + &String::from_utf8_lossy(&output.stderr);
        let tail: String = text
            .trim()
            .chars()
            .rev()
            .take(300)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        eprintln!(
            "+ origami {} -> {} {}",
            words.join(" "),
            output.status.code().unwrap_or(-1),
            tail
        );
        Ok(output)
    }

    /// Run the CLI and fail on a nonzero status, with its error text.
    pub fn ok<I, S>(&self, args: I) -> Result<String, String>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let args: Vec<_> = args.into_iter().collect();
        let first = args
            .first()
            .map(|a| a.as_ref().to_string_lossy().into_owned())
            .unwrap_or_default();
        let output = self.origami(&args)?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!("origami {first}: {}", stderr.trim()));
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    pub fn is_stopped(&self, machine: &Path) -> bool {
        self.origami([OsStr::new("status"), machine.as_os_str()])
            .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "stopped")
            .unwrap_or(false)
    }
}

/// The machine is stopped when the guard goes, whatever the test did.
pub struct RunningGuard<'a> {
    pub product: &'a Product,
    pub machine: PathBuf,
}

impl Drop for RunningGuard<'_> {
    fn drop(&mut self) {
        if !self.product.is_stopped(&self.machine) {
            let _ = self
                .product
                .origami([OsStr::new("stop"), self.machine.as_os_str()]);
        }
    }
}

pub fn sha256_file(path: &Path) -> std::io::Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut block = vec![0u8; 1 << 20];
    loop {
        let count = file.read(&mut block)?;
        if count == 0 {
            break;
        }
        hash.update(&block[..count]);
    }
    Ok(hex(&hash.finalize()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The disk at `url` in `dir`, downloaded and checked against `sha256` unless
/// a file with that hash is already there.
pub fn fetch(url: &str, sha256: &str, dir: &Path) -> Result<PathBuf, String> {
    fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let name = url.rsplit('/').next().unwrap_or("disk");
    let path = dir.join(name);
    if path.exists() && sha256_file(&path).map_err(|e| e.to_string())? == sha256 {
        return Ok(path);
    }
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(30)))
        .timeout_recv_response(Some(Duration::from_secs(60)))
        .timeout_recv_body(Some(Duration::from_secs(3600)))
        .build()
        .into();
    let mut last = String::new();
    for attempt in 1..=4 {
        eprintln!("downloading {url} (attempt {attempt})");
        match download(&agent, url, &path) {
            Ok(got) if got == sha256 => return Ok(path),
            Ok(got) => last = format!("sha256 {got} is not the pinned {sha256}"),
            Err(e) => last = e,
        }
        eprintln!("download failed: {last}");
        std::thread::sleep(Duration::from_secs(5 * attempt));
    }
    Err(format!("download of {url} failed: {last}"))
}

fn download(agent: &ureq::Agent, url: &str, path: &Path) -> Result<String, String> {
    let response = agent.get(url).call().map_err(|e| e.to_string())?;
    let mut body = response.into_body().into_reader();
    let mut file = fs::File::create(path).map_err(|e| e.to_string())?;
    let mut hash = Sha256::new();
    let mut block = vec![0u8; 1 << 20];
    let started = Instant::now();
    loop {
        let count = body.read(&mut block).map_err(|e| e.to_string())?;
        if count == 0 {
            break;
        }
        hash.update(&block[..count]);
        file.write_all(&block[..count]).map_err(|e| e.to_string())?;
    }
    eprintln!("downloaded {url} in {}s", started.elapsed().as_secs());
    Ok(hex(&hash.finalize()))
}
