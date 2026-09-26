use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

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
    #[serde(default)]
    pub network: Network,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub drive: Vec<Drive>,
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
            if endpoint.is_empty() || endpoint.contains(',') {
                return Err("private network endpoint must be a path without a comma".into());
            }
            let mac = file
                .network
                .mac
                .as_deref()
                .ok_or("private network needs mac")?;
            let parts: Vec<_> = mac.split(':').collect();
            if parts.len() != 6
                || parts
                    .iter()
                    .any(|part| part.len() != 2 || u8::from_str_radix(part, 16).is_err())
            {
                return Err("network MAC must contain six hexadecimal bytes".into());
            }
        }
        _ => return Err("network mode must be user, none, or private".into()),
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

pub fn create(dir: &Path, offering: &Offering, prom: &Path) -> Result<()> {
    if dir.exists() {
        return Err(format!("destination already exists: {}", dir.display()).into());
    }
    if fs::metadata(prom)?.len() != offering.firmware.size {
        return Err(format!("PROM must be {} bytes", offering.firmware.size).into());
    }
    fs::create_dir(dir)?;
    let result = (|| -> Result<()> {
        for name in ["firmware", "drives", "state", "logs"] {
            fs::create_dir(dir.join(name))?;
        }
        fs::copy(prom, dir.join("firmware/prom.bin"))?;
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
            network: Network::default(),
            drive: vec![],
        };
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
    fn selected_presets_match_compiled_machine_catalogue() {
        let catalog = catalogue().unwrap();
        assert_eq!(presets(&catalog).len(), 5);
        for (_, offering) in presets(&catalog) {
            assert_eq!(offering.cpus_per_node.iter().sum::<u32>(), offering.smp);
        }
    }
}
