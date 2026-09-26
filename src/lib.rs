use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::net::SocketAddrV4;
use std::path::{Path, PathBuf};

pub mod control;
pub mod install;
pub mod origin300;
pub mod runtime;

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[derive(Clone, Debug, Deserialize)]
pub struct Catalog {
    pub offerings: Vec<Offering>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Offering {
    pub product: String,
    pub topology: String,
    pub nodes: u32,
    pub smp: u32,
    #[serde(rename = "cpus-per-node")]
    pub cpus_per_node: Vec<u32>,
    #[serde(rename = "default-cpu-model")]
    pub cpu: String,
    #[serde(rename = "memory-per-node-mib")]
    pub memory: Memory,
    pub firmware: FirmwareRequirement,
    pub storage: Vec<Storage>,
    #[serde(rename = "needs-debug-leds-off", default)]
    pub needs_debug_leds_off: bool,
    pub resources: Vec<Resource>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Memory {
    pub accepted: Vec<u32>,
    pub default: u32,
}

#[derive(Clone, Debug, Deserialize)]
pub struct FirmwareRequirement {
    pub size: u64,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub sha256: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Storage {
    pub bus: String,
    pub targets: Vec<u32>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Resource {
    pub kind: String,
    pub count: u32,
    #[serde(default)]
    pub size: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MachineFile {
    pub format: u32,
    pub machine: Machine,
    pub firmware: Firmware,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<Origin300Identity>,
    #[serde(default)]
    pub network: Network,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub drive: Vec<Drive>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Origin300Identity {
    pub mac: String,
    pub spd_dimm2: String,
    pub spd_dimm3: String,
}

pub struct Origin300Create<'a> {
    pub spd_dimm2: &'a Path,
    pub spd_dimm3: &'a Path,
    pub mac: &'a str,
}

pub fn sha256_file(path: &Path) -> Result<String> {
    Ok(format!("{:x}", Sha256::digest(fs::read(path)?)))
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Network {
    #[serde(default = "default_network_mode")]
    pub mode: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mac: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub forward: Vec<PortForward>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PortForward {
    pub name: String,
    pub protocol: String,
    pub host_port: u16,
    pub guest_port: u16,
}

fn default_network_mode() -> String {
    "user".into()
}

impl Default for Network {
    fn default() -> Self {
        Self {
            mode: default_network_mode(),
            endpoint: None,
            mac: None,
            forward: vec![],
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Machine {
    pub model: String,
    pub nodes: u32,
    pub cpus_per_node: u32,
    pub memory_per_node: String,
    #[serde(default = "default_graphics")]
    pub graphics: String,
}

fn default_graphics() -> String {
    "none".into()
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Firmware {
    pub image: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Drive {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub bus: u32,
    pub target: u32,
    pub image: String,
    #[serde(default)]
    pub read_only: bool,
}

pub fn catalogue_sha256() -> String {
    format!(
        "{:x}",
        Sha256::digest(include_bytes!("../catalogue/sn-catalogue.json"))
    )
}

pub fn catalogue() -> Result<Catalog> {
    Ok(serde_json::from_str(include_str!(
        "../catalogue/sn-catalogue.json"
    ))?)
}

pub fn presets(catalog: &Catalog) -> Vec<(&'static str, &Offering)> {
    const PRESETS: &[(&str, &str, u32, u32)] = &[
        ("origin200-1", "origin200", 1, 1),
        ("origin200-2", "origin200", 1, 2),
        ("origin200-dual", "origin200", 2, 4),
        ("origin2000-8", "origin2000", 4, 8),
        ("origin300-2", "origin300", 1, 2),
    ];
    PRESETS
        .iter()
        .filter_map(|(name, product, nodes, smp)| {
            catalog
                .offerings
                .iter()
                .find(|o| o.product == *product && o.nodes == *nodes && o.smp == *smp)
                .map(|offering| (*name, offering))
        })
        .collect()
}

pub fn preset<'a>(catalog: &'a Catalog, name: &str) -> Result<&'a Offering> {
    presets(catalog)
        .into_iter()
        .find(|(n, _)| *n == name)
        .map(|(_, o)| o)
        .ok_or_else(|| format!("unknown preset: {name}").into())
}

pub fn read_machine(dir: &Path) -> Result<MachineFile> {
    Ok(toml::from_str(&fs::read_to_string(
        dir.join("machine.toml"),
    )?)?)
}

pub fn tcp_endpoint(endpoint: &str) -> Result<Option<SocketAddrV4>> {
    let Some(address) = endpoint.strip_prefix("tcp:") else {
        return Ok(None);
    };
    let address: SocketAddrV4 = address
        .parse()
        .map_err(|_| "private TCP endpoint must be tcp:127.0.0.1:PORT")?;
    if !address.ip().is_loopback() || address.port() == 0 {
        return Err("private TCP endpoint needs a loopback IPv4 address and nonzero port".into());
    }
    Ok(Some(address))
}

pub fn valid_mac(mac: &str) -> bool {
    let parts: Vec<_> = mac.split(':').collect();
    parts.len() == 6
        && parts
            .iter()
            .all(|part| part.len() == 2 && u8::from_str_radix(part, 16).is_ok())
}

pub fn validate<'a>(catalog: &'a Catalog, dir: &Path, file: &MachineFile) -> Result<&'a Offering> {
    if file.format != 1 {
        return Err(format!("unsupported machine format {}", file.format).into());
    }
    let memory = file
        .machine
        .memory_per_node
        .strip_suffix("MiB")
        .ok_or("memory_per_node must use MiB")?
        .parse::<u32>()?;
    let offering = catalog
        .offerings
        .iter()
        .find(|o| {
            o.product == file.machine.model
                && o.nodes == file.machine.nodes
                && o.smp == file.machine.nodes * file.machine.cpus_per_node
                && o.cpus_per_node
                    .iter()
                    .all(|cpus| *cpus == file.machine.cpus_per_node)
        })
        .ok_or("unsupported machine and processor population")?;
    if !offering.memory.accepted.contains(&memory) {
        return Err(format!(
            "{} MiB per node is not offered for {}",
            memory, offering.topology
        )
        .into());
    }
    if file.machine.graphics != "none"
        && !(file.machine.graphics == "rad4" && offering.product == "origin200")
    {
        return Err(format!("unsupported graphics selection {}", file.machine.graphics).into());
    }
    match file.network.mode.as_str() {
        "none" | "user" if file.network.endpoint.is_none() && file.network.mac.is_none() => (),
        "private" => {
            let endpoint = file
                .network
                .endpoint
                .as_deref()
                .ok_or("private network needs endpoint")?;
            if endpoint.is_empty() {
                return Err("private network endpoint cannot be empty".into());
            }
            let tcp = tcp_endpoint(endpoint)?;
            if cfg!(windows) && tcp.is_none() {
                return Err("Windows private network requires a loopback TCP endpoint".into());
            }
            let mac = file
                .network
                .mac
                .as_deref()
                .ok_or("private network needs mac")?;
            if !valid_mac(mac) {
                return Err("network MAC must contain six hexadecimal bytes".into());
            }
        }
        _ => return Err("network mode must be user, none, or private".into()),
    }
    let mut names = std::collections::HashSet::new();
    let mut host_ports = std::collections::HashSet::new();
    for forward in &file.network.forward {
        if forward.name.is_empty()
            || !forward
                .name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
        {
            return Err("forward name must use letters, digits, hyphens, or underscores".into());
        }
        if !matches!(forward.protocol.as_str(), "tcp" | "udp") {
            return Err(format!("forward {} must use tcp or udp", forward.name).into());
        }
        if forward.host_port == 0 || forward.guest_port == 0 {
            return Err(format!("forward {} needs nonzero ports", forward.name).into());
        }
        if !names.insert(&forward.name) {
            return Err(format!("duplicate forward name: {}", forward.name).into());
        }
        if !host_ports.insert((&forward.protocol, forward.host_port)) {
            return Err(format!(
                "duplicate {} host port {}",
                forward.protocol, forward.host_port
            )
            .into());
        }
    }
    let prom = resolve(dir, &file.firmware.image);
    if fs::metadata(&prom)?.len() != offering.firmware.size {
        return Err(format!(
            "firmware {} must be {} bytes",
            prom.display(),
            offering.firmware.size
        )
        .into());
    }
    if let Some(expected) = &offering.firmware.sha256 {
        if sha256_file(&prom)? != *expected {
            return Err(format!("firmware {} has an unexpected SHA-256", prom.display()).into());
        }
    }
    if offering.product == "origin300" {
        let identity = file
            .identity
            .as_ref()
            .ok_or("Origin 300 needs an identity section")?;
        if !valid_mac(&identity.mac) {
            return Err("Origin 300 identity MAC must contain six hexadecimal bytes".into());
        }
        if file.network.mode == "private" && file.network.mac.as_deref() != Some(&identity.mac) {
            return Err("private network MAC must match Origin 300 board identity".into());
        }
        origin300::validate_spd(dir, identity)?;
    } else if file.identity.is_some() {
        return Err("identity inputs are only supported for Origin 300".into());
    }
    let mut occupied = std::collections::HashSet::new();
    for drive in &file.drive {
        let bus = format!("scsi.{}", drive.bus);
        if !offering
            .storage
            .iter()
            .any(|s| s.bus == bus && s.targets.contains(&drive.target))
        {
            return Err(format!("unsupported SCSI target {} on {}", drive.target, bus).into());
        }
        if !occupied.insert((drive.bus, drive.target)) {
            return Err(format!("duplicate SCSI target {} on {}", drive.target, bus).into());
        }
        if !matches!(drive.kind.as_str(), "disk" | "cdrom" | "tape") {
            return Err(format!("unsupported drive type {}", drive.kind).into());
        }
        if drive.kind == "cdrom" && !drive.read_only {
            return Err("CD-ROM must be read-only".into());
        }
        if !resolve(dir, &drive.image).is_file() {
            return Err(format!("missing drive image {}", drive.image).into());
        }
    }
    Ok(offering)
}

pub fn resolve(dir: &Path, value: &str) -> PathBuf {
    let path = Path::new(value);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        dir.join(path)
    }
}

pub fn qemu_path_option(path: &Path) -> String {
    path.display().to_string().replace(',', ",,")
}

pub fn create(
    dir: &Path,
    offering: &Offering,
    prom: &Path,
    identity: Option<Origin300Create<'_>>,
) -> Result<()> {
    if dir.exists() {
        return Err(format!("destination already exists: {}", dir.display()).into());
    }
    if fs::metadata(prom)?.len() != offering.firmware.size {
        return Err(format!("PROM must be {} bytes", offering.firmware.size).into());
    }
    if let Some(expected) = &offering.firmware.sha256 {
        if sha256_file(prom)? != *expected {
            return Err("PROM has an unexpected SHA-256".into());
        }
    }
    if offering.product == "origin300" {
        let inputs = identity
            .as_ref()
            .ok_or("Origin 300 needs --spd-dimm2 and --spd-dimm3")?;
        origin300::validate_spd_file(inputs.spd_dimm2, origin300::DIMM2_SHA256)?;
        origin300::validate_spd_file(inputs.spd_dimm3, origin300::DIMM3_SHA256)?;
        if !valid_mac(inputs.mac) {
            return Err("Origin 300 MAC must contain six hexadecimal bytes".into());
        }
    } else if identity.is_some() {
        return Err("SPD inputs are only supported for Origin 300".into());
    }
    fs::create_dir(dir)?;
    let result = (|| -> Result<()> {
        for name in ["firmware", "drives", "state", "logs"] {
            fs::create_dir(dir.join(name))?;
        }
        fs::copy(prom, dir.join("firmware/prom.bin"))?;
        let identity = if let Some(inputs) = identity {
            fs::copy(inputs.spd_dimm2, dir.join("firmware/spd-dimm2.bin"))?;
            fs::copy(inputs.spd_dimm3, dir.join("firmware/spd-dimm3.bin"))?;
            Some(Origin300Identity {
                mac: inputs.mac.into(),
                spd_dimm2: "firmware/spd-dimm2.bin".into(),
                spd_dimm3: "firmware/spd-dimm3.bin".into(),
            })
        } else {
            None
        };
        let file = MachineFile {
            format: 1,
            machine: Machine {
                model: offering.product.clone(),
                nodes: offering.nodes,
                cpus_per_node: offering.cpus_per_node[0],
                memory_per_node: format!("{}MiB", offering.memory.default),
                graphics: if offering.product == "origin200"
                    && offering.nodes == 1
                    && offering.smp == 1
                {
                    "rad4"
                } else {
                    "none"
                }
                .into(),
            },
            firmware: Firmware {
                image: "firmware/prom.bin".into(),
            },
            identity,
            network: Network::default(),
            drive: vec![],
        };
        if let Some(identity) = &file.identity {
            origin300::validate_spd(dir, identity)?;
        }
        fs::write(dir.join("machine.toml"), toml::to_string_pretty(&file)?)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(dir);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_tcp_endpoint_stays_on_loopback() {
        assert!(tcp_endpoint("tcp:127.0.0.1:49173").unwrap().is_some());
        for endpoint in [
            "tcp:0.0.0.0:49173",
            "tcp:192.0.2.1:49173",
            "tcp:127.0.0.1:0",
        ] {
            assert!(tcp_endpoint(endpoint).is_err(), "{endpoint}");
        }
    }

    #[test]
    fn selected_presets_match_compiled_machine_catalogue() {
        let catalog = catalogue().unwrap();
        assert_eq!(presets(&catalog).len(), 5);
        for (_, offering) in presets(&catalog) {
            assert_eq!(offering.cpus_per_node.iter().sum::<u32>(), offering.smp);
        }
    }
}
