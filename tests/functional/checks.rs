//! What a logged-in guest is asked, and what each answer must be.

use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use regex::Regex;
use serde_json::{json, Value};

use crate::console::Console;
use crate::data::{Address, Family, System};

/// The product's user network: its gateway, the NFS server `origami share`
/// runs, and the address a guest that is not given one by DHCP takes.
pub const GATEWAY: &str = "10.98.0.2";
pub const NFS_SERVER: &str = "10.98.0.4";
pub const STATIC_ADDRESS: &str = "10.98.0.15";
pub const SHARE: &str = "smoke";
pub const CD_BYTES: usize = 1 << 20;
/// What the guest reads back from the start of the CD-ROM image. IRIX reads a
/// quarter of the 1 MiB image through the volume device.
pub const CD_READ_BYTES: usize = 256 * 1024;

/// POSIX cksum(1): CRC-32 with the length folded in, as `cksum` prints it.
pub fn posix_cksum(data: &[u8]) -> String {
    let mut table = [0u32; 256];
    for (i, slot) in table.iter_mut().enumerate() {
        let mut c = (i as u32) << 24;
        for _ in 0..8 {
            c = if c & 0x8000_0000 != 0 {
                (c << 1) ^ 0x04C1_1DB7
            } else {
                c << 1
            };
        }
        *slot = c;
    }
    let mut crc = 0u32;
    for &b in data {
        crc = (crc << 8) ^ table[(((crc >> 24) as u8) ^ b) as usize];
    }
    let mut n = data.len();
    while n != 0 {
        crc = (crc << 8) ^ table[(((crc >> 24) as u8) ^ (n as u8)) as usize];
        n >>= 8;
    }
    format!("{} {}", !crc, data.len())
}

/// Bytes that are not a pattern a driver could fake, made without a random
/// number crate: a xorshift stream from the clock.
pub fn noise(len: usize) -> Vec<u8> {
    let mut x = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0x9e37_79b9_7f4a_7c15, |d| d.as_nanos() as u64)
        | 1;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 24) as u8
        })
        .collect()
}

pub struct Results {
    started: Instant,
    pub checks: Vec<(String, bool, String, f64)>,
}

impl Results {
    pub fn new() -> Self {
        Self {
            started: Instant::now(),
            checks: Vec::new(),
        }
    }

    pub fn record(&mut self, name: &str, ok: bool, detail: &str, seconds: f64) {
        let tail: String = detail
            .chars()
            .rev()
            .take(400)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        eprintln!("[check] {name}: {} {tail}", if ok { "ok" } else { "FAIL" });
        self.checks.push((name.into(), ok, tail, seconds));
    }

    pub fn passed(&self) -> bool {
        !self.checks.is_empty() && self.checks.iter().all(|c| c.1)
    }

    pub fn failures(&self) -> Vec<String> {
        self.checks
            .iter()
            .filter(|c| !c.1)
            .map(|c| format!("{}: {}", c.0, c.2))
            .collect()
    }

    pub fn json(&self, system: &System, error: Option<&str>) -> Value {
        json!({
            "system": system.id,
            "preset": system.preset,
            "status": if error.is_none() && self.passed() { "pass" } else { "fail" },
            "error": error,
            "duration_s": self.started.elapsed().as_secs(),
            "checks": self.checks.iter().map(|(name, ok, detail, seconds)| json!({
                "name": name, "ok": ok, "detail": detail, "seconds": (seconds * 10.0).round() / 10.0,
            })).collect::<Vec<_>>(),
        })
    }
}

pub struct Fixtures<'a> {
    pub share_dir: &'a Path,
    pub share_data: &'a [u8],
    pub cd_data: &'a [u8],
}

fn sh(con: &mut Console, command: &str, secs: u64) -> Result<(i32, String), String> {
    con.run(command, Duration::from_secs(secs))
}

/// Run every check on a logged-in guest; a failure is recorded and the next
/// check still runs.
pub fn run_all(
    con: &mut Console,
    family: &Family,
    system: &System,
    fixtures: &Fixtures,
    results: &mut Results,
    deadline: Instant,
) -> Result<(), String> {
    let timed = |name: &str,
                 results: &mut Results,
                 check: &mut dyn FnMut() -> Result<(bool, String), String>| {
        let started = Instant::now();
        let (ok, detail) = check().unwrap_or_else(|e| (false, e));
        results.record(name, ok, &detail, started.elapsed().as_secs_f64());
        if Instant::now() > deadline {
            return Err("overall deadline reached".to_string());
        }
        Ok(ok)
    };

    timed("uname", results, &mut || {
        let (rc, out) = sh(con, "uname -m", 60)?;
        Ok((
            rc == 0 && out.trim() == family.uname,
            format!("{out:?}, want {}", family.uname),
        ))
    })?;

    timed("hinv", results, &mut || {
        let (rc, out) = sh(con, "hinv", 120)?;
        let mut missing = Vec::new();
        if !out.contains(&system.cpu) {
            missing.push(format!("cpu {}", system.cpu));
        }
        let cpus = Regex::new(r"(?m)^(\d+) \d+ MHZ \w+ Processors?").unwrap();
        match cpus.captures(&out).and_then(|c| c[1].parse::<u32>().ok()) {
            Some(n) if n == system.cpus => {}
            other => missing.push(format!("{} processors, saw {other:?}", system.cpus)),
        }
        let memory = Regex::new(r"Main memory size: (\d+) Mbyte").unwrap();
        match memory.captures(&out).and_then(|c| c[1].parse::<u32>().ok()) {
            Some(n) if n == system.memory_mb => {}
            other => missing.push(format!(
                "{} Mbyte of memory, saw {other:?}",
                system.memory_mb
            )),
        }
        if !out.contains("CDROM") {
            missing.push("CDROM".into());
        }
        if !out.contains(&family.nic) {
            missing.push(format!("ethernet {}", family.nic));
        }
        Ok((
            rc == 0 && missing.is_empty(),
            if missing.is_empty() {
                out
            } else {
                format!("missing {missing:?}: {out}")
            },
        ))
    })?;

    let addressed = timed("network_address", results, &mut || {
        if family.address == Address::Static {
            // The disk was installed on another network and keeps that address
            // in /etc/hosts and a default route, so DHCP is not in play.
            let (rc, out) = sh(
                con,
                &format!(
                    "ifconfig {nic} inet {STATIC_ADDRESS} netmask 0xffffff00 up; route delete default; route add default {GATEWAY}",
                    nic = family.nic
                ),
                60,
            )?;
            if rc != 0 {
                return Ok((false, out));
            }
        }
        let end = Instant::now() + Duration::from_secs(300);
        let rx = Regex::new(r"inet (10\.98\.0\.\d+)").unwrap();
        let mut out = String::new();
        while Instant::now() < end {
            out = sh(con, &format!("ifconfig {}", family.nic), 60)?.1;
            if let Some(m) = rx.captures(&out) {
                return Ok((true, m[1].to_string()));
            }
            std::thread::sleep(Duration::from_secs(10));
        }
        // What decides the address, for whoever reads the failure.
        let more = sh(con, "netstat -rn; cat /etc/config/ifconfig-1.options /etc/hosts; chkconfig | grep -i -E 'auto|dhcp|network'; ps -ef | grep -i -E 'dhcp|bootp'", 60)?.1;
        Ok((false, format!("{out}\n{more}")))
    })?;
    let mut ready = false;
    if addressed {
        ready = timed("ping_gateway", results, &mut || {
            let (rc, out) = sh(con, &format!("ping -c 3 {}", GATEWAY), 60)?;
            Ok((
                rc == 0
                    && Regex::new(r"(^|[^0-9])0% packet loss")
                        .unwrap()
                        .is_match(&out),
                out,
            ))
        })?;
    }

    // Without a working network every NFS command waits out its retries, so
    // the checks that need one fail at once.
    if ready {
        let mounted = timed("nfs_mount", results, &mut || {
            let (rc, out) = sh(
                con,
                &format!("mkdir -p /mnt/s; mount {}:/{SHARE} /mnt/s", NFS_SERVER),
                240,
            )?;
            Ok((rc == 0, out))
        })?;
        if mounted {
            timed("nfs_read_checksum", results, &mut || {
                let (rc, out) = sh(con, "cksum /mnt/s/data.bin", 300)?;
                let got = out.split_whitespace().take(2).collect::<Vec<_>>().join(" ");
                Ok((
                    rc == 0 && got == posix_cksum(fixtures.share_data),
                    format!("guest {got}"),
                ))
            })?;
            timed("nfs_write_checksum", results, &mut || {
                let (rc, _) = sh(
                    con,
                    "dd if=/mnt/s/data.bin of=/mnt/s/copy.bin bs=8k; sync",
                    300,
                )?;
                // A file the guest makes itself, so the checksum is not of data it only copied.
                let (rc2, _) = sh(
                    con,
                    "dd if=/unix bs=32k count=64 of=/mnt/s/guest.bin; sync",
                    300,
                )?;
                let (_, local) = sh(con, "dd if=/unix bs=32k count=64 2>/dev/null | cksum", 120)?;
                let copy_ok = fs::read(fixtures.share_dir.join("copy.bin"))
                    .map_err(|e| e.to_string())?
                    == fixtures.share_data;
                let written =
                    fs::read(fixtures.share_dir.join("guest.bin")).map_err(|e| e.to_string())?;
                let guest_ok = local
                    .split_whitespace()
                    .take(2)
                    .collect::<Vec<_>>()
                    .join(" ")
                    == posix_cksum(&written);
                sh(con, "umount /mnt/s", 60)?;
                Ok((
                    rc == 0 && rc2 == 0 && copy_ok && guest_ok,
                    format!("copy {copy_ok}, guest file {guest_ok}"),
                ))
            })?;
        }
    } else {
        for name in ["nfs_mount", "nfs_read_checksum", "nfs_write_checksum"] {
            results.record(
                name,
                false,
                "not run: the guest has no working network",
                0.0,
            );
        }
    }

    timed("cdrom_read_checksum", results, &mut || {
        // The device needs 2048-byte reads.
        let (rc, out) = sh(
            con,
            &format!(
                "dd if=/dev/rdsk/dks0d{}vol bs=2048 count={} 2>/dev/null | cksum",
                family.cd_target,
                CD_READ_BYTES / 2048
            ),
            300,
        )?;
        let got = out.split_whitespace().take(2).collect::<Vec<_>>().join(" ");
        Ok((
            rc == 0 && got == posix_cksum(&fixtures.cd_data[..CD_READ_BYTES]),
            format!("guest {got}"),
        ))
    })?;
    Ok(())
}
