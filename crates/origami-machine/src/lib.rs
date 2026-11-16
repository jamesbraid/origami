use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::net::SocketAddrV4;
use std::path::{Path, PathBuf};

pub mod addressing;
pub mod assets;
pub mod console;
pub mod control;
pub mod dir;
pub mod emu_id;
pub mod graphics;
pub mod install;
pub mod listing;
pub mod mac;
pub mod migrate;
pub mod model;
pub mod nvram;
pub mod profiles;
pub mod serial;
pub mod settings;
pub mod shares;
pub mod stores;

/// The error every fallible call here returns. It is `Send + Sync`, so it
/// crosses threads and the facade's event queue; strings, I/O, TOML and JSON
/// errors convert with `?` and `.into()`, and `downcast_ref` still finds the
/// source.
pub type Error = Box<dyn std::error::Error + Send + Sync>;

pub type Result<T> = std::result::Result<T, Error>;

/// The machine.toml format. Format 2 machines keep QEMU-created storage;
/// format 3 adds the module controller NVRAM; format 4 stores each graphics
/// pipe in place of one graphics name; format 5 names the engine in
/// `[engine]` and records the firmware that seeded the stores.
pub const MACHINE_FORMAT: u32 = 5;

/// The last format that kept QEMU's options in `[qemu]`. It is read, and
/// upgraded to the current format on `origami upgrade`.
pub const FORMAT_WITH_QEMU_TABLE: u32 = 4;

/// A machine.toml written by a later Origami than this one reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TooNew {
    pub found: i64,
    pub path: Option<PathBuf>,
}

impl std::fmt::Display for TooNew {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(path) = &self.path {
            write!(f, "{} is ", path.display())?;
        }
        write!(
            f,
            "machine format {}, which this Origami does not read (it reads up to format \
             {MACHINE_FORMAT}); use a newer Origami",
            self.found
        )
    }
}

impl std::error::Error for TooNew {}

#[derive(Clone, Debug, Deserialize)]
pub struct Machines {
    /// The catalogue's schema version, as the engine that supplied it
    /// numbers it.
    #[serde(default)]
    pub version: u32,
    pub offerings: Vec<Offering>,
    #[serde(default)]
    pub topologies: Vec<Topology>,
    /// What each setting an offering lists is, written once.
    #[serde(rename = "setting-definitions", default)]
    pub setting_definitions: Vec<settings::Definition>,
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
    /// Unique in the machine list: `TOPOLOGY.POPULATION[.PROCESSOR]`.
    pub id: String,
    pub product: String,
    pub topology: String,
    pub nodes: u32,
    /// The CPUs the offering starts with, in all.
    #[serde(alias = "smp")]
    pub cpus: u32,
    #[serde(rename = "cpus-per-node")]
    pub cpus_per_node: Vec<u32>,
    /// What the guest's host id derives from: `text`, `module`, `address`
    /// or `o2`. Every offering accepts the identity properties.
    #[serde(rename = "hostid-source")]
    pub hostid_source: String,
    pub processor: Processor,
    /// Whether the offering fits a processor module other than its
    /// topology's default, which a machine then stores.
    #[serde(rename = "selects-processor", default)]
    pub selects_processor: bool,
    #[serde(rename = "memory-per-node-mib")]
    pub memory: Memory,
    pub storage: Vec<StorageItem>,
    #[serde(rename = "scsi-adapters")]
    pub scsi_adapters: Vec<ScsiAdapter>,
    /// The machine inputs a launch may give with `--set`, each defined in
    /// the machine list's `setting-definitions`.
    #[serde(default)]
    pub settings: Vec<settings::OfferingSetting>,
    /// Settings that go together.
    #[serde(rename = "setting-groups", default)]
    pub setting_groups: Vec<settings::Group>,
    /// Serial lines in launch order; the line of kind `console` is the
    /// primary console.
    pub consoles: Vec<Console>,
    /// The onboard Ethernets.
    #[serde(default)]
    pub networks: Vec<NetworkAdapter>,
    /// The size of the boot PROM's flash array, for a machine that takes the
    /// PROM as a machine option.
    #[serde(default)]
    pub firmware: Option<Firmware>,
    /// How the guest's time runs; absent from an engine whose machine list
    /// does not say.
    #[serde(default)]
    pub clock: Option<Clock>,
    /// The engines that run this offering.
    #[serde(default)]
    pub engines: Vec<String>,
    /// What each engine needs to run the offering, keyed by engine id. Only
    /// that engine's adapter reads its value.
    #[serde(default)]
    pub engine: BTreeMap<String, serde_json::Value>,
}

/// The boot PROM's place in the machine's flash.
#[derive(Clone, Debug, Deserialize)]
pub struct Firmware {
    /// The flash array's size in bytes.
    pub size: u64,
}

/// The guest-time choices of a machine. Only the defaults matter to a
/// launch.
#[derive(Clone, Debug, Deserialize)]
pub struct Clock {
    /// The `-icount` shift the machine starts with; absent for a machine
    /// that runs without instruction counting.
    #[serde(rename = "default-icount-shift", default)]
    pub default_icount_shift: Option<u32>,
    /// The `-rtc clock=` that goes with instruction counting.
    #[serde(rename = "default-rtc", default)]
    pub default_rtc: Option<String>,
}

impl Offering {
    /// The onboard Ethernet a launch binds and the PROM netboots from.
    pub fn default_network(&self) -> Option<&NetworkAdapter> {
        self.default_network_index()
            .map(|index| &self.networks[index])
    }

    /// The position of [`Offering::default_network`] in `networks`.
    pub fn default_network_index(&self) -> Option<usize> {
        self.networks
            .iter()
            .position(|adapter| adapter.default)
            .or((!self.networks.is_empty()).then_some(0))
    }
}

/// An onboard Ethernet.
#[derive(Clone, Debug, Deserialize)]
pub struct NetworkAdapter {
    /// The chip, such as `bcm5701-ethernet`.
    pub kind: String,
    /// The guest's name for it, such as `tg0`.
    #[serde(default)]
    pub interface: Option<String>,
    #[serde(default)]
    pub default: bool,
    #[serde(default)]
    pub mac: Option<AdapterMac>,
}

impl NetworkAdapter {
    /// The chip's name for people: `bcm5701-ethernet` is BCM5701.
    pub fn chip(&self) -> String {
        self.kind
            .strip_suffix("-ethernet")
            .unwrap_or(&self.kind)
            .to_ascii_uppercase()
    }
}

/// Where an onboard Ethernet's address comes from.
#[derive(Clone, Debug, Deserialize)]
pub struct AdapterMac {
    /// True when it follows the machine's identity address; false when the
    /// adapter takes an address of its own.
    #[serde(default)]
    pub derived: bool,
    /// What this NIC's address adds to the identity address, when `derived`.
    #[serde(default)]
    pub offset: u32,
    /// This NIC's address when the identity address is not set.
    #[serde(rename = "default-address", default)]
    pub default_address: Option<String>,
    /// The prefix the PROM needs on this NIC's address.
    #[serde(rename = "required-prefix", default)]
    pub required_prefix: Option<String>,
}

/// The first three bytes of an address QEMU generates for a NIC, SGI's own.
pub(crate) const SGI_OUI: &str = "08:00:69";

impl Offering {
    /// The prefix the machine's addresses must start with: the one the
    /// machine list says its PROM needs, else the one its default address
    /// has, else SGI's own.
    pub fn mac_prefix(&self) -> &str {
        self.default_network()
            .and_then(|adapter| adapter.mac.as_ref())
            .and_then(|mac| {
                mac.required_prefix.as_deref().or_else(|| {
                    mac.default_address
                        .as_deref()
                        .and_then(|address| address.get(..SGI_OUI.len()))
                })
            })
            .unwrap_or(SGI_OUI)
    }
}

/// Refuse a machine MAC the machine's PROM does not accept: one outside the
/// prefix the machine list names.
pub fn check_mac(offering: &Offering, mac: &str) -> Result<()> {
    let prefix = offering.mac_prefix();
    if mac
        .get(..prefix.len())
        .is_some_and(|start| start.eq_ignore_ascii_case(prefix))
    {
        return Ok(());
    }
    Err(format!(
        "{} needs an Ethernet address starting {prefix}, not {mac}; its PROM refuses other addresses",
        listing::machine_name(&offering.topology)
    )
    .into())
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

/// A serial line the machine offers.
#[derive(Clone, Debug, Deserialize)]
pub struct Console {
    pub kind: String,
    pub node: u32,
    /// The name a console choice uses, such as `ioc3_a`; absent for a line
    /// the machine list does not name.
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Memory {
    pub accepted: Vec<u32>,
    pub default: u32,
}

/// One writable store of a machine, named as its file under `state/`.
#[derive(Clone, Debug, Deserialize)]
pub struct StorageItem {
    pub name: String,
    pub size: u64,
    #[serde(rename = "read-only")]
    pub read_only: bool,
    pub initial: String,
}

impl Offering {
    /// The SCSI bus a drive's `bus` number names: the machine list's
    /// adapters in order, whatever QEMU calls each (`scsi.0`, `aic0.0`).
    pub fn scsi_bus(&self, index: u32) -> Option<&ScsiAdapter> {
        self.scsi_adapters.get(index as usize)
    }

    /// That bus's QEMU name; `scsi.N` for a number the machine lacks.
    pub fn scsi_bus_name(&self, index: u32) -> String {
        self.scsi_bus(index)
            .map_or_else(|| format!("scsi.{index}"), |adapter| adapter.bus.clone())
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct ScsiAdapter {
    pub bus: String,
    pub targets: Vec<u32>,
    /// The bus's kind, such as `fc-loop` for a Fibre Channel private loop;
    /// absent from a QEMU whose machine list does not name it.
    #[serde(rename = "type", default)]
    pub kind: String,
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
    /// Host folders the guest mounts over NFS.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub share: Vec<Share>,
    /// The firmware images that seeded the stores, where known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub firmware: Option<model::FirmwareRecord>,
    /// Settings applied once the PROM has initialised the machine.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub firstboot: Option<model::FirstBoot>,
    /// Which engine runs the machine, and the options only it reads.
    pub engine: model::EngineChoice,
}

impl MachineFile {
    /// Read `text` as a machine.toml of the current format, or of format 4,
    /// whose `[qemu]` table becomes `[engine]`. Older formats need an
    /// upgrade, which this refuses with the same words `read_machine` uses.
    pub fn parse(text: &str) -> Result<Self> {
        match migrate::format(text) {
            Some(found) if found > i64::from(MACHINE_FORMAT) => {
                Err(Box::new(TooNew { found, path: None }))
            }
            Some(found) if found == i64::from(FORMAT_WITH_QEMU_TABLE) => {
                let legacy: LegacyFile = toml::from_str(text)?;
                Ok(legacy.into())
            }
            _ => Ok(toml::from_str(text)?),
        }
    }
}

/// A format 4 machine.toml, which kept QEMU's options in `[qemu]`.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LegacyFile {
    format: u32,
    machine: Machine,
    identity: Option<Identity>,
    #[serde(default)]
    network: Network,
    #[serde(default)]
    pipe: Vec<graphics::Pipe>,
    #[serde(default)]
    drive: Vec<Drive>,
    #[serde(default)]
    share: Vec<Share>,
    #[serde(default)]
    qemu: LegacyQemu,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyQemu {
    #[serde(default, rename = "extra-args")]
    extra_args: Vec<String>,
}

impl From<LegacyFile> for MachineFile {
    fn from(old: LegacyFile) -> Self {
        debug_assert_eq!(old.format, FORMAT_WITH_QEMU_TABLE);
        Self {
            format: MACHINE_FORMAT,
            machine: old.machine,
            identity: old.identity,
            network: old.network,
            pipe: old.pipe,
            drive: old.drive,
            share: old.share,
            firmware: None,
            firstboot: None,
            engine: model::EngineChoice::qemu(&old.qemu.extra_args),
        }
    }
}

/// Identity QEMU builds into the machine's own records. An SN1 machine's L1
/// serves `mac` to its firmware and gives it to the IOC3.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    pub mac: String,
    /// The `emu-id` QEMU builds the machine's serials and station address
    /// from, 1 to 4095. A machine made before emu-ids has none.
    #[serde(default, rename = "emu-id", skip_serializing_if = "Option::is_none")]
    pub emu_id: Option<u32>,
}

impl Identity {
    /// An identity with `mac`, lowercased as machine files keep it.
    pub fn new(mac: &str, emu_id: Option<u32>) -> Self {
        Self {
            mac: mac.to_ascii_lowercase(),
            emu_id,
        }
    }
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

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
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
    /// The address the shares are served at on a tap or vmnet network,
    /// with the network's prefix length, such as `192.168.1.50/24`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub share_address: Option<String>,
    /// The `A.B.C.0/24` that user, private and install networking use;
    /// `10.98.0.0/24` when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subnet: Option<String>,
}

/// A host folder the guest mounts as `ADDRESS:/name`.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Share {
    pub name: String,
    pub path: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
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
            share_address: None,
            subnet: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
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

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
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
    /// Keep a CHD disk's guest writes in its `.diff.chd` file instead of
    /// folding them into the image when the machine stops.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub overlay: bool,
}

/// `arg` quoted so a shell reads it back as one word.
pub fn shell_quote(arg: &str) -> String {
    let plain = !arg.is_empty()
        && arg
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_@%+=:,./-".contains(c));
    if plain {
        arg.into()
    } else if cfg!(windows) {
        format!("\"{}\"", arg.replace('"', "\\\""))
    } else {
        format!("'{}'", arg.replace('\'', "'\\''"))
    }
}

/// Whether `path` names a MAME CHD image.
pub fn is_chd(path: &Path) -> bool {
    path.extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("chd"))
}

/// Refuse a file that is not a version 5 CHD hard disk with 512-byte
/// sectors, the only kind QEMU's chd driver opens. The check reads the
/// header only: the magic, the version and the unit size.
pub fn check_chd(path: &Path) -> Result<()> {
    use std::io::Read;
    let mut header = [0u8; 64];
    let read = fs::File::open(path)
        .and_then(|mut file| file.read_exact(&mut header))
        .map_err(|error| match error.kind() {
            std::io::ErrorKind::UnexpectedEof => {
                format!("{} is not a CHD image", path.display())
            }
            _ => format!("{}: {error}", path.display()),
        });
    read?;
    if &header[..8] != b"MComprHD" {
        return Err(format!("{} is not a CHD image", path.display()).into());
    }
    let version = u32::from_be_bytes(header[12..16].try_into()?);
    if version != 5 {
        return Err(format!(
            "{} is CHD version {version}; only version 5 is supported",
            path.display()
        )
        .into());
    }
    let unit = u32::from_be_bytes(header[60..64].try_into()?);
    if unit != 512 {
        return Err(format!(
            "{} has {unit}-byte units, not 512-byte sectors; only hard disk CHDs are supported",
            path.display()
        )
        .into());
    }
    Ok(())
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

/// Keep an earlier format's machine.toml as `machine.toml.format<N>` before
/// a newer one replaces it, so a release that reads only N still has it.
/// Returns the copy's path, or None when the file is current or absent.
pub fn keep_older_format(dir: &Path) -> Result<Option<PathBuf>> {
    let path = dir.join("machine.toml");
    let Ok(text) = fs::read_to_string(&path) else {
        return Ok(None);
    };
    let Some(old) = migrate::format(&text).filter(|f| (1..i64::from(MACHINE_FORMAT)).contains(f))
    else {
        return Ok(None);
    };
    let copy = dir.join(format!("machine.toml.format{old}"));
    fs::write(&copy, text)?;
    Ok(Some(copy))
}

/// Replace the machine.toml in `dir` with `file`. The new text is written
/// beside it first and moved into place last, and an earlier format's file
/// is kept (see [`keep_older_format`]). On failure the old file stays and
/// nothing else is left.
pub fn write_machine(dir: &Path, file: &MachineFile) -> Result<()> {
    let temporary = dir.join("machine.toml.upgrade");
    let mut kept = None;
    let result = (|| -> Result<()> {
        fs::write(&temporary, toml::to_string_pretty(file)?)?;
        kept = keep_older_format(dir)?;
        fs::rename(&temporary, dir.join("machine.toml"))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
        if let Some(copy) = kept {
            let _ = fs::remove_file(copy);
        }
    }
    result
}

pub fn read_machine(dir: &Path) -> Result<MachineFile> {
    let path = dir.join("machine.toml");
    let text = fs::read_to_string(&path).map_err(|error| {
        format!(
            "cannot read {}: {error}. Use a machine directory created with origami create",
            path.display()
        )
    })?;
    if let Some(found) = migrate::format(&text).filter(|f| *f > i64::from(MACHINE_FORMAT)) {
        return Err(Box::new(TooNew {
            found,
            path: Some(path),
        }));
    }
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
    // A machine made against the version 2 machine list stored a node board,
    // which version 3 folds into the topology name.
    let stored_board = toml::from_str::<toml::Table>(&text)
        .ok()
        .and_then(|table| table.get("machine")?.get("board").cloned());
    if stored_board.is_some() {
        return Err(format!(
            "{} stores a node board, which the machine list no longer takes apart from the \
             topology; upgrade it once with origami upgrade {}",
            path.display(),
            dir.display()
        )
        .into());
    }
    // An unreleased build kept the emu-id as `[identity] serial`, which is
    // QEMU's text serial now.
    let numbered_serial = toml::from_str::<toml::Table>(&text)
        .ok()
        .and_then(|table| table.get("identity")?.get("serial")?.as_integer());
    if numbered_serial.is_some() {
        return Err(format!(
            "{} has the emu-id as [identity] serial, as an unreleased build wrote it; upgrade \
             it once with origami upgrade {}",
            path.display(),
            dir.display()
        )
        .into());
    }
    MachineFile::parse(&text).map_err(|error| {
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
pub fn set_machine_mac(
    machines: &Machines,
    dir: &Path,
    file: &mut MachineFile,
    mac: Option<&str>,
) -> Result<()> {
    let Some(mac) = mac else { return Ok(()) };
    if !valid_mac(mac) {
        return Err("--mac must contain six hexadecimal bytes".into());
    }
    if let Ok(offer) = offering(machines, &file.machine) {
        check_mac(offer, mac)?;
    }
    let mac = mac.to_ascii_lowercase();
    match &file.identity {
        Some(identity) if !identity.mac.eq_ignore_ascii_case(&mac) => Err(format!(
            "this machine's MAC is {}; omit --mac or pass that address",
            identity.mac
        )
        .into()),
        Some(_) => Ok(()),
        None => {
            mac::check_free(machines, dir, offering(machines, &file.machine).ok(), &mac)?;
            file.identity = Some(Identity { mac, emu_id: None });
            Ok(())
        }
    }
}

/// Like `set_machine_mac`, for a change that needs the machine to have a MAC:
/// a machine with no identity gets an emu-id, as `create` would give it, and
/// the MAC that emu-id derives, unless one is asked for. Returns the address
/// it generated.
pub fn ensure_machine_mac(
    machines: &Machines,
    dir: &Path,
    file: &mut MachineFile,
    mac: Option<&str>,
) -> Result<Option<String>> {
    let offer = offering(machines, &file.machine)?;
    if file.identity.is_some() {
        set_machine_mac(machines, dir, file, mac)?;
        return Ok(None);
    }
    let emu_id = emu_id::assign(machines, dir, offer, None, mac.is_none())?;
    if mac.is_some() {
        set_machine_mac(machines, dir, file, mac)?;
        if let Some(identity) = file.identity.as_mut() {
            identity.emu_id = Some(emu_id);
        }
        return Ok(None);
    }
    let generated = mac::assign(machines, dir, offer, None, Some(emu_id))?;
    if let Some(mac) = &generated {
        file.identity = Some(Identity::new(mac, Some(emu_id)));
    }
    Ok(generated)
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
    let found = machines.offerings.iter().find(|o| {
        o.product == machine.model
            && o.nodes == machine.nodes
            && machine.topology.as_ref() == Some(&o.topology)
            && o.cpus_per_node == profiles::population(machine)
            && match &machine.processor {
                Some(id) => &o.processor.id == id,
                None => profiles::fits_default_processor(machines, o),
            }
    });
    if let Some(offering) = found {
        return Ok(offering);
    }
    // A topology this QEMU no longer lists was renamed or removed.
    if !machines.offerings.iter().any(|o| o.topology == topology) {
        return Err(migrate::unlisted_topology(topology).into());
    }
    Err(match &machine.processor {
        Some(id) => format!(
            "{topology} does not offer {id} with {:?} CPUs per node",
            profiles::population(machine)
        ),
        None => "unsupported machine and processor population".into(),
    }
    .into())
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
pub fn validate_network(file: &MachineFile) -> Result<()> {
    let mode = file.network.mode.as_str();
    addressing::Addresses::of(file)?;
    match mode {
        "none" | "user" if file.network.endpoint.is_none() && file.network.interface.is_none() => {
            return Ok(())
        }
        // Without an endpoint, `run` hosts the network itself.
        "private" => {
            if let Some(endpoint) = file.network.endpoint.as_deref() {
                if endpoint.is_empty() {
                    return Err("private network endpoint cannot be empty".into());
                }
                let tcp = tcp_endpoint(endpoint)?;
                if cfg!(windows) && tcp.is_none() {
                    return Err("Windows private network requires a loopback TCP endpoint".into());
                }
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

/// Check `file` against the rules every engine shares. The engine's own
/// checks, such as its extra arguments, are its adapter's to run.
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
    settings::validate(machines, offering, &file.machine.inputs)?;
    serial::console_line(offering, file.machine.console.as_deref())?;
    validate_network(file)?;
    shares::validate(file)?;
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
    stores::check_state(dir, offering)?;
    if let Some(identity) = &file.identity {
        if !valid_mac(&identity.mac) {
            return Err("identity MAC must contain six hexadecimal bytes".into());
        }
        // 0 is "not configured", valid in a hand-edited file; the CLI never
        // writes it.
        if identity.emu_id.is_some_and(|emu_id| emu_id > emu_id::MAX) {
            return Err(format!(
                "identity emu-id must be from 0 to {}. Change [identity] emu-id in {}",
                emu_id::MAX,
                dir.join("machine.toml").display()
            )
            .into());
        }
        // An older machine may hold an address its PROM refuses. Say so,
        // rather than change the identity an installed guest knows.
        check_mac(offering, &identity.mac).map_err(|error| {
            format!(
                "{error}. Change [identity] mac in {}",
                dir.join("machine.toml").display()
            )
        })?;
    }
    let mut occupied = std::collections::HashSet::new();
    for drive in &file.drive {
        let bus = offering.scsi_bus_name(drive.bus);
        if !offering
            .scsi_bus(drive.bus)
            .is_some_and(|s| s.targets.contains(&drive.target))
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
        let image = resolve(dir, &drive.image);
        if !image.is_file() {
            return Err(format!("missing drive image {}", drive.image).into());
        }
        if is_chd(&image) {
            if drive.kind != "disk" {
                return Err(format!(
                    "{} is a CHD image, which attaches as a disk only; CD-ROM and tape CHDs are not supported",
                    drive.image
                )
                .into());
            }
            check_chd(&image)?;
        } else if drive.overlay {
            return Err(format!("overlay applies to CHD images, not {}", drive.image).into());
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

pub fn validate_create_inputs(
    dir: &Path,
    offering: &Offering,
    memory_per_node: Option<u32>,
    mac: Option<&str>,
) -> Result<()> {
    check_memory(
        offering,
        memory_per_node.unwrap_or(profiles::default_memory(offering)),
    )?;
    match fs::symlink_metadata(dir) {
        Ok(_) => return Err(format!("destination already exists: {}", dir.display()).into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    if mac.is_some_and(|mac| !valid_mac(mac)) {
        return Err("--mac must contain six hexadecimal bytes".into());
    }
    if let Some(mac) = mac {
        check_mac(offering, mac)?;
    }
    Ok(())
}

/// What creates a new machine's storage: the engine that makes the files
/// and the original firmware images it reads.
pub struct MachineInit<'a> {
    pub stores: &'a dyn stores::StorePreparer,
    pub boot_prom: &'a Path,
    pub io_prom: Option<&'a Path>,
}

pub fn create_configured(
    dir: &Path,
    offering: &Offering,
    init: &MachineInit<'_>,
    memory_per_node: Option<u32>,
    identity: Option<Identity>,
    pipes: Vec<graphics::Pipe>,
    inputs: std::collections::BTreeMap<String, String>,
) -> Result<()> {
    let memory_per_node = memory_per_node.unwrap_or(profiles::default_memory(offering));
    let padded = init.stores.stage_boot_prom(offering, init.boot_prom)?;
    let boot_prom = padded.as_ref().map_or(init.boot_prom, |file| file.as_ref());
    if let Some(parent) = dir.parent().filter(|parent| !parent.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    fs::create_dir(dir)?;
    let result = (|| -> Result<()> {
        for name in ["drives", "logs"] {
            fs::create_dir(dir.join(name))?;
        }
        init.stores
            .create_stores(offering, boot_prom, init.io_prom, &dir.join("state"))?;
        let mut machine =
            model::Machine::from_offering(offering, Some(memory_per_node), identity, pipes, inputs);
        machine.firmware = model::FirmwareRecord::of(init.boot_prom, init.io_prom);
        let file = machine.to_file();
        fs::write(dir.join("machine.toml"), toml::to_string_pretty(&file)?)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(dir);
    }
    result
}

/// A hand-trimmed excerpt of a `query-sgi-machines` reply: one offering of
/// each kind the unit tests exercise. The neutral types read what they share
/// with the reply directly; the QEMU adapter's translation also fills in the
/// console names and the processor choice, which this does the short way.
/// The product build's product-state test reads the real machine list.
#[cfg(test)]
pub(crate) fn test_machines() -> Machines {
    let wire: serde_json::Value =
        serde_json::from_str(include_str!("../../../tests/fixtures/sgi-machines.json")).unwrap();
    let mut machines: Machines = serde_json::from_value(wire.clone()).unwrap();
    for (offering, record) in machines
        .offerings
        .iter_mut()
        .zip(wire["offerings"].as_array().unwrap())
    {
        offering.selects_processor = record["machine-options"]
            .as_str()
            .unwrap()
            .split(',')
            .any(|option| option.starts_with("processor="));
        for (console, line) in offering
            .consoles
            .iter_mut()
            .zip(record["consoles"].as_array().unwrap())
        {
            console.name = line["argument"]
                .as_str()
                .and_then(|argument| argument.strip_prefix("-serial chardev:"))
                .map(str::to_string);
        }
    }
    machines
}

/// The stand-in stores the unit tests use.
#[cfg(test)]
pub(crate) mod test_support;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engine_section_round_trips_and_private_is_omitted_when_empty() {
        let base = "format = 5\n[machine]\nmodel = \"origin200\"\nnodes = 1\ncpus_per_node = 1\nmemory_per_node = \"256MiB\"\n[engine]\nid = \"qemu\"\n";
        let plain = MachineFile::parse(base).unwrap();
        assert!(plain.engine.extra_args().is_empty());
        let text = toml::to_string_pretty(&plain).unwrap();
        assert!(text.contains("[engine]\nid = \"qemu\"\n"), "{text}");
        assert!(!text.contains("private"), "{text}");

        let with = format!(
            "{base}[engine.private]\nextra-args = [\"-global\", \"bcm570x.mac=08:00:69:12:34:56\"]\n"
        );
        let file = MachineFile::parse(&with).unwrap();
        assert_eq!(
            file.engine.extra_args(),
            ["-global", "bcm570x.mac=08:00:69:12:34:56"]
        );
        let again = MachineFile::parse(&toml::to_string_pretty(&file).unwrap()).unwrap();
        assert_eq!(again.engine, file.engine);
    }

    #[test]
    fn a_format_4_file_reads_with_its_qemu_table_as_the_engine() {
        let old = "format = 4\n[machine]\nmodel = \"origin200\"\nnodes = 1\ncpus_per_node = 1\nmemory_per_node = \"256MiB\"\n[qemu]\nextra-args = [\"-d\", \"guest_errors\"]\n";
        let file = MachineFile::parse(old).unwrap();
        assert_eq!(file.format, MACHINE_FORMAT);
        assert_eq!(file.engine.id, "qemu");
        assert_eq!(file.engine.extra_args(), ["-d", "guest_errors"]);
        let text = toml::to_string_pretty(&file).unwrap();
        assert!(text.starts_with("format = 5\n"), "{text}");
        assert!(!text.contains("[qemu]"), "{text}");
    }

    /// Each format 4 fixture reads as the format 5 fixture beside it, and a
    /// format 5 file writes back byte for byte.
    #[test]
    fn machine_fixtures_convert_and_round_trip() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/machines");
        let update = std::env::var_os("ORIGAMI_UPDATE_SNAPSHOT").is_some();
        for name in ["origin200", "octane2-everything"] {
            let old = fs::read_to_string(dir.join(format!("format4-{name}.toml"))).unwrap();
            let new = toml::to_string_pretty(&MachineFile::parse(&old).unwrap()).unwrap();
            let expected = dir.join(format!("format5-{name}.toml"));
            if update {
                fs::write(&expected, &new).unwrap();
            }
            assert_eq!(new, fs::read_to_string(expected).unwrap(), "{name}");
        }
        for name in ["origin200", "octane2-everything", "firmware"] {
            let text = fs::read_to_string(dir.join(format!("format5-{name}.toml"))).unwrap();
            let file = MachineFile::parse(&text).unwrap();
            assert_eq!(toml::to_string_pretty(&file).unwrap(), text, "{name}");
        }
    }

    #[test]
    fn a_newer_format_is_too_new_to_read() {
        let error = MachineFile::parse("format = 6\n[future]\nx = 1\n").unwrap_err();
        let too_new = error.downcast_ref::<TooNew>().unwrap();
        assert_eq!(too_new.found, 6);
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("machine.toml"), "format = 99\n").unwrap();
        let error = read_machine(dir.path()).unwrap_err();
        assert!(error.downcast_ref::<TooNew>().is_some(), "{error}");
        assert!(error.to_string().contains("use a newer Origami"), "{error}");
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
    fn drives_name_their_bus_as_the_machine_list_does() {
        let machines = test_machines();
        let by_id = |id: &str| machines.offerings.iter().find(|o| o.id == id).unwrap();
        // The O2's onboard controllers are aic0.0 and aic1.0, not scsi.N.
        let o2 = by_id("o2.1");
        assert_eq!(o2.scsi_bus_name(0), "aic0.0");
        assert_eq!(o2.scsi_bus_name(1), "aic1.0");
        assert!(o2.scsi_bus(2).is_none());
        assert_eq!(o2.scsi_bus_name(2), "scsi.2");
        // Every other machine's buses are scsi.0, scsi.1 and so on.
        let octane = by_id("octane.1");
        assert_eq!(octane.scsi_bus_name(1), "scsi.1");
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
        let stores = test_support::ZeroStores { fail: false };
        let init = MachineInit {
            stores: &stores,
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
        // Without a size an Origin 200 gets the preset's 256 MiB.
        let default_size = root.join("default-size");
        create_configured(
            &default_size,
            offer,
            &init,
            None,
            None,
            vec![],
            Default::default(),
        )
        .unwrap();
        let file = read_machine(&default_size).unwrap();
        assert_eq!(file.machine.memory_per_node, "256MiB");
        let failing = test_support::ZeroStores { fail: true };
        let broken = MachineInit {
            stores: &failing,
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

    #[test]
    fn the_machine_list_gives_the_mac_prefix() {
        let machines = test_machines();
        let mut offer = preset(&machines, "onyx350").unwrap().clone();
        assert_eq!(offer.mac_prefix(), "08:00:69");
        // A required prefix wins over the default address's.
        let mac = offer.networks[0].mac.as_mut().unwrap();
        mac.required_prefix = Some("08:00:6a".into());
        assert_eq!(offer.mac_prefix(), "08:00:6a");
        assert!(check_mac(&offer, "08:00:69:00:00:01").is_err());
        check_mac(&offer, "08:00:6A:00:00:01").unwrap();
        // NICs that take their own addresses, as on the Origin 300 NUMAlink
        // topologies, still need SGI's prefix.
        let mac = offer.networks[0].mac.as_mut().unwrap();
        *mac = AdapterMac {
            derived: false,
            offset: 0,
            default_address: None,
            required_prefix: None,
        };
        assert_eq!(offer.mac_prefix(), "08:00:69");
        assert!(check_mac(&offer, "52:54:00:12:34:56").is_err());
        check_mac(&offer, "08:00:69:00:00:01").unwrap();
    }

    #[test]
    fn every_machine_refuses_a_non_sgi_address() {
        let machines = test_machines();
        let absent = Path::new("/nonexistent/origami-mac-prefix");
        let onyx = preset(&machines, "onyx350").unwrap();
        let origin = preset(&machines, "origin200-1").unwrap();
        for (offer, name) in [(onyx, "Onyx 350"), (origin, "Origin 200")] {
            let error = validate_create_inputs(absent, offer, None, Some("52:54:00:12:34:56"))
                .unwrap_err()
                .to_string();
            assert_eq!(
                error,
                format!(
                    "{name} needs an Ethernet address starting 08:00:69, \
                     not 52:54:00:12:34:56; its PROM refuses other addresses"
                )
            );
            validate_create_inputs(absent, offer, None, Some("08:00:69:AB:CD:EF")).unwrap();
        }

        let mut file = MachineFile {
            format: MACHINE_FORMAT,
            machine: Machine {
                topology: Some(onyx.topology.clone()),
                population: onyx.cpus_per_node.clone(),
                inputs: Default::default(),
                model: onyx.product.clone(),
                nodes: onyx.nodes,
                cpus_per_node: onyx.cpus_per_node[0],
                processor: None,
                memory_per_node: format!("{}MiB", onyx.memory.default),
                console: None,
            },
            identity: None,
            network: Network::default(),
            pipe: vec![],
            drive: vec![],
            share: vec![],
            firmware: None,
            firstboot: None,
            engine: model::EngineChoice::qemu(&[]),
        };
        let error = set_machine_mac(
            &machines,
            Path::new("/nonexistent/machine"),
            &mut file,
            Some("52:54:00:12:34:56"),
        )
        .unwrap_err()
        .to_string();
        assert!(
            error.contains("needs an Ethernet address starting 08:00:69"),
            "{error}"
        );
        assert!(file.identity.is_none());
        set_machine_mac(
            &machines,
            Path::new("/nonexistent/machine"),
            &mut file,
            Some("08:00:69:12:34:56"),
        )
        .unwrap();
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
        let stores = test_support::ZeroStores { fail: false };
        let prom = root.join("prom.img");
        let init = MachineInit {
            stores: &stores,
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
            Some(Identity::new("08:00:69:aa:bb:cc", None)),
            vec![],
            Default::default(),
        )
        .unwrap();
        let mut file = read_machine(&machine).unwrap();
        assert_eq!(file.identity.as_ref().unwrap().mac, "08:00:69:aa:bb:cc");
        file.network = Network {
            mode: "private".into(),
            endpoint: Some("tcp:127.0.0.1:4242".into()),
            interface: None,
            forward: vec![],
            share_address: None,
            subnet: None,
        };
        validate(&machines, &machine, &file).unwrap();
        let error = set_machine_mac(
            &machines,
            Path::new("/nonexistent/machine"),
            &mut file,
            Some("08:00:69:12:34:56"),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("08:00:69:aa:bb:cc"), "{error}");
        set_machine_mac(
            &machines,
            Path::new("/nonexistent/machine"),
            &mut file,
            Some("08:00:69:AA:BB:CC"),
        )
        .unwrap();
        file.identity = None;
        let error = validate(&machines, &machine, &file)
            .unwrap_err()
            .to_string();
        assert!(error.contains("--mac"), "{error}");
        set_machine_mac(
            &machines,
            Path::new("/nonexistent/machine"),
            &mut file,
            Some("08:00:69:12:34:56"),
        )
        .unwrap();
        validate(&machines, &machine, &file).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_machine_without_a_mac_is_assigned_one_where_networking_needs_it() {
        let root = tempfile::tempdir().unwrap();
        let machines = test_machines();
        let offer = preset(&machines, "origin200-1").unwrap();
        let stores = test_support::ZeroStores { fail: false };
        let prom = root.path().join("prom.img");
        let init = MachineInit {
            stores: &stores,
            boot_prom: &prom,
            io_prom: None,
        };
        let machine = root.path().join("old");
        create_configured(
            &machine,
            offer,
            &init,
            None,
            None,
            vec![],
            Default::default(),
        )
        .unwrap();
        let other = root.path().join("other");
        create_configured(
            &other,
            offer,
            &init,
            None,
            Some(Identity::new("08:00:69:01:02:03", None)),
            vec![],
            Default::default(),
        )
        .unwrap();
        let mut file = read_machine(&machine).unwrap();
        file.network = Network {
            mode: "private".into(),
            endpoint: Some("tcp:127.0.0.1:4242".into()),
            interface: None,
            forward: vec![],
            share_address: None,
            subnet: None,
        };
        let mac = ensure_machine_mac(&machines, &machine, &mut file, None)
            .unwrap()
            .unwrap();
        // No sibling holds an emu-id, so it is emu-id 1 and its derived MAC.
        assert_eq!(mac, "08:00:69:e1:00:10");
        assert_eq!(file.identity.as_ref().unwrap().emu_id, Some(1));
        assert_eq!(file.identity.as_ref().unwrap().mac, mac);
        validate(&machines, &machine, &file).unwrap();
        // It keeps that address, and an explicit duplicate is still refused.
        assert_eq!(
            ensure_machine_mac(&machines, &machine, &mut file, None).unwrap(),
            None
        );
        file.identity = None;
        assert!(
            ensure_machine_mac(&machines, &machine, &mut file, Some("08:00:69:01:02:03")).is_err()
        );
    }
}
