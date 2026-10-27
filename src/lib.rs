use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::net::SocketAddrV4;
use std::path::{Path, PathBuf};

pub mod assets;
pub mod control;
pub mod graphics;
pub mod install;
pub mod listing;
pub mod machines;
pub mod migrate;
pub mod nvram;
pub mod profiles;
pub mod runtime;

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// The machine.toml format. Format 2 machines keep QEMU-created storage;
/// format 3 adds the module controller NVRAM; format 4 stores each graphics
/// pipe in place of one graphics name.
pub const MACHINE_FORMAT: u32 = 4;

#[derive(Clone, Debug, Deserialize)]
pub struct Machines {
    pub offerings: Vec<Offering>,
    #[serde(default)]
    pub topologies: Vec<Topology>,
}

/// A topology in the machine list, read for the processor modules and
/// graphics pipes it offers.
#[derive(Clone, Debug, Deserialize)]
pub struct Topology {
    pub name: String,
    /// The modules `processor=` selects, the default first; empty where the
    /// topology offers no choice.
    #[serde(rename = "processor-options", default)]
    pub processor_options: Vec<ProcessorOption>,
    /// The graphics pipes it takes; absent where it has no graphics slot.
    #[serde(default)]
    pub graphics: Option<graphics::Graphics>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ProcessorOption {
    pub assembly: ProcessorAssembly,
    #[serde(rename = "accepted-memory-mib")]
    pub accepted_memory: Vec<u32>,
    #[serde(rename = "cpus-per-node")]
    pub cpus_per_node: Vec<u32>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ProcessorAssembly {
    pub id: String,
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
    pub processor: Processor,
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
    /// Machine options this offering accepts beyond its storage.
    #[serde(rename = "hardware-inputs")]
    pub hardware_inputs: Vec<InputBinding>,
    /// Values a launch may override, and where each is set.
    pub overrides: Vec<Override>,
    /// Serial lines in -serial order; the line of kind `console` is the
    /// primary console.
    pub consoles: Vec<Console>,
}

/// The processor every CPU of an offering is.
#[derive(Clone, Debug, Deserialize)]
pub struct Processor {
    pub id: String,
    pub part: String,
    #[serde(rename = "clock-hz")]
    pub clock_hz: u64,
    #[serde(rename = "cache-mib")]
    pub cache_mib: u32,
}

impl std::fmt::Display for Processor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} MHz {}, {} MB secondary cache",
            self.clock_hz / 1_000_000,
            self.part,
            self.cache_mib
        )
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct Override {
    pub property: String,
    /// `machine` for a -M property, `cpu` for a -cpu property.
    pub target: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct InputBinding {
    pub option: String,
    pub required: bool,
}

/// A serial line the machine offers, named by the chardev its argument uses.
#[derive(Clone, Debug, Deserialize)]
pub struct Console {
    pub kind: String,
    pub node: u32,
    pub argument: String,
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
    pub identity: Option<Identity>,
    #[serde(default)]
    pub network: Network,
    /// The graphics pipes; none for a machine without graphics.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pipe: Vec<graphics::Pipe>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub drive: Vec<Drive>,
    /// Escape hatch for QEMU options the CLI does not generate correctly.
    #[serde(default, skip_serializing_if = "Qemu::is_empty")]
    pub qemu: Qemu,
}

/// Raw QEMU options, unsupported: they are neither validated nor migrated.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Qemu {
    /// Appended verbatim after every option the CLI generates.
    #[serde(default, rename = "extra-args", skip_serializing_if = "Vec::is_empty")]
    pub extra_args: Vec<String>,
}

impl Qemu {
    fn is_empty(&self) -> bool {
        self.extra_args.is_empty()
    }
}

/// Identity QEMU builds into the machine's own records. An SN1 machine's L1
/// serves `mac` to its firmware and gives it to the IOC3.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    pub mac: String,
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
    /// Host interface a tap network attaches to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interface: Option<String>,
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
            interface: None,
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
    /// The processor module, where the topology offers a choice; absent
    /// means its default. Each node's flash records the module when the
    /// machine is created, so it never changes afterwards.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub processor: Option<String>,
    pub memory_per_node: String,
    /// The chardev name of the console line, such as `ioc3_a`, when it
    /// differs from the machine list's primary console.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub console: Option<String>,
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

/// The machine list of the QEMU that launches guests.
pub fn machines() -> Result<Machines> {
    machines::load(&runtime::qemu_path()?)
}

pub fn presets(machines: &Machines) -> Vec<(String, &Offering)> {
    profiles::presets(machines)
}

pub fn preset<'a>(machines: &'a Machines, name: &str) -> Result<&'a Offering> {
    presets(machines)
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
    // Format 1 has sections the current format refuses, so check it first.
    let release = match migrate::format(&text) {
        Some(1) => Some("0.1"),
        Some(2) => Some("0.2"),
        Some(3) => Some("0.3"),
        _ => None,
    };
    if let Some(release) = release {
        return Err(format!(
            "{} is from Origami {release}; upgrade it once with origami upgrade {}",
            path.display(),
            dir.display()
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

/// Give the machine `mac`, the one Ethernet address QEMU uses for its
/// identity records and onboard adapter. A machine keeps the address it has:
/// changing it would change the identity an installed guest knows.
pub fn set_machine_mac(file: &mut MachineFile, mac: Option<&str>) -> Result<()> {
    let Some(mac) = mac else { return Ok(()) };
    if !valid_mac(mac) {
        return Err("--mac must contain six hexadecimal bytes".into());
    }
    let mac = mac.to_ascii_lowercase();
    match &file.identity {
        Some(identity) if !identity.mac.eq_ignore_ascii_case(&mac) => Err(format!(
            "this machine's MAC is {}; omit --mac or pass that address",
            identity.mac
        )
        .into()),
        _ => {
            file.identity = Some(Identity { mac });
            Ok(())
        }
    }
}

pub fn valid_mac(mac: &str) -> bool {
    let parts: Vec<_> = mac.split(':').collect();
    parts.len() == 6
        && parts
            .iter()
            .all(|part| part.len() == 2 && u8::from_str_radix(part, 16).is_ok())
}

/// The offering in the machine list a machine was created from.
pub fn offering<'a>(machines: &'a Machines, machine: &Machine) -> Result<&'a Offering> {
    let topology = machine.topology.as_deref().unwrap_or_default();
    if let Some(id) = &machine.processor {
        profiles::processor_option(machines, topology, id)?;
    }
    machines
        .offerings
        .iter()
        .find(|o| {
            o.product == machine.model
                && o.nodes == machine.nodes
                && machine.topology.as_ref() == Some(&o.topology)
                && o.cpus_per_node == profiles::population(machine)
                && match &machine.processor {
                    Some(id) => &o.processor.id == id,
                    None => profiles::fits_default_processor(machines, o),
                }
        })
        .ok_or_else(|| match &machine.processor {
            Some(id) => format!(
                "{topology} does not offer {id} with {:?} CPUs per node",
                profiles::population(machine)
            )
            .into(),
            None => "unsupported machine and processor population".into(),
        })
}

/// Refuse `memory` MiB per node unless `offering` accepts it.
fn check_memory(offering: &Offering, memory: u32) -> Result<()> {
    if offering.memory.accepted.contains(&memory) {
        return Ok(());
    }
    let accepted: Vec<_> = offering
        .memory
        .accepted
        .iter()
        .map(u32::to_string)
        .collect();
    Err(format!(
        "{memory} MiB per node is not offered for {} with {} MHz {}; choose {}",
        offering.topology,
        offering.processor.clock_hz / 1_000_000,
        offering.processor.part,
        accepted.join(", ")
    )
    .into())
}

/// Check the network choice against what this host's QEMU can run. Tap
/// names the host interface in `interface`; private and vmnet use `endpoint`.
pub(crate) fn validate_network(file: &MachineFile) -> Result<()> {
    let mode = file.network.mode.as_str();
    match mode {
        "none" | "user" if file.network.endpoint.is_none() && file.network.interface.is_none() => {
            return Ok(())
        }
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
        }
        // tap-bsd needs a third-party kernel extension that Apple silicon
        // does not load, so macOS uses vmnet instead.
        "tap" if cfg!(target_os = "macos") => {
            return Err("tap networking is unavailable on macOS; use --mode vmnet".into())
        }
        "tap" => {
            if file.network.interface.as_deref().unwrap_or("").is_empty() {
                return Err("tap network needs --interface".into());
            }
        }
        "vmnet" if !cfg!(target_os = "macos") => return Err("vmnet networking needs macOS".into()),
        "vmnet" => {
            let endpoint = file.network.endpoint.as_deref().unwrap_or("");
            if endpoint.is_empty() || endpoint.starts_with("tcp:") {
                return Err("vmnet network needs the socket_vmnet socket path".into());
            }
        }
        _ => return Err("network mode must be user, none, private, tap or vmnet".into()),
    }
    // Installation serves the guest by its Ethernet address, and machines
    // sharing a LAN or vmnet subnet would collide on QEMU's default one.
    if file.identity.is_none() {
        return Err(format!("{mode} networking needs the machine's MAC; supply --mac").into());
    }
    Ok(())
}

pub fn validate<'a>(
    machines: &'a Machines,
    dir: &Path,
    file: &MachineFile,
) -> Result<&'a Offering> {
    if file.format != MACHINE_FORMAT {
        return Err(format!("unsupported machine format {}", file.format).into());
    }
    let memory = file
        .machine
        .memory_per_node
        .strip_suffix("MiB")
        .ok_or("memory_per_node must use MiB")?
        .parse::<u32>()?;
    let offering = offering(machines, &file.machine)?;
    check_memory(offering, memory)?;
    graphics::validate(machines, &offering.topology, offering.nodes, &file.pipe)?;
    profiles::validate_inputs(offering, &file.machine.inputs)?;
    runtime::console_line(offering, file.machine.console.as_deref())?;
    validate_network(file)?;
    runtime::check_extra_args(&file.qemu.extra_args)?;
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
    if let Some(identity) = &file.identity {
        if !valid_mac(&identity.mac) {
            return Err("identity MAC must contain six hexadecimal bytes".into());
        }
        if !profiles::takes_mac(offering) {
            return Err(format!("{} takes no machine MAC", offering.topology).into());
        }
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
    mac: Option<&str>,
) -> Result<()> {
    check_memory(offering, memory_per_node.unwrap_or(offering.memory.default))?;
    match fs::symlink_metadata(dir) {
        Ok(_) => return Err(format!("destination already exists: {}", dir.display()).into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    if mac.is_some_and(|mac| !valid_mac(mac)) {
        return Err("--mac must contain six hexadecimal bytes".into());
    }
    if mac.is_some() && !profiles::takes_mac(offering) {
        return Err(format!("{} takes no machine MAC", offering.topology).into());
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

/// The boot and IO PROM images that create `offering`'s storage: the files
/// given, else verified downloads of the kinds its machine list entry reads.
pub fn firmware(
    offering: &Offering,
    boot_prom: Option<&Path>,
    io_prom: Option<&Path>,
) -> Result<(PathBuf, Option<PathBuf>)> {
    let image = |name: &str, given: Option<&Path>, option: &str| -> Result<Option<PathBuf>> {
        if let Some(path) = given {
            return Ok(Some(path.into()));
        }
        let Some(input) = offering
            .init_inputs
            .iter()
            .find(|input| input.name == name && input.required)
        else {
            return Ok(None);
        };
        assets::acquire(&input.kind)
            .map(Some)
            .map_err(|error| format!("{error}; supply {option} FILE").into())
    };
    let boot_prom = image("boot-prom", boot_prom, "--prom")?
        .ok_or_else(|| format!("{} reads no boot PROM", offering.topology))?;
    Ok((boot_prom, image("io-prom", io_prom, "--io-prom")?))
}

pub fn create_configured(
    dir: &Path,
    offering: &Offering,
    init: &MachineInit<'_>,
    memory_per_node: Option<u32>,
    mac: Option<&str>,
    pipes: Vec<graphics::Pipe>,
    inputs: std::collections::BTreeMap<String, String>,
) -> Result<()> {
    let memory_per_node = memory_per_node.unwrap_or(offering.memory.default);
    let arguments =
        runtime::init_arguments(offering, init.boot_prom, init.io_prom, &dir.join("state"));
    if let Some(parent) = dir.parent().filter(|parent| !parent.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    fs::create_dir(dir)?;
    let result = (|| -> Result<()> {
        for name in ["drives", "logs"] {
            fs::create_dir(dir.join(name))?;
        }
        runtime::create_state(init.tool, &arguments)?;
        let file = MachineFile {
            format: MACHINE_FORMAT,
            machine: Machine {
                topology: Some(offering.topology.clone()),
                population: offering.cpus_per_node.clone(),
                inputs,
                model: offering.product.clone(),
                nodes: offering.nodes,
                cpus_per_node: offering.cpus_per_node[0],
                processor: profiles::selects_processor(offering)
                    .then(|| offering.processor.id.clone()),
                memory_per_node: format!("{memory_per_node}MiB"),
                console: None,
            },
            identity: mac.map(|mac| Identity {
                mac: mac.to_ascii_lowercase(),
            }),
            network: Network::default(),
            pipe: pipes,
            drive: vec![],
            qemu: Default::default(),
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
/// reads the real machine list.
#[cfg(test)]
pub(crate) fn test_machines() -> Machines {
    serde_json::from_str(include_str!("../tests/fixtures/sgi-machines.json")).unwrap()
}

/// The stand-in QEMU tools the command tests use.
#[cfg(all(test, unix))]
#[path = "../tests/common/mod.rs"]
pub(crate) mod test_support;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qemu_section_round_trips_and_is_omitted_when_empty() {
        let base = "format = 4\n[machine]\nmodel = \"origin200\"\nnodes = 1\ncpus_per_node = 1\nmemory_per_node = \"256MiB\"\n";
        let plain: MachineFile = toml::from_str(base).unwrap();
        assert!(plain.qemu.extra_args.is_empty());
        let text = toml::to_string_pretty(&plain).unwrap();
        assert!(!text.contains("qemu"), "{text}");

        let with = format!(
            "{base}[qemu]\nextra-args = [\"-global\", \"bcm570x.mac=08:00:69:12:34:56\"]\n"
        );
        let file: MachineFile = toml::from_str(&with).unwrap();
        assert_eq!(
            file.qemu.extra_args,
            ["-global", "bcm570x.mac=08:00:69:12:34:56"]
        );
        let again: MachineFile = toml::from_str(&toml::to_string_pretty(&file).unwrap()).unwrap();
        assert_eq!(again.qemu.extra_args, file.qemu.extra_args);
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

    #[cfg(unix)]
    #[test]
    fn create_accepts_listed_memory_and_rejects_unsupported_memory() {
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
        let machines = test_machines();
        let offer = preset(&machines, "origin200-1").unwrap();
        let tool = test_support::init_tool(&root);
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
            vec![],
            Default::default(),
        )
        .unwrap();
        let file = read_machine(&machine).unwrap();
        assert_eq!(file.machine.memory_per_node, "128MiB");
        validate(&machines, &machine, &file).unwrap();
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
            vec![],
            Default::default()
        )
        .is_err());
        assert!(!interrupted.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn create_preflight_rejects_inputs_before_firmware_fetch() {
        let machines = test_machines();
        let offer = preset(&machines, "origin200-1").unwrap();
        let absent = std::env::temp_dir().join(format!("origami-absent-{}", std::process::id()));
        assert!(validate_create_inputs(&absent, offer, Some(96), None).is_err());
        assert!(validate_create_inputs(Path::new("."), offer, None, None).is_err());
        let sn1 = preset(&machines, "fuel-1").unwrap();
        assert!(validate_create_inputs(&absent, sn1, None, None).is_ok());
        assert!(!absent.exists());
    }

    #[cfg(unix)]
    #[cfg(unix)]
    #[test]
    fn private_networking_uses_the_machine_mac() {
        let root = std::env::temp_dir().join(format!("origami-identity-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir(&root).unwrap();
        let machines = test_machines();
        let offer = preset(&machines, "origin200-1").unwrap();
        let tool = test_support::init_tool(&root);
        let prom = root.join("prom.img");
        let init = MachineInit {
            tool: &tool,
            boot_prom: &prom,
            io_prom: None,
        };
        let machine = root.join("machine");
        assert!(validate_create_inputs(&machine, offer, None, Some("08:00:69:zz:34:56")).is_err());
        create_configured(
            &machine,
            offer,
            &init,
            None,
            Some("02:00:5d:aa:bb:cc"),
            vec![],
            Default::default(),
        )
        .unwrap();
        let mut file = read_machine(&machine).unwrap();
        assert_eq!(file.identity.as_ref().unwrap().mac, "02:00:5d:aa:bb:cc");
        file.network = Network {
            mode: "private".into(),
            endpoint: Some("tcp:127.0.0.1:4242".into()),
            interface: None,
            forward: vec![],
        };
        validate(&machines, &machine, &file).unwrap();
        let error = set_machine_mac(&mut file, Some("08:00:69:12:34:56"))
            .unwrap_err()
            .to_string();
        assert!(error.contains("02:00:5d:aa:bb:cc"), "{error}");
        set_machine_mac(&mut file, Some("02:00:5D:AA:BB:CC")).unwrap();
        file.identity = None;
        let error = validate(&machines, &machine, &file)
            .unwrap_err()
            .to_string();
        assert!(error.contains("--mac"), "{error}");
        set_machine_mac(&mut file, Some("08:00:69:12:34:56")).unwrap();
        validate(&machines, &machine, &file).unwrap();
        fs::remove_dir_all(root).unwrap();
    }
}
