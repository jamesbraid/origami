//! What a machine's file says, as plain data for a settings pane.

use super::network::NetworkSetting;
use crate::Loaded;
use origami_machine::{serial, shares, PortForward, Result};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PipeSetting {
    pub slot: String,
    pub product: String,
    pub family: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DriveSetting {
    pub name: String,
    /// `disk`, `cdrom` or `tape`.
    pub kind: String,
    pub bus: u32,
    pub target: u32,
    pub image: String,
    pub read_only: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShareSetting {
    pub name: String,
    pub host_dir: String,
    /// What to run as root in IRIX to mount it; empty when `mount_problem` says why not.
    pub mount_lines: Vec<String>,
    pub mount_problem: Option<String>,
    /// Whether the problem is fixed by an address for shared folders in the Network settings.
    pub needs_address: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConsolePortSetting {
    pub name: String,
    /// What a picker shows, the same in every app, with the default marked.
    pub label: String,
    pub is_default: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MachineSettings {
    pub nodes: u32,
    /// The processor module and its part, such as `R12000`. Fixed at creation.
    pub processor: String,
    pub processor_part: String,
    pub cpus_per_node: u32,
    /// The CPU counts per node the machine's topology and module take.
    pub cpus_per_node_allowed: Vec<u32>,
    pub memory_per_node_mib: u32,
    pub memory_allowed_mib: Vec<u32>,
    pub pipes: Vec<PipeSetting>,
    pub drives: Vec<DriveSetting>,
    /// The first free target on the first SCSI bus, for a disk added in Settings.
    pub disk_target: Option<u32>,
    /// The last free target there, for a CD-ROM, so disks keep the low numbers.
    pub cd_target: Option<u32>,
    pub network: NetworkSetting,
    pub forwards: Vec<PortForward>,
    pub shares: Vec<ShareSetting>,
    /// The serial line in use: the file's choice, else the machine's default.
    pub console_port: String,
    pub console_ports: Vec<ConsolePortSetting>,
}

/// One row of the Drives list: its title and what is in it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DriveRow {
    /// The drive's name in the machine file, for `DetachDrive`.
    pub name: String,
    /// `System disk`, `Disk 2`, `CD drive` or `Tape drive`.
    pub title: String,
    /// The file, its size, the OS on it when known, and `changes kept apart`.
    pub caption: String,
}

/// The rows for `file`'s drives. Blocking: it opens each image for its size
/// and, when `read`, for the OS on it, which a running machine's disks must not get.
pub(crate) fn drive_rows(
    dir: &std::path::Path,
    file: &origami_machine::MachineFile,
    read: bool,
) -> Vec<DriveRow> {
    let system = super::core::system_drive(file).map(|d| d.name.clone());
    let mut disks = 1;
    file.drive
        .iter()
        .map(|d| {
            let title = match d.kind.as_str() {
                "cdrom" => "CD drive".to_string(),
                "tape" => "Tape drive".to_string(),
                _ if Some(&d.name) == system.as_ref() => "System disk".to_string(),
                _ => {
                    disks += 1;
                    format!("Disk {disks}")
                }
            };
            let image = origami_machine::resolve(dir, &d.image);
            let mut parts = vec![image
                .file_name()
                .map_or_else(|| d.image.clone(), |n| n.to_string_lossy().into_owned())];
            if let Ok(bytes) = std::fs::metadata(&image).map(|m| m.len()) {
                parts.push(super::size_label(super::disks::guest_size(&image, bytes)));
            }
            if d.kind == "disk" {
                let os = super::disks::cached_label(&image)
                    .or_else(|| read.then(|| super::disks::label(&image).ok()).flatten());
                parts.extend(os.and_then(super::disks::recognised));
            }
            if d.overlay {
                parts.push("changes kept apart".into());
            }
            DriveRow {
                name: d.name.clone(),
                title,
                caption: parts.join(", "),
            }
        })
        .collect()
}

/// What the startup line says: `Screen` or `Console`, after
/// [`UiString::StartupMessagesOn`](super::UiString::StartupMessagesOn).
pub fn startup_value(console: super::BootConsole) -> &'static str {
    match console {
        super::BootConsole::Screen => "Screen",
        super::BootConsole::Serial => "Console",
    }
}

fn port_label(name: &str) -> String {
    match name {
        "l1" => "L1 system controller",
        "ioc3_a" => "Serial port A (IOC3)",
        "ioc3_b" => "Serial port B (IOC3)",
        other => other,
    }
    .into()
}

fn free_targets(
    file: &origami_machine::MachineFile,
    offer: &origami_machine::Offering,
) -> Vec<u32> {
    let used = |t: &u32| file.drive.iter().any(|d| d.bus == 0 && d.target == *t);
    offer.scsi_bus(0).map_or_else(Vec::new, |b| {
        b.targets.iter().copied().filter(|t| !used(t)).collect()
    })
}

fn share_setting(
    file: &origami_machine::MachineFile,
    share: &origami_machine::Share,
) -> ShareSetting {
    let bridged = matches!(file.network.mode.as_str(), "tap" | "vmnet");
    let needs_address = bridged && file.network.share_address.is_none();
    let problem = |text: &str| (vec![], Some(text.to_string()));
    let (mount_lines, mount_problem) = match shares::mount_lines(file, share) {
        Ok(lines) => (lines.to_vec(), None),
        Err(_) if file.network.mode == "none" => {
            problem("Turn the network on to use shared folders.")
        }
        Err(_) if needs_address => problem("Set an address for shared folders in Network."),
        Err(error) => problem(&error.to_string()),
    };
    ShareSetting {
        name: share.name.clone(),
        host_dir: share.path.to_string_lossy().into_owned(),
        mount_lines,
        mount_problem,
        needs_address,
    }
}

/// Refuses a share name the guest cannot type, in the core's words.
pub fn check_share_name(name: &str) -> super::CoreResult<()> {
    shares::check_name(name).map_err(|error| {
        super::CoreError::new(
            super::ErrorKind::Invalid,
            "Use letters, digits, hyphens and underscores.",
        )
        .with_detail(error.to_string())
    })
}

/// The share name the guest would see for the host folder `dir`: its own
/// name with what a share name refuses turned into hyphens.
pub fn suggest_share_name(dir: &str) -> String {
    let leaf = dir
        .trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("");
    let clean: String = leaf
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '-'
            }
        })
        .collect();
    let clean: String = clean.trim_start_matches('.').chars().take(64).collect();
    if clean.is_empty() {
        "share".into()
    } else {
        clean
    }
}

pub(crate) fn read(loaded: &Loaded) -> Result<MachineSettings> {
    let (file, offer) = (&loaded.file, loaded.offering()?);
    Ok(MachineSettings {
        nodes: file.machine.nodes,
        processor: offer.processor.id.clone(),
        processor_part: offer.processor.part.clone(),
        cpus_per_node: file.machine.cpus_per_node,
        cpus_per_node_allowed: crate::change::cpu_counts(&loaded.catalogue, offer),
        memory_per_node_mib: file
            .machine
            .memory_per_node
            .trim_end_matches("MiB")
            .parse()
            .unwrap_or(0),
        memory_allowed_mib: offer.memory.accepted.clone(),
        pipes: file
            .pipe
            .iter()
            .map(|p| PipeSetting {
                slot: p.slot.clone(),
                product: p.product.clone(),
                family: p.family.clone(),
            })
            .collect(),
        drives: file
            .drive
            .iter()
            .map(|d| DriveSetting {
                name: d.name.clone(),
                kind: d.kind.clone(),
                bus: d.bus,
                target: d.target,
                image: d.image.clone(),
                read_only: d.read_only,
            })
            .collect(),
        disk_target: free_targets(file, offer).first().copied(),
        cd_target: free_targets(file, offer).last().copied(),
        network: NetworkSetting::read(file, offer),
        forwards: file.network.forward.clone(),
        shares: file.share.iter().map(|s| share_setting(file, s)).collect(),
        console_port: match &file.machine.console {
            Some(port) => port.clone(),
            None => serial::default_console(offer)?.into(),
        },
        console_ports: serial::console_choices(offer)
            .into_iter()
            .map(|p| ConsolePortSetting {
                label: match p.primary {
                    true => format!("{} (default)", port_label(&p.name)),
                    false => port_label(&p.name),
                },
                name: p.name,
                is_default: p.primary,
            })
            .collect(),
    })
}
