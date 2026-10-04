use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::net::SocketAddrV4;
use std::path::{Path, PathBuf};

pub mod assets;
pub mod catalogue;
pub mod control;
pub mod install;
pub mod origin300;
pub mod profiles;
pub mod runtime;

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// The machine.toml format. Format 2 machines keep QEMU-created storage.
pub const MACHINE_FORMAT: u32 = 2;

#[derive(Clone, Debug, Deserialize)]
pub struct Catalog {
    pub schema: String,
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
    /// The `-M` value selecting this product, topology and node count.
    #[serde(rename = "machine-options")]
    pub machine_options: String,
    #[serde(rename = "init-inputs")]
    pub init_inputs: Vec<InitInput>,
    pub storage: Vec<StorageItem>,
    #[serde(rename = "scsi-adapters")]
    pub scsi_adapters: Vec<ScsiAdapter>,
    #[serde(rename = "needs-debug-leds-off", default)]
    pub needs_debug_leds_off: bool,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Memory {
    pub accepted: Vec<u32>,
    pub default: u32,
}

/// A firmware image that creating a machine's storage reads. Its name is
/// also the init tool's option for that image.
#[derive(Clone, Debug, Deserialize)]
pub struct InitInput {
    pub name: String,
    pub kind: String,
    pub required: bool,
}

/// One writable store of a machine, named as QEMU names its file, block
/// node and machine property.
#[derive(Clone, Debug, Deserialize)]
pub struct StorageItem {
    pub name: String,
    pub size: u64,
    #[serde(rename = "read-only")]
    pub read_only: bool,
    pub initial: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ScsiAdapter {
    pub bus: String,
    pub targets: Vec<u32>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MachineFile {
    pub format: u32,
    pub machine: Machine,
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

pub fn sha256_hex(data: &[u8]) -> String {
    Sha256::digest(data)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

pub fn sha256_file(path: &Path) -> Result<String> {
    Ok(sha256_hex(&fs::read(path)?))
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
}

fn default_graphics() -> String {
    "none".into()
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

/// The machine catalogue of the QEMU that launches guests.
pub fn catalogue() -> Result<Catalog> {
    catalogue::load(&runtime::qemu_path()?)
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
    // Format 1 has sections format 2 refuses, so check it first.
    if toml::from_str::<toml::Table>(&text)
        .ok()
        .and_then(|table| table.get("format")?.as_integer())
        == Some(1)
    {
        return Err(format!(
            "{} was created by an earlier Origami and cannot be opened. \
             Create a new machine with origami create and attach its disks",
            path.display()
        )
        .into());
    }
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
    if file.format != MACHINE_FORMAT {
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
                && file.machine.topology.as_ref() == Some(&o.topology)
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
    runtime::check_state(dir, offering)?;
    if let (true, Some(identity)) = (offering.product == "origin300", &file.identity) {
        if offering.nodes != 1 || offering.smp != 2 {
            return Err("explicit Origin 300 identity currently requires one two-CPU node".into());
        }
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
            .scsi_adapters
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
    if let Some(inputs) = identity {
        if offering.product != "origin300" {
            return Err("SPD inputs are only supported for Origin 300".into());
        }
        if offering.nodes != 1 || offering.smp != 2 {
            return Err("explicit SPD inputs and persistent IP35 flash currently require one two-CPU node; omit them to use native QEMU defaults".into());
        }
        origin300::validate_spd_file(inputs.spd_dimm2, origin300::DIMM2_SHA256)?;
        origin300::validate_spd_file(inputs.spd_dimm3, origin300::DIMM3_SHA256)?;
        if !valid_mac(inputs.mac) {
            return Err("Origin 300 MAC must contain six hexadecimal bytes".into());
        }
    }
    Ok(())
}

/// What creates a new machine's storage: QEMU's init tool and the original
/// firmware images it reads.
pub struct MachineInit<'a> {
    pub tool: &'a Path,
    pub boot_prom: &'a Path,
    pub io_prom: Option<&'a Path>,
}

pub fn create_configured(
    dir: &Path,
    offering: &Offering,
    init: &MachineInit<'_>,
    memory_per_node: Option<u32>,
    identity: Option<Origin300Create<'_>>,
    graphics: Option<&str>,
    inputs: std::collections::BTreeMap<String, String>,
) -> Result<()> {
    let graphics = graphics.unwrap_or(profiles::default_graphics(offering));
    let memory_per_node = memory_per_node.unwrap_or(offering.memory.default);
    let arguments =
        runtime::init_arguments(offering, init.boot_prom, init.io_prom, &dir.join("state"))?;
    if let Some(parent) = dir.parent().filter(|parent| !parent.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    fs::create_dir(dir)?;
    let result = (|| -> Result<()> {
        for name in ["drives", "logs"] {
            fs::create_dir(dir.join(name))?;
        }
        runtime::create_state(init.tool, &arguments)?;
        let identity = if let Some(inputs) = identity {
            fs::create_dir(dir.join("firmware"))?;
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
            format: MACHINE_FORMAT,
            machine: Machine {
                topology: Some(offering.topology.clone()),
                population: offering.cpus_per_node.clone(),
                inputs,
                model: offering.product.clone(),
                nodes: offering.nodes,
                cpus_per_node: offering.cpus_per_node[0],
                memory_per_node: format!("{memory_per_node}MiB"),
                graphics: graphics.into(),
            },
            identity,
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

/// A hand-trimmed excerpt of a `query-sgi-machines` reply: one offering of
/// each kind the unit tests exercise. The product build's product-state test
/// reads the real catalogue.
#[cfg(test)]
pub(crate) fn test_catalogue() -> Catalog {
    catalogue::parse(include_str!("../tests/fixtures/sgi-machines.json")).unwrap()
}

/// Write an executable script without this process ever holding it open
/// for writing. A child forked meanwhile by another test thread would
/// inherit such a descriptor, and running the script would then fail with
/// "Text file busy" until that child calls exec.
#[cfg(all(test, unix))]
pub(crate) fn write_script(path: &Path, script: &str) {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let mut writer = Command::new("sh")
        .args(["-c", "cat > \"$1\" && chmod 755 \"$1\"", "sh"])
        .arg(path)
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    writer
        .stdin
        .take()
        .unwrap()
        .write_all(script.as_bytes())
        .unwrap();
    assert!(writer.wait().unwrap().success());
}

/// A stand-in for QEMU's init tool that creates `offering`'s storage files
/// zero-filled and records its arguments in `<tool>.args`.
#[cfg(all(test, unix))]
pub(crate) fn fake_init_tool(root: &Path, offering: &Offering) -> PathBuf {
    let tool = root.join("qemu-sgi-machine-init");
    let mut script = String::from(
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$0.args\"\nfor dir; do :; done\nmkdir \"$dir\" || exit 1\n",
    );
    for item in &offering.storage {
        script.push_str(&format!(
            "dd if=/dev/zero of=\"$dir/{}.raw\" bs=1 count=0 seek={} 2>/dev/null || exit 1\n",
            item.name, item.size
        ));
    }
    write_script(&tool, &script);
    tool
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

    #[cfg(unix)]
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
        let catalog = test_catalogue();
        let offer = preset(&catalog, "origin200-1").unwrap();
        let tool = fake_init_tool(&root, offer);
        let init = MachineInit {
            tool: &tool,
            boot_prom: &prom,
            io_prom: None,
        };
        let machine = root.join("nested/machine");
        create_configured(
            &machine,
            offer,
            &init,
            Some(128),
            None,
            None,
            Default::default(),
        )
        .unwrap();
        let file = read_machine(&machine).unwrap();
        assert_eq!(file.machine.memory_per_node, "128MiB");
        validate(&catalog, &machine, &file).unwrap();
        let missing_tool = root.join("absent-tool");
        let broken = MachineInit {
            tool: &missing_tool,
            ..init
        };
        let interrupted = root.join("new/parents/machine");
        assert!(create_configured(
            &interrupted,
            offer,
            &broken,
            None,
            None,
            None,
            Default::default()
        )
        .is_err());
        assert!(!interrupted.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn downloadable_proms_name_catalogue_presets() {
        let catalog = test_catalogue();
        let manifest = crate::assets::manifest().unwrap();
        for (profile, _) in presets(&catalog) {
            assert_eq!(
                manifest
                    .proms
                    .iter()
                    .filter(|prom| prom.profiles.contains(&profile))
                    .count(),
                1,
                "missing or ambiguous PROM: {profile}"
            );
        }
    }

    #[test]
    fn create_preflight_rejects_inputs_before_firmware_fetch() {
        let catalog = test_catalogue();
        let offer = preset(&catalog, "origin200-1").unwrap();
        let absent = std::env::temp_dir().join(format!("origami-absent-{}", std::process::id()));
        assert!(validate_create_inputs(&absent, offer, Some(96), None).is_err());
        assert!(validate_create_inputs(Path::new("."), offer, None, None).is_err());
        let sn1 = preset(&catalog, "fuel-1").unwrap();
        assert!(validate_create_inputs(&absent, sn1, None, None).is_ok());
        assert!(!absent.exists());
    }

    #[cfg(unix)]
    #[test]
    fn create_runs_the_init_tool_and_reopen_requires_its_storage() {
        use std::time::{SystemTime, UNIX_EPOCH};
        let root = std::env::temp_dir().join(format!(
            "origami-create-state-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&root).unwrap();
        let prom = root.join("boot prom.img");
        let io_prom = root.join("io,prom.img");
        let catalog = test_catalogue();
        let offer = preset(&catalog, "origin2000-8").unwrap();
        let tool = fake_init_tool(&root, offer);
        let machine = root.join("machine");
        let without_io = MachineInit {
            tool: &tool,
            boot_prom: &prom,
            io_prom: None,
        };
        let error = create_configured(
            &machine,
            offer,
            &without_io,
            None,
            None,
            None,
            Default::default(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("--io-prom"), "{error}");
        assert!(!machine.exists());
        let init = MachineInit {
            io_prom: Some(&io_prom),
            ..without_io
        };
        create_configured(&machine, offer, &init, None, None, None, Default::default()).unwrap();
        let arguments = fs::read_to_string(root.join("qemu-sgi-machine-init.args")).unwrap();
        let state = machine.join("state");
        assert_eq!(
            arguments.lines().collect::<Vec<_>>(),
            [
                "--machine",
                "origin2000,topology=origin2000-rack,nodes=4",
                "--boot-prom",
                prom.to_str().unwrap(),
                "--io-prom",
                io_prom.to_str().unwrap(),
                state.to_str().unwrap(),
            ]
        );
        let file = read_machine(&machine).unwrap();
        assert!(!machine.join("firmware").exists());
        validate(&catalog, &machine, &file).unwrap();
        fs::remove_file(state.join("node3-flash.raw")).unwrap();
        let error = validate(&catalog, &machine, &file).unwrap_err().to_string();
        assert!(error.contains("node3-flash.raw"), "{error}");
        assert!(error.contains("origami create"), "{error}");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn machines_from_an_earlier_format_ask_to_be_recreated() {
        let root = std::env::temp_dir().join(format!("origami-format-1-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        fs::write(
            root.join("machine.toml"),
            "format = 1\n[machine]\nmodel = \"origin200\"\n[firmware]\nimage = \"firmware/prom.bin\"\n",
        )
        .unwrap();
        let error = read_machine(&root).unwrap_err().to_string();
        assert!(error.contains("earlier Origami"), "{error}");
        assert!(error.contains("origami create"), "{error}");
        fs::remove_dir_all(root).unwrap();
    }
}
