//! What a settings pane needs to show and change a machine's network, in
//! the words every app uses.

use crate::{NetworkChange, NetworkMode};
use origami_machine::{addressing, emu_id, mac, MachineFile, Offering};
use std::path::PathBuf;

/// A machine's network as its file has it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NetworkSetting {
    /// `user`, `private`, `none`, `tap` or `vmnet`.
    pub mode: String,
    pub endpoint: Option<String>,
    pub interface: Option<String>,
    pub share_address: Option<String>,
    /// The /24 that Shared and Private use: the file's, else the default.
    pub subnet: String,
    pub mac: Option<String>,
    /// The tap device or socket_vmnet socket, as its field shows it; empty when none.
    pub device: String,
    /// Where the Ethernet address came from.
    pub mac_note: String,
    /// Findings for a bridged machine, which shares its network with other computers.
    pub warnings: Vec<String>,
}

impl NetworkSetting {
    pub(crate) fn read(file: &MachineFile, offer: &Offering) -> Self {
        let network = &file.network;
        let id = file
            .identity
            .as_ref()
            .and_then(|i| i.emu_id)
            .filter(|id| *id != 0);
        let mac = file.identity.as_ref().map(|i| i.mac.clone());
        let made = id.is_some_and(|id| mac::from_emu_id(offer, id).ok() == mac);
        let mut warnings = vec![];
        if matches!(network.mode.as_str(), "tap" | "vmnet") {
            warnings.push(
                "Origami can only check machines in this library. A computer on your network \
                 that uses the same Ethernet address will clash with this machine."
                    .into(),
            );
            if id.is_none() {
                warnings.push(
                    "A machine with no ID may use an address that another machine on your \
                     network has."
                        .into(),
                );
            }
        }
        Self {
            mode: network.mode.clone(),
            endpoint: network.endpoint.clone(),
            interface: network.interface.clone(),
            share_address: network.share_address.clone(),
            subnet: addressing::Addresses::of(file)
                .map(|a| a.cidr())
                .unwrap_or_default(),
            device: match network.mode.as_str() {
                "vmnet" => network.endpoint.clone(),
                _ => network.interface.clone(),
            }
            .unwrap_or_default(),
            mac_note: match (&mac, made, id) {
                (None, ..) => "None yet. Private and Bridged give the machine one.".into(),
                (_, true, Some(id)) => format!("Made from this machine's ID, {id}."),
                _ => "Set when the machine was made.".into(),
            },
            mac,
            warnings,
        }
    }
}

/// The change that moves the machine in `file` to `mode`. `device` is what
/// the Bridged field holds: the tap device, or the socket_vmnet socket, which
/// defaults to the usual one. A change replaces the whole network, so only
/// what the new mode uses is kept.
pub(crate) fn change(
    file: &MachineFile,
    mode: NetworkMode,
    device: &str,
    share_address: Option<&str>,
    subnet: Option<&str>,
) -> Result<NetworkChange, String> {
    let host = matches!(mode, NetworkMode::Tap | NetworkMode::Vmnet);
    let old = &file.network;
    let device = Some(device.trim()).filter(|d| !d.is_empty());
    let setup = bridged_setup();
    let (mut endpoint, mut interface) = (None, None);
    match mode {
        NetworkMode::Private if old.mode == "private" => endpoint = old.endpoint.clone(),
        NetworkMode::Tap => {
            let device = device.ok_or_else(|| {
                format!("Enter the {} to use Bridged.", setup.field.to_lowercase())
            })?;
            interface = Some(device.to_string());
        }
        NetworkMode::Vmnet => endpoint = Some(device.unwrap_or(&setup.example).to_string()),
        _ => {}
    }
    Ok(NetworkChange {
        mode,
        endpoint,
        interface,
        share_address: share_address
            .map(str::trim)
            .filter(|a| host && !a.is_empty())
            .map(Into::into),
        subnet: subnet.map(str::trim).map(Into::into),
        mac: None,
    })
}

/// One way to connect a machine that this computer offers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NetworkChoice {
    pub mode: NetworkMode,
    pub label: String,
    pub detail: String,
}

/// The modes this host can run, in the order a picker lists them: tap on
/// Linux and Windows, socket_vmnet on macOS.
pub fn host_network_modes() -> Vec<NetworkChoice> {
    let bridged = if cfg!(target_os = "macos") {
        NetworkMode::Vmnet
    } else {
        NetworkMode::Tap
    };
    [
        (
            NetworkMode::User,
            "Shared (NAT)",
            "Internet through this computer.",
        ),
        (
            NetworkMode::Private,
            "Private",
            "Only Origami's own machines.",
        ),
        (bridged, "Bridged", "Its own address on your network."),
        (NetworkMode::None, "Off", "No network."),
    ]
    .map(|(mode, label, detail)| NetworkChoice {
        mode,
        label: label.into(),
        detail: detail.into(),
    })
    .into()
}

/// The first socket_vmnet socket in Homebrew's usual places.
fn vmnet_endpoint() -> Option<PathBuf> {
    [
        "/opt/homebrew/var/run/socket_vmnet",
        "/usr/local/var/run/socket_vmnet",
    ]
    .into_iter()
    .map(PathBuf::from)
    .find(|p| p.exists())
}

/// How to set up the host side of Bridged on this computer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BridgedSetup {
    /// The label of the one field Bridged needs: the device, adapter or socket.
    pub field: String,
    /// What fills it, as an example, or the socket found on this computer.
    pub example: String,
    pub steps: String,
    /// The commands in `steps`, for a Copy button.
    pub commands: Option<String>,
}

pub fn bridged_setup() -> BridgedSetup {
    let (field, example, steps, commands) = if cfg!(target_os = "macos") {
        let commands = "brew install socket_vmnet && sudo brew services start socket_vmnet";
        (
            "socket_vmnet socket",
            vmnet_endpoint().map_or_else(|| "/opt/homebrew/var/run/socket_vmnet".into(), |p| p.display().to_string()),
            format!("Install and start socket_vmnet, once:\n{commands}\nThe default socket gives a shared network. A socket such as socket_vmnet.bridged.en0 joins your network."),
            Some(commands),
        )
    } else if cfg!(windows) {
        (
            "TAP adapter name",
            "OpenVPN TAP-Windows6".into(),
            "Install a TAP-Windows6 adapter from OpenVPN as an administrator. Type its name from Network Connections, then use Bridge Connections there to join it to your network.".into(),
            None,
        )
    } else {
        let commands =
            "sudo ip tuntap add dev tap0 mode tap user $USER\nsudo ip link set tap0 master br0 up";
        (
            "Tap device",
            "tap0".into(),
            format!("Make a tap device you own and join it to a bridge that holds your Ethernet interface. Once, as root:\n{commands}\nEach running machine needs its own tap device."),
            Some(commands),
        )
    };
    BridgedSetup {
        field: field.into(),
        example,
        steps,
        commands: commands.map(Into::into),
    }
}

/// What New Machine shows for the machine ID and the Ethernet address.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IdentityPreview {
    pub emu_id: u32,
    /// The address the machine would get; empty when it cannot be made from an ID.
    pub mac: String,
    /// Whether the address is made from the ID, so a typed one is optional.
    pub from_id: bool,
    /// Why the ID or address cannot be used.
    pub problem: Option<String>,
    /// Which field the problem belongs to, so it shows on that row.
    pub problem_field: Option<IdentityField>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdentityField {
    Id,
    Address,
}

pub(crate) fn not_an_id() -> String {
    format!("Choose a number from 1 to {}.", emu_id::LAST_MINTED)
}

/// The sentence for an ID that is out of range or held by another machine
/// beside `dir`, naming that machine and no path.
fn id_problem(dir: &std::path::Path, id: u32) -> Option<String> {
    if emu_id::check_range(id).is_err() {
        return Some(not_an_id());
    }
    emu_id::taken(dir)
        .into_iter()
        .find(|(_, other)| *other == id)
        .map(|(other, _)| {
            let name = other
                .file_name()
                .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
            format!("{name} already uses ID {id}. Choose another.")
        })
}

pub(crate) fn preview(
    machines: &origami_machine::Machines,
    dir: &std::path::Path,
    offer: &Offering,
    typed_id: &str,
    typed_mac: Option<&str>,
) -> IdentityPreview {
    let typed_id = typed_id.trim();
    let typed_mac = typed_mac.map(str::trim).filter(|m| !m.is_empty());
    let mut problem = None;
    let mut field = None;
    let id = if typed_id.is_empty() {
        emu_id::assign(machines, dir, offer, None, typed_mac.is_none()).unwrap_or(0)
    } else {
        match typed_id.parse::<u32>() {
            Ok(id) => {
                problem = id_problem(dir, id);
                id
            }
            Err(_) => {
                problem = Some(not_an_id());
                0
            }
        }
    };
    if problem.is_some() {
        field = Some(IdentityField::Id);
    }
    let made = mac::from_emu_id(offer, id).ok();
    let shown = match typed_mac {
        Some(typed) => {
            if !origami_machine::valid_mac(typed) && problem.is_none() {
                problem = Some(
                    "Write the address as six pairs of hex digits, such as 08:00:69:e1:00:30."
                        .into(),
                );
                field = Some(IdentityField::Address);
            }
            typed.to_ascii_lowercase()
        }
        None => made.clone().unwrap_or_default(),
    };
    if problem.is_none() && !shown.is_empty() {
        problem = mac::check_free(machines, dir, Some(offer), &shown)
            .err()
            .map(|e| e.to_string());
        field = problem.as_ref().map(|_| IdentityField::Address);
    }
    IdentityPreview {
        emu_id: id,
        mac: shown,
        from_id: made.is_some(),
        problem,
        problem_field: field,
    }
}
