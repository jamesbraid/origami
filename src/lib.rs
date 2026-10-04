use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::net::SocketAddrV4;
use std::path::{Path, PathBuf};

pub mod assets;
pub mod control;
pub mod firmware;
pub mod install;
pub mod origin300;
pub mod profiles;
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
    #[serde(rename = "backend-ids", default)]
    pub backend_ids: Vec<String>,
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
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub spd_dimm2: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub spd_dimm3: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chassis_eeprom: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub board_eeprom: Option<String>,
}

pub struct Origin300Create<'a> {
    pub spd_dimm2: Option<&'a Path>,
    pub spd_dimm3: Option<&'a Path>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub topology: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub population: Vec<u32>,
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub inputs: std::collections::BTreeMap<String, String>,
    pub model: String,
    pub nodes: u32,
    pub cpus_per_node: u32,
    pub memory_per_node: String,
    #[serde(default = "default_graphics")]
    pub graphics: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub console: Option<String>,
}

fn default_graphics() -> String {
    "none".into()
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Firmware {
    pub image: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub io_image: Option<String>,
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

pub fn presets(catalog: &Catalog) -> Vec<(String, &Offering)> {
    profiles::presets(catalog)
}

pub fn preset<'a>(catalog: &'a Catalog, name: &str) -> Result<&'a Offering> {
    presets(catalog)
        .into_iter()
        .find(|(n, _)| n == name)
        .map(|(_, offering)| offering)
        .ok_or_else(|| format!("unknown or unimplemented preset: {name}").into())
}

pub fn read_machine(dir: &Path) -> Result<MachineFile> {
    let path = dir.join("machine.toml");
    let text = fs::read_to_string(&path).map_err(|error| {
        format!(
            "cannot read {}: {error}. Use a machine directory created with origami create",
            path.display()
        )
    })?;
    toml::from_str(&text).map_err(|error| {
        format!("invalid machine configuration {}: {error}", path.display()).into()
    })
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
                && file.machine.topology.as_ref().map_or(
                    o.topology
                        == profiles::legacy_topology(&file.machine.model, file.machine.nodes),
                    |topology| o.topology == *topology,
                )
                && o.cpus_per_node == profiles::population(&file.machine)
        })
        .ok_or("unsupported machine and processor population")?;
    if !offering.memory.accepted.contains(&memory) {
        return Err(format!(
            "{} MiB per node is not offered for {}",
            memory, offering.topology
        )
        .into());
    }
    profiles::validate_graphics(offering, &file.machine.graphics)?;
    profiles::validate_console(offering, file.machine.console.as_deref())?;
    profiles::validate_inputs(offering, &file.machine.inputs)?;
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
    runtime::validate_state(dir, file, offering)?;
    if offering.product == "origin300" && file.identity.is_some() {
        if offering.nodes != 1 || offering.smp != 2 {
            return Err("explicit Origin 300 identity currently requires one two-CPU node".into());
        }
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
        let path = resolve(dir, &drive.image);
        if !path.is_file() {
            return Err(format!("missing drive image {}; restore the disk and its backing files from a complete backup, or attach an existing image with drive-attach", path.display()).into());
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

pub fn validate_io_prom(offering: &Offering, path: &Path) -> Result<()> {
    if !profiles::has_io_prom(offering) {
        return Err(format!("{} has no separate IO PROM flash", offering.topology).into());
    }
    firmware::Layout::find(&offering.topology, "io")?.read_original(path)?;
    Ok(())
}

fn read_boot_prom(offering: &Offering, path: &Path) -> Result<Vec<u8>> {
    firmware::Layout::find(&offering.topology, "cpu")?.read_original(path)
}

pub fn validate_create_inputs(
    dir: &Path,
    offering: &Offering,
    memory_per_node: Option<u32>,
    identity: Option<&Origin300Create<'_>>,
) -> Result<()> {
    let memory_per_node = memory_per_node.unwrap_or(offering.memory.default);
    if !offering.memory.accepted.contains(&memory_per_node) {
        return Err(format!(
            "{} MiB per node is not offered for {}",
            memory_per_node, offering.topology
        )
        .into());
    }
    match fs::symlink_metadata(dir) {
        Ok(_) => return Err(format!("destination already exists: {}", dir.display()).into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let mut parent = dir.parent();
    while let Some(path) = parent.filter(|path| !path.as_os_str().is_empty()) {
        match fs::symlink_metadata(path) {
            Ok(_) => {
                if !fs::metadata(path)?.is_dir() {
                    return Err(format!(
                        "destination parent is not a directory: {}",
                        path.display()
                    )
                    .into());
                }
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => parent = path.parent(),
            Err(error) => return Err(error.into()),
        }
    }
    if offering.product == "origin300" && identity.is_some() {
        if offering.nodes != 1 || offering.smp != 2 {
            return Err(
                "explicit Origin 300 identity currently requires one two-CPU chassis".into(),
            );
        }
        let inputs = identity.ok_or("Origin 300 needs an identity selection")?;
        match (inputs.spd_dimm2, inputs.spd_dimm3) {
            (Some(dimm2), Some(dimm3)) => {
                origin300::validate_spd_file(dimm2)?;
                origin300::validate_spd_file(dimm3)?;
            }
            (None, None) => (),
            _ => {
                return Err(
                    "supply --spd-dimm2 and --spd-dimm3 together, or omit both for QEMU defaults"
                        .into(),
                )
            }
        }
        if !valid_mac(inputs.mac) {
            return Err("Origin 300 MAC must contain six hexadecimal bytes".into());
        }
    } else if offering.product != "origin300" && identity.is_some() {
        return Err("SPD inputs are only supported for Origin 300".into());
    }
    Ok(())
}

pub fn create(
    dir: &Path,
    offering: &Offering,
    prom: &Path,
    memory_per_node: Option<u32>,
    identity: Option<Origin300Create<'_>>,
) -> Result<()> {
    create_configured(
        dir,
        offering,
        prom,
        None,
        memory_per_node,
        identity,
        None,
        Default::default(),
    )
}

pub fn create_configured(
    dir: &Path,
    offering: &Offering,
    prom: &Path,
    io_prom: Option<&Path>,
    memory_per_node: Option<u32>,
    identity: Option<Origin300Create<'_>>,
    graphics: Option<&str>,
    inputs: std::collections::BTreeMap<String, String>,
) -> Result<()> {
    validate_create_inputs(dir, offering, memory_per_node, identity.as_ref())?;
    let graphics = graphics.unwrap_or(profiles::default_graphics(offering));
    profiles::validate_graphics(offering, graphics)?;
    profiles::validate_inputs(offering, &inputs)?;
    let cpu_layout = firmware::Layout::find(&offering.topology, "cpu")?;
    let original = read_boot_prom(offering, prom)?;
    let cpu_image = cpu_layout.prepare(&original)?;
    let cpu_images = vec![cpu_image; offering.nodes as usize];
    let io_original = if let Some(path) = io_prom {
        if !profiles::has_io_prom(offering) {
            return Err(format!("{} has no separate IO PROM flash", offering.topology).into());
        }
        Some(firmware::Layout::find(&offering.topology, "io")?.read_original(path)?)
    } else {
        None
    };
    let io_images = if let Some(original) = &io_original {
        vec![
            firmware::Layout::find(&offering.topology, "io")?.prepare(original)?;
            firmware::io_count(offering)
        ]
    } else {
        vec![]
    };
    create_with_images(
        dir,
        offering,
        memory_per_node,
        identity,
        graphics,
        inputs,
        &cpu_images,
        &io_images,
        Some(&original),
        io_original.as_deref(),
    )
}

pub fn import_configured(
    dir: &Path,
    offering: &Offering,
    cpu_flash: &[PathBuf],
    io_flash: &[PathBuf],
    memory_per_node: Option<u32>,
    identity: Option<Origin300Create<'_>>,
    graphics: Option<&str>,
    inputs: std::collections::BTreeMap<String, String>,
) -> Result<()> {
    validate_create_inputs(dir, offering, memory_per_node, identity.as_ref())?;
    let graphics = graphics.unwrap_or(profiles::default_graphics(offering));
    profiles::validate_graphics(offering, graphics)?;
    profiles::validate_inputs(offering, &inputs)?;
    if cpu_flash.len() != offering.nodes as usize {
        return Err(format!(
            "{} needs {} independent --cpu-flash inputs in node order",
            offering.topology, offering.nodes
        )
        .into());
    }
    let cpu = firmware::Layout::find(&offering.topology, "cpu")?;
    let cpu_images = cpu_flash
        .iter()
        .map(|path| cpu.read_prepared(path))
        .collect::<Result<Vec<_>>>()?;
    let io_images = if io_flash.is_empty() {
        vec![]
    } else {
        let count = firmware::io_count(offering);
        if !profiles::has_io_prom(offering) || io_flash.len() != count {
            return Err(format!(
                "{} needs {count} --io-flash inputs in board order",
                offering.topology
            )
            .into());
        }
        let io = firmware::Layout::find(&offering.topology, "io")?;
        io_flash
            .iter()
            .map(|path| io.read_prepared(path))
            .collect::<Result<Vec<_>>>()?
    };
    create_with_images(
        dir,
        offering,
        memory_per_node,
        identity,
        graphics,
        inputs,
        &cpu_images,
        &io_images,
        None,
        None,
    )
}

fn create_with_images(
    dir: &Path,
    offering: &Offering,
    memory_per_node: Option<u32>,
    identity: Option<Origin300Create<'_>>,
    graphics: &str,
    inputs: std::collections::BTreeMap<String, String>,
    cpu_images: &[Vec<u8>],
    io_images: &[Vec<u8>],
    cpu_original: Option<&[u8]>,
    io_original: Option<&[u8]>,
) -> Result<()> {
    let memory_per_node = memory_per_node.unwrap_or(offering.memory.default);
    if let Some(parent) = dir.parent().filter(|parent| !parent.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    fs::create_dir(dir)?;
    let result = (|| -> Result<()> {
        for name in ["firmware", "drives", "state", "logs"] {
            fs::create_dir(dir.join(name))?;
        }
        if let Some(original) = cpu_original {
            fs::write(dir.join("firmware/prom.bin"), original)?;
        }
        if let Some(original) = io_original {
            fs::write(dir.join("firmware/io6prom.img"), original)?;
        }
        let identity = if let Some(inputs) = identity {
            if let (Some(dimm2), Some(dimm3)) = (inputs.spd_dimm2, inputs.spd_dimm3) {
                fs::copy(dimm2, dir.join("firmware/spd-dimm2.bin"))?;
                fs::copy(dimm3, dir.join("firmware/spd-dimm3.bin"))?;
            }
            Some(Origin300Identity {
                mac: inputs.mac.into(),
                spd_dimm2: inputs
                    .spd_dimm2
                    .map_or(String::new(), |_| "firmware/spd-dimm2.bin".into()),
                spd_dimm3: inputs
                    .spd_dimm3
                    .map_or(String::new(), |_| "firmware/spd-dimm3.bin".into()),
                chassis_eeprom: None,
                board_eeprom: None,
            })
        } else {
            None
        };
        let file = MachineFile {
            format: 1,
            machine: Machine {
                topology: Some(offering.topology.clone()),
                population: offering.cpus_per_node.clone(),
                inputs,
                model: offering.product.clone(),
                nodes: offering.nodes,
                cpus_per_node: offering.cpus_per_node[0],
                memory_per_node: format!("{memory_per_node}MiB"),
                graphics: graphics.into(),
                console: None,
            },
            firmware: Firmware {
                image: if cpu_original.is_some() {
                    "firmware/prom.bin".into()
                } else {
                    firmware::cpu_paths(Path::new(""), offering)[0]
                        .display()
                        .to_string()
                },
                io_image: if io_images.is_empty() {
                    None
                } else if io_original.is_some() {
                    Some("firmware/io6prom.img".into())
                } else {
                    Some("state/io-proms/io0.bin".into())
                },
            },
            identity,
            network: Network::default(),
            drive: vec![],
        };
        if let Some(identity) = &file.identity {
            origin300::validate_spd(dir, identity)?;
        }
        runtime::create_state(dir, &file, offering, cpu_images, io_images)?;
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
    fn embedded_offerings_match_pinned_qemu() {
        let product: serde_json::Value =
            serde_json::from_str(include_str!("../catalogue/sn-catalogue.json")).unwrap();
        let qemu: serde_json::Value =
            serde_json::from_str(include_str!("../qemu/hw/mips/sgi/sn-catalogue.json")).unwrap();
        let qemu_offerings = qemu["offerings"].as_array().unwrap();
        for offering in product["offerings"].as_array().unwrap() {
            let matching: Vec<_> = qemu_offerings
                .iter()
                .filter(|candidate| {
                    ["product", "topology", "nodes", "smp", "cpus-per-node"]
                        .iter()
                        .all(|key| candidate[key] == offering[*key])
                })
                .collect();
            assert_eq!(matching.len(), 1, "no unique QEMU offering for {offering}");
            for key in [
                "cpus-per-node",
                "default-cpu-model",
                "memory-per-node-mib",
                "firmware",
                "storage",
                "resources",
            ] {
                assert_eq!(
                    offering[key], matching[0][key],
                    "mismatched {key} for {offering}"
                );
            }
        }
    }

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
    fn local_boot_input_accepts_unlisted_containers_within_qemu_input_limit() {
        let root = std::env::temp_dir().join(format!("origami-local-prom-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("unlisted-container.bin");
        let catalog = catalogue().unwrap();
        for (profile, size) in [
            ("origin200-1", 1048576 + 128),
            ("origin300-2", 2 * 1048576),
            ("octane-impact", 2 * 1048576 + 128),
        ] {
            fs::write(&path, vec![0x5a; size]).unwrap();
            let offering = preset(&catalog, profile).unwrap();
            assert!(read_boot_prom(offering, &path).is_ok(), "{profile}");
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn local_boot_input_rejects_empty_directory_and_oversized_files() {
        let root =
            std::env::temp_dir().join(format!("origami-invalid-prom-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("prom.bin");
        let catalog = catalogue().unwrap();
        let offering = preset(&catalog, "origin200-1").unwrap();
        assert!(read_boot_prom(offering, &root).is_err());
        let input = fs::File::create(&path).unwrap();
        assert!(read_boot_prom(offering, &path).is_err());
        input.set_len(1048576 + 4096).unwrap();
        assert!(read_boot_prom(offering, &path).is_ok());
        input.set_len(1048576 + 4097).unwrap();
        assert!(read_boot_prom(offering, &path).is_err());
        drop(input);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn create_accepts_catalogue_memory_and_rejects_unsupported_memory() {
        use std::time::{SystemTime, UNIX_EPOCH};
        let root = std::env::temp_dir().join(format!(
            "sgi-create-memory-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&root).unwrap();
        let prom = root.join("prom.bin");
        fs::write(&prom, [0u8; 1]).unwrap();
        let catalog = catalogue().unwrap();
        let offer = preset(&catalog, "origin200-1").unwrap();
        let machine = root.join("nested/machine");
        create(&machine, offer, &prom, Some(128), None).unwrap();
        let file = read_machine(&machine).unwrap();
        assert_eq!(file.machine.memory_per_node, "128MiB");
        validate(&catalog, &machine, &file).unwrap();
        let rejected = root.join("rejected");
        assert!(create(&rejected, offer, &prom, Some(96), None).is_err());
        assert!(!rejected.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn downloadable_proms_match_catalogue_requirements() {
        let catalog = catalogue().unwrap();
        let registry = crate::assets::manifest().unwrap();
        for (id, offer) in presets(&catalog) {
            let profile = profiles::profile(&id).unwrap();
            let prom = registry.get(profile.boot_prom, "boot").unwrap();
            assert!(
                prom.size > 0
                    && prom.size
                        <= firmware::Layout::find(&offer.topology, "cpu")
                            .unwrap()
                            .size() as u64
                            + 4096,
                "{id}"
            );
            assert_eq!(
                profile.io_prom.is_some(),
                profiles::has_io_prom(offer),
                "{id}"
            );
            if let Some(id) = profile.io_prom {
                let prom = registry.get(id, "io").unwrap();
                assert!(prom.size > 0 && prom.size <= 1048576);
            }
        }
    }

    #[test]
    fn create_preflight_rejects_inputs_before_firmware_fetch() {
        let catalog = catalogue().unwrap();
        let offer = preset(&catalog, "origin200-1").unwrap();
        let absent = std::env::temp_dir().join(format!("origami-absent-{}", std::process::id()));
        assert!(validate_create_inputs(&absent, offer, Some(96), None).is_err());
        assert!(validate_create_inputs(Path::new("."), offer, None, None).is_err());
        let sn1 = preset(&catalog, "origin300-2").unwrap();
        assert!(validate_create_inputs(&absent, sn1, None, None).is_ok());
        assert!(!absent.exists());
    }

    #[test]
    fn create_preflight_rejects_unusable_destination_paths() {
        use std::time::{SystemTime, UNIX_EPOCH};
        let root = std::env::temp_dir().join(format!(
            "origami-create-path-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&root).unwrap();
        let catalog = catalogue().unwrap();
        let offer = preset(&catalog, "origin200-1").unwrap();
        let file = root.join("file");
        fs::write(&file, b"existing").unwrap();
        assert!(validate_create_inputs(&file.join("machine"), offer, None, None).is_err());
        #[cfg(unix)]
        {
            let link = root.join("dangling");
            std::os::unix::fs::symlink(root.join("absent"), &link).unwrap();
            assert!(validate_create_inputs(&link, offer, None, None).is_err());
            assert!(validate_create_inputs(&link.join("machine"), offer, None, None).is_err());
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn selected_presets_match_compiled_machine_catalogue() {
        let catalog = catalogue().unwrap();
        assert_eq!(presets(&catalog).len(), 12);
        for (_, offering) in presets(&catalog) {
            assert_eq!(offering.cpus_per_node.iter().sum::<u32>(), offering.smp);
        }
    }
}
