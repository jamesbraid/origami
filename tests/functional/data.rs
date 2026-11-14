//! `tests/data/systems.toml`: which machines to boot, on what disk, and what
//! their guests must report.

use std::collections::BTreeMap;

use serde::Deserialize;

#[derive(Deserialize)]
pub struct Data {
    pub family: BTreeMap<String, Family>,
    pub system: Vec<System>,
}

/// What machines on one software platform share: the installed disk and how
/// the guest names its hardware.
#[derive(Deserialize)]
pub struct Family {
    /// What `uname -m` prints.
    pub uname: String,
    /// The Ethernet interface `hinv` and `ifconfig` name.
    pub nic: String,
    /// How the guest gets an address on the product's 10.98.0.0/24 network:
    /// by DHCP, or, for a disk installed on another network that keeps its
    /// address in /etc/hosts, set by the test after login.
    #[serde(default)]
    pub address: Address,
    /// SCSI target the test CD-ROM attaches at; the guest sees dks0d<N>vol.
    pub cd_target: u32,
    /// SCSI target of the system disk.
    pub disk_target: u32,
    /// Absent until a disk is published; every system of the family is then skipped.
    pub disk: Option<Disk>,
    /// Why the family cannot run, shown in the test listing.
    pub skip: Option<String>,
}

#[derive(Deserialize, Default, PartialEq, Eq, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub enum Address {
    #[default]
    Dhcp,
    Static,
}

#[derive(Deserialize)]
pub struct Disk {
    pub url: String,
    pub sha256: String,
}

#[derive(Deserialize)]
pub struct System {
    pub id: String,
    pub family: String,
    pub preset: String,
    /// The emulators that boot it: `qemu` today.
    pub engines: Vec<String>,
    /// The host systems (linux, macos, windows) that boot it.
    pub hosts: Vec<String>,
    /// A substring of the `hinv` processor line.
    pub cpu: String,
    pub cpus: u32,
    /// What `hinv` reports as main memory, in MiB.
    pub memory_mb: u32,
    /// `create --memory-per-node`, when the preset's own default is not wanted.
    pub memory_per_node: Option<u32>,
    /// Seconds to wait for a login prompt.
    #[serde(default = "default_login_timeout")]
    pub login_timeout: u64,
    /// Why this system cannot run on any host, shown in the test listing.
    pub skip: Option<String>,
}

fn default_login_timeout() -> u64 {
    1200
}

pub fn load() -> Data {
    toml::from_str(include_str!("../data/systems.toml")).expect("tests/data/systems.toml")
}

/// The host operating system in the names systems.toml uses.
pub fn host() -> &'static str {
    if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(windows) {
        "windows"
    } else {
        "linux"
    }
}
