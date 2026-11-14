//! A guest machine run by the product, and the commands a test types at it.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use regex::Regex;

use super::capture;
use super::cleanup::{self, Registration};
use super::console::{self, Console};
use super::product::Product;
use super::{pause, secs, Result};

pub struct Guest {
    product: Product,
    pub name: String,
    pub mac: String,
    pub dir: PathBuf,
    con: Option<Console>,
    _registration: Registration,
}

/// Copy a machine directory without the files that belong to one run or
/// are rebuilt for the copy.
fn copy_machine(from: &Path, to: &Path) -> Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let name = entry.file_name();
        if matches!(
            name.to_str(),
            Some("system.qcow2" | "machine.lock" | "run.json")
        ) {
            continue;
        }
        let src = entry.path();
        let dst = to.join(&name);
        if entry.file_type()?.is_dir() {
            copy_machine(&src, &dst)?;
        } else {
            fs::copy(&src, &dst)?;
        }
    }
    Ok(())
}

/// `mac = "..."` in a machine's `machine.toml`, replaced.
pub fn set_mac(machine: &Path, mac: &str) -> Result<()> {
    let path = machine.join("machine.toml");
    let text = fs::read_to_string(&path)?;
    let rx = Regex::new(r#"(?m)^mac = ".*"$"#)?;
    if !rx.is_match(&text) {
        return Err(format!("{} has no mac setting", path.display()).into());
    }
    fs::write(
        &path,
        rx.replace(&text, format!("mac = \"{mac}\"").as_str())
            .as_ref(),
    )?;
    Ok(())
}

/// The MAC address in a machine's `machine.toml`.
pub fn mac_of(machine: &Path) -> Result<String> {
    let doc: toml::Table = fs::read_to_string(machine.join("machine.toml"))?.parse()?;
    Ok(doc["identity"]["mac"]
        .as_str()
        .ok_or("no identity.mac")?
        .to_lowercase())
}

impl Guest {
    /// A new machine that is a copy-on-write overlay of the prepared machine
    /// `source`; the original disk is never written. The copy is a clone and
    /// keeps the original's MAC, so it gets its own: machines on one LAN
    /// need distinct ones.
    pub fn create(product: &Product, source: &Path, name: &str, mac: &str) -> Result<Guest> {
        let dir = product.machines().join(name);
        let _ = fs::remove_dir_all(&dir);
        copy_machine(source, &dir)?;
        fs::create_dir_all(dir.join("drives"))?;
        let backing = source.join("drives/system.qcow2");
        product.qemu_img([
            "create".as_ref(),
            "-q".as_ref(),
            "-f".as_ref(),
            "qcow2".as_ref(),
            "-b".as_ref(),
            backing.as_os_str(),
            "-F".as_ref(),
            "qcow2".as_ref(),
            dir.join("drives/system.qcow2").as_os_str(),
        ])?;
        set_mac(&dir, mac)?;
        Ok(Self::adopt(product, name, mac, dir))
    }

    /// A guest for a machine directory that already exists.
    pub fn adopt(product: &Product, name: &str, mac: &str, dir: PathBuf) -> Guest {
        let (p, d, n) = (product.clone(), dir.clone(), name.to_string());
        let registration = cleanup::register(&format!("guest {name}"), move || {
            // A signal means a cancelled or timed-out job. Keep what shows
            // where the guest was, unless a newer cleanup already did.
            if d.join("state/run.json").exists()
                && !p.evidence().join(format!("{n}-failure")).exists()
            {
                capture::save(&p.evidence(), &n, &d, None);
            }
            stop_machine(&p, &d, None, &n);
        });
        Guest {
            product: product.clone(),
            name: name.into(),
            mac: mac.into(),
            dir,
            con: None,
            _registration: registration,
        }
    }

    pub fn network(&self, args: &[&str]) -> Result<()> {
        let mut all: Vec<&std::ffi::OsStr> = vec!["network-set".as_ref(), self.dir.as_os_str()];
        all.extend(args.iter().map(|a| std::ffi::OsStr::new(*a)));
        self.product.origami_ok(all)?;
        Ok(())
    }

    pub fn start(&mut self) -> Result<()> {
        self.product.origami_ok([
            "run".as_ref(),
            self.dir.as_os_str(),
            "--display".as_ref(),
            "none".as_ref(),
            "--background".as_ref(),
        ])?;
        let log = self
            .product
            .evidence()
            .join(format!("{}-console.log", self.name));
        let mut con = Console::connect(&self.dir, Some(&log))?;
        console::login(&mut con, secs(1500))?;
        self.con = Some(con);
        self.sh("true", secs(120))?;
        Ok(())
    }

    pub fn console(&mut self) -> &mut Console {
        self.con.as_mut().expect("guest has not been started")
    }

    /// Run a shell command; returns its exit status and output.
    pub fn sh(&mut self, command: &str, timeout: Duration) -> Result<(i32, String)> {
        let (status, text) = self.console().run(command, timeout)?;
        // The first line is the echoed command; the shell's next prompt
        // arrives ahead of the output.
        let body = text.split('\n').skip(1).collect::<Vec<_>>().join("\n");
        let mut out = body.trim();
        while let Some(rest) = out.strip_prefix("# ") {
            out = rest;
        }
        Ok((status, out.to_string()))
    }

    /// Run a command that must exit 0; returns its output.
    pub fn must(&mut self, command: &str, timeout: Duration) -> Result<String> {
        let (status, out) = self.sh(command, timeout)?;
        println!(
            "[{}] {command} -> {status}: {}",
            self.name,
            super::head(&out, 400)
        );
        if status != 0 {
            return Err(format!("{}: {command:?} exited {status}: {out}", self.name).into());
        }
        Ok(out)
    }

    /// The IPv4 address ef0 gets on `prefix`, waiting for DHCP. The installed
    /// system starts on the address it held during the install, so the
    /// first address seen is not the answer.
    pub fn address(&mut self, prefix: &str, timeout: Duration) -> Result<String> {
        let end = Instant::now() + timeout;
        let rx = Regex::new(r"inet (\d+\.\d+\.\d+\.\d+)")?;
        let mut last = String::new();
        while Instant::now() < end {
            let (_, out) = self.sh("ifconfig ef0", secs(120))?;
            if let Some(m) = rx.captures(&out).filter(|m| m[1].starts_with(prefix)) {
                return Ok(m[1].to_string());
            }
            last = out;
            pause(secs(10));
        }
        Err(format!("{}: ef0 got no {prefix}* address; last: {last}", self.name).into())
    }

    /// Shut IRIX down cleanly, then make sure QEMU is gone. Safe to call
    /// twice and on a guest that never started.
    pub fn stop(&mut self) {
        if let Some(con) = &mut self.con {
            let _ = con.line("halt -p");
            let end = Instant::now() + secs(240);
            while Instant::now() < end && !self.product.stopped(&self.dir) {
                pause(secs(5));
            }
            con.close();
            self.con = None;
        }
        stop_machine(&self.product, &self.dir, None, &self.name);
    }
}

impl Drop for Guest {
    fn drop(&mut self) {
        if std::thread::panicking() {
            // Evidence first: stopping the machine destroys what it shows.
            capture::save(&self.product.evidence(), &self.name, &self.dir, None);
        }
        self.stop();
    }
}

/// Stop a machine that is still running, whatever state its console is in.
pub fn stop_machine(product: &Product, dir: &Path, _hint: Option<u32>, name: &str) {
    if dir.join("state/run.json").exists() && !product.stopped(dir) {
        eprintln!("stopping {name}");
        let _ = product.origami_for(["stop".as_ref(), dir.as_os_str()], secs(600));
        pause(secs(3));
    }
}
