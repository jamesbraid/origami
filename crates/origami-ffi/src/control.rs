//! The UniFFI control plane over the facade. Names carry an `Origami` prefix
//! so they cannot collide with platform frameworks.

use crate::engines;
use crate::keys::OrigamiKey;
use origami_core::facade as f;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

macro_rules! errors {
    ($($variant:ident => $kind:ident),* $(,)?) => {
        /// What went wrong. The variant is the kind a program branches on;
        /// `message` is a sentence safe to show, `detail` names the field or
        /// path where there is one.
        #[derive(Debug, thiserror::Error, uniffi::Error)]
        pub enum CoreError {
            $(#[error("{message}")] $variant { message: String, detail: Option<String> }),*
        }

        impl From<f::CoreError> for CoreError {
            fn from(e: f::CoreError) -> Self {
                match e.kind {
                    $(f::ErrorKind::$kind => CoreError::$variant { message: e.message, detail: e.detail }),*
                }
            }
        }

        /// The kind of an error, for a failed operation.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
        pub enum OrigamiErrorKind { $($variant),* }

        impl From<f::ErrorKind> for OrigamiErrorKind {
            fn from(k: f::ErrorKind) -> Self {
                match k { $(f::ErrorKind::$kind => OrigamiErrorKind::$variant),* }
            }
        }
    };
}

errors! {
    NotFound => NotFound,
    Invalid => Invalid,
    Busy => Busy,
    NotRunning => NotRunning,
    Unsupported => Unsupported,
    NeedsUpgrade => NeedsUpgrade,
    Io => Io,
    Protocol => Protocol,
    Engine => Engine,
    Cancelled => Cancelled,
    Internal => Internal,
}

type Res<T> = Result<T, CoreError>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum OrigamiEventKind {
    State,
    Display,
    Console,
    Operation,
    Library,
}

impl From<f::EventKind> for OrigamiEventKind {
    fn from(k: f::EventKind) -> Self {
        match k {
            f::EventKind::State => Self::State,
            f::EventKind::Display => Self::Display,
            f::EventKind::Console => Self::Console,
            f::EventKind::Operation => Self::Operation,
            f::EventKind::Library => Self::Library,
        }
    }
}

/// What changed on one machine (or, with no machine, on the core). Level
/// triggered: pull the state you care about.
#[derive(Clone, Debug, uniffi::Record)]
pub struct OrigamiEvent {
    pub machine: Option<String>,
    pub kinds: Vec<OrigamiEventKind>,
}

/// Implemented by the app. Called on a thread the core owns, never on a UI
/// thread, with no core lock held: hop to the UI thread inside it, and feel
/// free to call back into the core. Do not block.
#[uniffi::export(with_foreign)]
pub trait OrigamiObserver: Send + Sync {
    fn on_event(&self, event: OrigamiEvent);
}

struct ObserverBridge(Arc<dyn OrigamiObserver>);

impl f::Observer for ObserverBridge {
    fn on_event(&self, event: f::Event) {
        self.0.on_event(OrigamiEvent {
            machine: event.machine,
            kinds: event.kinds.into_iter().map(Into::into).collect(),
        });
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum OrigamiMachineState {
    NotRunning,
    Starting,
    Running,
    Paused,
    Stopping,
    Exited,
    LeftRunning,
}

impl From<f::MachineState> for OrigamiMachineState {
    fn from(s: f::MachineState) -> Self {
        match s {
            f::MachineState::NotRunning => Self::NotRunning,
            f::MachineState::Starting => Self::Starting,
            f::MachineState::Running => Self::Running,
            f::MachineState::Paused => Self::Paused,
            f::MachineState::Stopping => Self::Stopping,
            f::MachineState::Exited => Self::Exited,
            f::MachineState::LeftRunning => Self::LeftRunning,
        }
    }
}

impl From<OrigamiMachineState> for f::MachineState {
    fn from(s: OrigamiMachineState) -> Self {
        match s {
            OrigamiMachineState::NotRunning => Self::NotRunning,
            OrigamiMachineState::Starting => Self::Starting,
            OrigamiMachineState::Running => Self::Running,
            OrigamiMachineState::Paused => Self::Paused,
            OrigamiMachineState::Stopping => Self::Stopping,
            OrigamiMachineState::Exited => Self::Exited,
            OrigamiMachineState::LeftRunning => Self::LeftRunning,
        }
    }
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct OrigamiEngineInfo {
    pub id: String,
    /// What a person calls the engine, such as `QEMU`.
    pub name: String,
    pub version: Option<String>,
    pub max_heads: u32,
    pub gpu_regions: bool,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct OrigamiOffering {
    pub id: String,
    /// What the New Machine picker shows, such as `Fuel, 1 CPU`.
    pub label: String,
    pub engine: String,
    pub product: String,
    pub topology: String,
    /// What a person calls the topology, such as `Origin 200`.
    pub topology_name: String,
    /// Whether a machine made with the defaults has a graphics head.
    pub has_display: bool,
    /// Whether the machine runs IRIX, so Install IRIX applies.
    pub runs_irix: bool,
    pub nodes: u32,
    pub cpus: u32,
    pub memory_default_mib: u32,
    pub memory_accepted_mib: Vec<u32>,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct OrigamiSummaryCard {
    pub title: String,
    pub value: String,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct OrigamiMachineSummary {
    pub key: String,
    pub name: String,
    pub product: String,
    pub product_label: String,
    pub topology: Option<String>,
    pub topology_name: Option<String>,
    /// The tiles under the machine's name: memory, nodes where several, model.
    pub cards: Vec<OrigamiSummaryCard>,
    /// Whether the machine has a graphics head, from its file.
    pub has_display: bool,
    /// Whether the machine runs IRIX, so Install IRIX applies.
    pub runs_irix: bool,
    pub nodes: u32,
    pub memory_per_node_mib: u32,
    pub state: OrigamiMachineState,
    pub problem: Option<String>,
    /// Made by an older Origami: `upgrade_machine` first, then it opens.
    pub needs_upgrade: bool,
    /// What the system disk holds, such as `IRIX 6.5.30`, once read; a first
    /// listing starts the read and a library event follows.
    pub system_disk: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum OrigamiLineKind {
    Result,
    Detail,
    Advice,
    Warning,
    Note,
    /// A change the guest sees at its next boot.
    GuestNote,
}

/// One line of what an operation had to say.
#[derive(Clone, Debug, uniffi::Record)]
pub struct OrigamiReportLine {
    pub kind: OrigamiLineKind,
    pub text: String,
    /// What to tell the person, or none for the lines kept for the log.
    pub shown: Option<String>,
    /// A command the line tells the person to run, for a Copy button.
    pub copy: Option<String>,
}

fn report_lines(report: origami_core::Report) -> Vec<OrigamiReportLine> {
    report
        .lines
        .into_iter()
        .map(|l| OrigamiReportLine {
            kind: match l.kind {
                origami_core::Kind::Result => OrigamiLineKind::Result,
                origami_core::Kind::Detail => OrigamiLineKind::Detail,
                origami_core::Kind::Advice => OrigamiLineKind::Advice,
                origami_core::Kind::Warning => OrigamiLineKind::Warning,
                origami_core::Kind::Note => OrigamiLineKind::Note,
                origami_core::Kind::GuestNote => OrigamiLineKind::GuestNote,
            },
            shown: l.shown(),
            copy: l.copy_text().map(Into::into),
            text: l.text,
        })
        .collect()
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct OrigamiCreateRequest {
    pub name: String,
    pub offering: String,
    pub memory_per_node_mib: Option<u32>,
    /// A graphics product, or "none"; without it the topology's default pipe.
    pub graphics: Option<String>,
    pub boot_prom: Option<String>,
    pub io_prom: Option<String>,
    /// An `OrigamiEngineInfo::id` that lists the offering; without it the
    /// first engine that does. The machine keeps it for life.
    pub engine: Option<String>,
    pub system_disk: Option<OrigamiSystemDisk>,
    /// The ID and Ethernet address to give the machine instead of the next free ones.
    pub identity: Option<OrigamiIdentityRequest>,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct OrigamiIdentityRequest {
    /// The ID as typed, 1 to 2999.
    pub emu_id: Option<String>,
    pub mac: Option<String>,
}

/// What New Machine shows for the machine ID and Ethernet address.
#[derive(Clone, Debug, uniffi::Record)]
pub struct OrigamiIdentityPreview {
    pub emu_id: u32,
    /// Empty when the address cannot be made from an ID.
    pub mac: String,
    /// Whether the address is made from the ID, so a typed one is optional.
    pub from_id: bool,
    pub problem: Option<String>,
}

/// The system disk a new machine starts with.
#[derive(Clone, Debug, uniffi::Enum)]
pub enum OrigamiSystemDisk {
    Empty {
        gb: u32,
    },
    /// `copy` puts the file in the machine's folder; `overlay` keeps a CHD's
    /// changes apart from the image.
    Image {
        path: String,
        copy: bool,
        overlay: bool,
    },
    /// An `OrigamiReadyDisk::name`: downloaded and checked first when it is
    /// not cached, then copied in with its changes kept apart.
    Ready {
        name: String,
    },
}

/// A ready-made disk the server offers for a machine.
#[derive(Clone, Debug, uniffi::Record)]
pub struct OrigamiReadyDisk {
    pub name: String,
    /// The IRIX release.
    pub version: String,
    pub bytes: u64,
    /// Already downloaded, so Create needs no download.
    pub cached: bool,
    /// What to show in the list, such as `IRIX 6.5.22, 835 MB`.
    pub label: String,
}

/// A downloaded disk, listed in Preferences.
#[derive(Clone, Debug, uniffi::Record)]
pub struct OrigamiCachedDisk {
    pub key: String,
    pub label: String,
    pub bytes: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum OrigamiDriveKind {
    Disk,
    Cdrom,
    Tape,
}

/// What `check_disk_image` says about a file.
#[derive(Clone, Debug, uniffi::Record)]
pub struct OrigamiDiskImageCheck {
    pub bytes: u64,
    pub is_chd: bool,
    /// Bytes free where a copy would go.
    pub free_bytes: u64,
    /// The name of another machine that already uses the file.
    pub used_by: Option<String>,
    /// Why the file cannot be attached, in the core's words.
    pub problem: Option<String>,
}

/// What to say under the choices for an image, and whether it can be added.
#[derive(Clone, Debug, uniffi::Record)]
pub struct OrigamiImageVerdict {
    pub note: Option<String>,
    /// The note is a warning rather than reassurance.
    pub warning: bool,
    pub ready: bool,
}

/// The New Machine disk choices and how an image is kept. Index 0 of `place` copies
/// the image and index 0 of `keep` keeps a CHD's changes apart.
#[derive(Clone, Debug, uniffi::Record)]
pub struct OrigamiDiskLabels {
    pub choices: Vec<String>,
    pub place: Vec<String>,
    pub keep: Vec<String>,
    pub same_kind: String,
    /// The choice a ready-made disk adds after the others, and the title of
    /// the list of releases under it.
    pub ready: String,
    pub release: String,
    /// The empty disk's size to start with, in GB.
    pub default_gb: u32,
}

/// The note and readiness for a checked image, given whether it is copied in.
#[uniffi::export]
pub fn origami_image_verdict(check: OrigamiDiskImageCheck, copy: bool) -> OrigamiImageVerdict {
    let v = f::image_verdict(
        &f::DiskImageCheck {
            bytes: check.bytes,
            is_chd: check.is_chd,
            free_bytes: check.free_bytes,
            used_by: check.used_by,
            problem: check.problem,
        },
        copy,
    );
    OrigamiImageVerdict {
        note: v.note,
        warning: v.warning,
        ready: v.ready,
    }
}

/// What picking the default model needs of an offering.
#[derive(Clone, Debug, uniffi::Record)]
pub struct OrigamiOfferingKey {
    pub id: String,
    pub product: String,
}

/// The index of the model New Machine selects among `offerings`, or none when empty.
#[uniffi::export]
pub fn origami_default_offering(offerings: Vec<OrigamiOfferingKey>) -> Option<u32> {
    f::default_offering(
        offerings
            .iter()
            .map(|o| (o.id.as_str(), o.product.as_str())),
    )
    .map(|i| i as u32)
}

#[uniffi::export]
pub fn origami_disk_labels() -> OrigamiDiskLabels {
    let l = f::disk_labels();
    OrigamiDiskLabels {
        choices: l.choices,
        place: l.place,
        keep: l.keep,
        same_kind: l.same_kind,
        ready: l.ready,
        release: l.release,
        default_gb: l.default_gb,
    }
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct OrigamiPipeSetting {
    pub slot: String,
    pub product: String,
    pub family: String,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct OrigamiDriveSetting {
    pub name: String,
    /// `disk`, `cdrom` or `tape`.
    pub kind: String,
    pub bus: u32,
    pub target: u32,
    pub image: String,
    pub read_only: bool,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct OrigamiForward {
    pub name: String,
    /// `tcp` or `udp`.
    pub protocol: String,
    pub host_port: u16,
    pub guest_port: u16,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct OrigamiShare {
    pub name: String,
    pub host_dir: String,
    /// What to run as root in IRIX to mount it; empty when `mount_problem` says why not.
    pub mount_lines: Vec<String>,
    pub mount_problem: Option<String>,
    /// Whether an address for shared folders in the Network settings fixes the problem.
    pub needs_address: bool,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct OrigamiConsolePort {
    pub name: String,
    pub label: String,
    pub is_default: bool,
}

/// What a machine's file says, for a settings pane.
#[derive(Clone, Debug, uniffi::Record)]
pub struct OrigamiMachineSettings {
    pub nodes: u32,
    /// The processor module and its part, such as `R12000`. Fixed at creation.
    pub processor: String,
    pub processor_part: String,
    pub cpus_per_node: u32,
    /// The counts `OrigamiChange::SetProcessor` accepts.
    pub cpus_per_node_allowed: Vec<u32>,
    pub memory_per_node_mib: u32,
    /// The sizes `OrigamiChange::SetMemory` accepts.
    pub memory_allowed_mib: Vec<u32>,
    pub pipes: Vec<OrigamiPipeSetting>,
    pub drives: Vec<OrigamiDriveSetting>,
    /// The first free target on the first SCSI bus, for a disk added in Settings.
    pub disk_target: Option<u32>,
    /// The last free target there, for a CD-ROM.
    pub cd_target: Option<u32>,
    pub network: OrigamiNetworkSetting,
    pub forwards: Vec<OrigamiForward>,
    pub shares: Vec<OrigamiShare>,
    /// The serial line in use: the file's choice, else the default.
    pub console_port: String,
    /// The lines `OrigamiChange::SetConsole` accepts.
    pub console_ports: Vec<OrigamiConsolePort>,
}

impl From<f::MachineSettings> for OrigamiMachineSettings {
    fn from(s: f::MachineSettings) -> Self {
        Self {
            nodes: s.nodes,
            processor: s.processor,
            processor_part: s.processor_part,
            cpus_per_node: s.cpus_per_node,
            cpus_per_node_allowed: s.cpus_per_node_allowed,
            memory_per_node_mib: s.memory_per_node_mib,
            memory_allowed_mib: s.memory_allowed_mib,
            pipes: s
                .pipes
                .into_iter()
                .map(|p| OrigamiPipeSetting {
                    slot: p.slot,
                    product: p.product,
                    family: p.family,
                })
                .collect(),
            drives: s
                .drives
                .into_iter()
                .map(|d| OrigamiDriveSetting {
                    name: d.name,
                    kind: d.kind,
                    bus: d.bus,
                    target: d.target,
                    image: d.image,
                    read_only: d.read_only,
                })
                .collect(),
            disk_target: s.disk_target,
            cd_target: s.cd_target,
            network: s.network.into(),
            forwards: s
                .forwards
                .into_iter()
                .map(|p| OrigamiForward {
                    name: p.name,
                    protocol: p.protocol,
                    host_port: p.host_port,
                    guest_port: p.guest_port,
                })
                .collect(),
            shares: s
                .shares
                .into_iter()
                .map(|s| OrigamiShare {
                    name: s.name,
                    host_dir: s.host_dir,
                    mount_lines: s.mount_lines,
                    mount_problem: s.mount_problem,
                    needs_address: s.needs_address,
                })
                .collect(),
            console_port: s.console_port,
            console_ports: s
                .console_ports
                .into_iter()
                .map(|p| OrigamiConsolePort {
                    name: p.name,
                    label: p.label,
                    is_default: p.is_default,
                })
                .collect(),
        }
    }
}

/// A machine's network as its file has it.
#[derive(Clone, Debug, uniffi::Record)]
pub struct OrigamiNetworkSetting {
    /// `user`, `private`, `none`, `tap` or `vmnet`.
    pub mode: String,
    pub endpoint: Option<String>,
    pub interface: Option<String>,
    pub share_address: Option<String>,
    /// The /24 Shared and Private use.
    pub subnet: String,
    pub mac: Option<String>,
    /// The tap device or socket_vmnet socket, as its field shows it.
    pub device: String,
    /// Where the Ethernet address came from.
    pub mac_note: String,
    /// Findings for a bridged machine.
    pub warnings: Vec<String>,
}

impl From<f::NetworkSetting> for OrigamiNetworkSetting {
    fn from(n: f::NetworkSetting) -> Self {
        Self {
            mode: n.mode,
            endpoint: n.endpoint,
            interface: n.interface,
            share_address: n.share_address,
            subnet: n.subnet,
            mac: n.mac,
            device: n.device,
            mac_note: n.mac_note,
            warnings: n.warnings,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum OrigamiNetworkMode {
    User,
    Private,
    None,
    /// Bridged on Linux and Windows.
    Tap,
    /// Bridged on macOS, through socket_vmnet.
    Vmnet,
}

impl From<OrigamiNetworkMode> for origami_core::NetworkMode {
    fn from(m: OrigamiNetworkMode) -> Self {
        match m {
            OrigamiNetworkMode::User => Self::User,
            OrigamiNetworkMode::Private => Self::Private,
            OrigamiNetworkMode::None => Self::None,
            OrigamiNetworkMode::Tap => Self::Tap,
            OrigamiNetworkMode::Vmnet => Self::Vmnet,
        }
    }
}

impl From<origami_core::NetworkMode> for OrigamiNetworkMode {
    fn from(m: origami_core::NetworkMode) -> Self {
        use origami_core::NetworkMode as M;
        match m {
            M::User => Self::User,
            M::Private => Self::Private,
            M::None => Self::None,
            M::Tap => Self::Tap,
            M::Vmnet => Self::Vmnet,
        }
    }
}

/// One way to connect a machine that this computer offers.
#[derive(Clone, Debug, uniffi::Record)]
pub struct OrigamiNetworkChoice {
    pub mode: OrigamiNetworkMode,
    pub label: String,
    pub detail: String,
}

/// How to set up the host side of Bridged on this computer.
#[derive(Clone, Debug, uniffi::Record)]
pub struct OrigamiBridgedSetup {
    /// The label of the one field Bridged needs.
    pub field: String,
    /// An example for it, or the socket found on this computer.
    pub example: String,
    pub steps: String,
    /// The commands in `steps`, for a Copy button.
    pub commands: Option<String>,
}

/// Refuses a share name the guest cannot type, in the core's words.
#[uniffi::export]
pub fn origami_check_share_name(name: String) -> Res<()> {
    Ok(f::check_share_name(&name)?)
}

/// The share name to offer for a host folder: its own name, made valid.
#[uniffi::export]
pub fn origami_suggest_share_name(dir: String) -> String {
    f::suggest_share_name(&dir)
}

/// The modes this computer can run, in picker order.
#[uniffi::export]
pub fn origami_host_network_modes() -> Vec<OrigamiNetworkChoice> {
    f::host_network_modes()
        .into_iter()
        .map(|c| OrigamiNetworkChoice {
            mode: c.mode.into(),
            label: c.label,
            detail: c.detail,
        })
        .collect()
}

#[uniffi::export]
pub fn origami_bridged_setup() -> OrigamiBridgedSetup {
    let b = f::bridged_setup();
    OrigamiBridgedSetup {
        field: b.field,
        example: b.example,
        steps: b.steps,
        commands: b.commands,
    }
}

/// One edit to a stopped machine's settings.
#[derive(Clone, Debug, uniffi::Enum)]
pub enum OrigamiChange {
    SetProcessor {
        cpus_per_node: u32,
    },
    SetMemory {
        mib: u32,
    },
    AddForward {
        forward: OrigamiForward,
    },
    RemoveForward {
        name: String,
    },
    AddShare {
        name: String,
        host_dir: String,
    },
    RemoveShare {
        name: String,
    },
    DetachDrive {
        name: String,
    },
    /// A new, empty system disk.
    CreateDisk {
        size_mib: u64,
    },
    AttachDrive {
        path: String,
        kind: OrigamiDriveKind,
        target: u32,
        overlay: bool,
    },
    SetConsole {
        port: Option<String>,
    },
}

impl From<OrigamiChange> for origami_core::Change {
    fn from(c: OrigamiChange) -> Self {
        match c {
            OrigamiChange::SetProcessor { cpus_per_node } => Self::SetProcessor {
                module: None,
                cpus_per_node: Some(cpus_per_node),
            },
            OrigamiChange::SetMemory { mib } => Self::SetMemory(mib),
            OrigamiChange::AddForward { forward: p } => {
                Self::AddForward(origami_core::PortForward {
                    name: p.name,
                    protocol: p.protocol,
                    host_port: p.host_port,
                    guest_port: p.guest_port,
                })
            }
            OrigamiChange::RemoveForward { name } => Self::RemoveForward(name),
            OrigamiChange::AddShare { name, host_dir } => Self::AddShare {
                name,
                host_dir: PathBuf::from(host_dir),
            },
            OrigamiChange::RemoveShare { name } => Self::RemoveShare(name),
            OrigamiChange::DetachDrive { name } => Self::DetachDrive(name),
            OrigamiChange::CreateDisk { size_mib } => Self::CreateDisk { size_mib },
            OrigamiChange::AttachDrive {
                path,
                kind,
                target,
                overlay,
            } => Self::AttachDrive(drive_request(path, kind, target, overlay)),
            OrigamiChange::SetConsole { port } => Self::SetConsole(port),
        }
    }
}

fn drive_request(
    path: String,
    kind: OrigamiDriveKind,
    target: u32,
    overlay: bool,
) -> origami_core::DriveRequest {
    origami_core::DriveRequest {
        file: PathBuf::from(path),
        kind: match kind {
            OrigamiDriveKind::Disk => origami_core::DriveKind::Disk,
            OrigamiDriveKind::Cdrom => origami_core::DriveKind::Cdrom,
            OrigamiDriveKind::Tape => origami_core::DriveKind::Tape,
        },
        target,
        overlay,
    }
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct OrigamiHeadView {
    pub index: u32,
    pub width: u32,
    pub height: u32,
    pub seq: u64,
    pub active: bool,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct OrigamiConsoleView {
    pub connected: bool,
    pub down_reason: Option<String>,
    pub total: u64,
}

/// Why the engine's program ended. A guest power-off is a clean stop.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum OrigamiExitReason {
    GuestPoweredOff,
    HostStopped,
    Crashed,
    RunnerError,
}

impl From<f::ExitReason> for OrigamiExitReason {
    fn from(r: f::ExitReason) -> Self {
        match r {
            f::ExitReason::GuestPoweredOff => Self::GuestPoweredOff,
            f::ExitReason::HostStopped => Self::HostStopped,
            f::ExitReason::Crashed => Self::Crashed,
            f::ExitReason::RunnerError => Self::RunnerError,
        }
    }
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct OrigamiMachineView {
    pub key: String,
    pub name: String,
    pub product: String,
    pub engine: String,
    /// Whether the machine has a graphics head by its configuration, as
    /// against a serial-only machine whose display is not up yet.
    pub has_display: bool,
    pub state: OrigamiMachineState,
    pub exit_code: Option<i32>,
    /// How the engine's program ended, once the state is `Exited`.
    pub exit_reason: Option<OrigamiExitReason>,
    /// What it said or how it ended, such as `signal 9` or its last error.
    pub exit_detail: Option<String>,
    pub last_error: Option<String>,
    pub reason: String,
    /// The reason, when it says more than the state pill does.
    pub note: Option<String>,
    pub heads: Vec<OrigamiHeadView>,
    pub console: OrigamiConsoleView,
    pub operation: Option<u64>,
}

impl From<f::MachineView> for OrigamiMachineView {
    fn from(v: f::MachineView) -> Self {
        let note = v.note().map(str::to_string);
        Self {
            key: v.key,
            name: v.name,
            product: v.product,
            engine: v.engine,
            has_display: v.has_display,
            state: v.state.into(),
            exit_code: v.exit_code,
            exit_reason: v.exit_reason.map(Into::into),
            exit_detail: v.exit_detail,
            last_error: v.last_error,
            note,
            reason: v.reason,
            heads: v
                .heads
                .into_iter()
                .map(|h| OrigamiHeadView {
                    index: h.index,
                    width: h.width,
                    height: h.height,
                    seq: h.seq,
                    active: h.active,
                })
                .collect(),
            console: OrigamiConsoleView {
                connected: v.console.connected,
                down_reason: v.console.down_reason,
                total: v.console.total,
            },
            operation: v.operation,
        }
    }
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct OrigamiConsoleChunk {
    pub bytes: Vec<u8>,
    pub next: u64,
    pub dropped: u64,
    pub generation: u64,
    /// Output below this offset is replayed history, not just printed.
    pub live_from: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum OrigamiStopKind {
    Clean,
    Force,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum OrigamiControl {
    Reset,
    PowerButton,
    Nmi,
    Pause,
    Resume,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum OrigamiPointerButton {
    Left,
    Middle,
    Right,
}

/// A pointer event. `At` is absolute, in a head's pixels; `Move` is relative
/// guest counts for a captured pointer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum OrigamiPointer {
    Move {
        dx: i32,
        dy: i32,
    },
    At {
        head: u32,
        x: i32,
        y: i32,
    },
    Button {
        button: OrigamiPointerButton,
        down: bool,
    },
    Wheel {
        notches: i32,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum OrigamiOperationState {
    Running,
    Succeeded,
    Failed {
        kind: OrigamiErrorKind,
        message: String,
    },
    Cancelled,
}

impl From<f::OperationState> for OrigamiOperationState {
    fn from(s: f::OperationState) -> Self {
        match s {
            f::OperationState::Running => Self::Running,
            f::OperationState::Succeeded => Self::Succeeded,
            f::OperationState::Failed(e) => Self::Failed {
                kind: e.kind.into(),
                message: e.message,
            },
            f::OperationState::Cancelled => Self::Cancelled,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum OrigamiPlatform {
    Macos,
    Windows,
    Linux,
}

/// When a command is available, given the selected machine's state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum OrigamiEnabledWhen {
    Always,
    MachineSelected,
    MachineStopped,
    MachineLive,
    MachineRunning,
    MachinePaused,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct OrigamiShortcut {
    pub platform: OrigamiPlatform,
    pub accelerator: String,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct OrigamiCommand {
    pub id: String,
    pub title: String,
    pub enabled_when: OrigamiEnabledWhen,
    pub shortcuts: Vec<OrigamiShortcut>,
}

impl From<f::EnabledWhen> for OrigamiEnabledWhen {
    fn from(w: f::EnabledWhen) -> Self {
        match w {
            f::EnabledWhen::Always => Self::Always,
            f::EnabledWhen::MachineSelected => Self::MachineSelected,
            f::EnabledWhen::MachineStopped => Self::MachineStopped,
            f::EnabledWhen::MachineLive => Self::MachineLive,
            f::EnabledWhen::MachineRunning => Self::MachineRunning,
            f::EnabledWhen::MachinePaused => Self::MachinePaused,
        }
    }
}

impl From<OrigamiEnabledWhen> for f::EnabledWhen {
    fn from(w: OrigamiEnabledWhen) -> Self {
        match w {
            OrigamiEnabledWhen::Always => Self::Always,
            OrigamiEnabledWhen::MachineSelected => Self::MachineSelected,
            OrigamiEnabledWhen::MachineStopped => Self::MachineStopped,
            OrigamiEnabledWhen::MachineLive => Self::MachineLive,
            OrigamiEnabledWhen::MachineRunning => Self::MachineRunning,
            OrigamiEnabledWhen::MachinePaused => Self::MachinePaused,
        }
    }
}

/// Whether a command is available when the selected machine is in `state`
/// (None for no selection).
#[uniffi::export]
pub fn origami_command_enabled(
    when: OrigamiEnabledWhen,
    state: Option<OrigamiMachineState>,
) -> bool {
    f::EnabledWhen::from(when).holds(state.map(Into::into))
}

/// The pointer status line, such as `Follows mouse`.
#[uniffi::export]
pub fn origami_pointer_status(follows: bool, captured: bool) -> String {
    f::pointer_status(follows, captured).to_string()
}

/// A byte count as the apps show it, such as `3.3 GB`.
#[uniffi::export]
pub fn origami_size_label(bytes: u64) -> String {
    f::size_label(bytes)
}

/// The ABI version of the frame plane (`origami_frame.h`).
#[uniffi::export]
pub fn origami_frame_plane_version() -> u32 {
    crate::frame::ABI_VERSION
}

/// What the machine window must show, as one JSON document (see
/// `origami_core::behaviour`): the table each app's self-test drives.
#[cfg(feature = "fake")]
#[uniffi::export]
pub fn origami_behaviour_json() -> String {
    origami_core::behaviour::JSON.to_owned()
}

/// Whether a row can run here and what is wrong with what a self-test read
/// back (see `origami_core::behaviour::evaluate`): JSON in, JSON out.
#[cfg(feature = "fake")]
#[uniffi::export]
pub fn origami_behaviour_evaluate(request: String) -> String {
    origami_core::behaviour::evaluate(&request)
}

/// The About credits as one JSON document (see `origami-credits`).
#[uniffi::export]
pub fn origami_credits_json() -> String {
    origami_credits::credits_json()
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct OrigamiInstallProfile {
    pub name: String,
    pub label: String,
    pub detail: String,
}

/// An IRIX release the install assistant offers.
#[derive(Clone, Debug, uniffi::Record)]
pub struct OrigamiInstallRelease {
    pub release: String,
    pub label: String,
    pub profiles: Vec<OrigamiInstallProfile>,
}

/// What a machine's install setup configures.
#[derive(Clone, Debug, uniffi::Record)]
pub struct OrigamiInstallPlan {
    /// What it sets up, for the footer.
    pub summary: String,
    pub method: crate::OrigamiInstallMethod,
    pub release: String,
    pub profile: String,
    pub local_media: bool,
    pub addons: Vec<String>,
}

/// A package to install after IRIX: a known one by `name`, or one from
/// `source` (a file or HTTPS address) with its `install` selections.
#[derive(Clone, Debug, uniffi::Record)]
pub struct OrigamiInstallAddon {
    pub name: String,
    pub source: Option<String>,
    pub install: Vec<String>,
}

/// What the assistant has asked for.
#[derive(Clone, Debug, uniffi::Record)]
pub struct OrigamiInstallSetup {
    pub method: Option<crate::OrigamiInstallMethod>,
    pub release: String,
    pub profile: String,
    pub media_root: Option<String>,
    pub addons: Vec<OrigamiInstallAddon>,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct OrigamiInstallAddonChoice {
    pub name: String,
    pub label: String,
    pub note: Option<String>,
    pub checked: bool,
    pub locked: bool,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct OrigamiMediaFolder {
    pub found: u32,
    pub total: u32,
    pub missing: Vec<String>,
    pub verdict: String,
    pub complete: bool,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct OrigamiGuideStep {
    pub text: String,
    pub command: Option<String>,
    pub send: Option<String>,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct OrigamiInstallGuide {
    pub server: String,
    pub prepare: Vec<OrigamiGuideStep>,
    pub installer: Vec<OrigamiGuideStep>,
    pub packages: Vec<OrigamiGuideStep>,
    pub restart: Vec<OrigamiGuideStep>,
    pub finish: Vec<OrigamiGuideStep>,
    pub package_note: String,
}

impl From<f::GuideStep> for OrigamiGuideStep {
    fn from(s: f::GuideStep) -> Self {
        Self {
            text: s.text,
            command: s.command,
            send: s.send,
        }
    }
}

/// Where the PROM prints.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum OrigamiBootConsole {
    Serial,
    Screen,
}

/// The IRIX releases the install assistant offers.
#[uniffi::export]
pub fn origami_install_releases() -> Vec<OrigamiInstallRelease> {
    f::install_releases()
        .into_iter()
        .map(|r| OrigamiInstallRelease {
            release: r.release,
            label: r.label,
            profiles: r
                .profiles
                .into_iter()
                .map(|p| OrigamiInstallProfile {
                    name: p.name,
                    label: p.label,
                    detail: p.detail,
                })
                .collect(),
        })
        .collect()
}

/// What a folder holds of an install's discs. Reads directory entries only.
#[uniffi::export]
pub fn origami_install_media_folder(
    path: String,
    release: String,
    profile: String,
) -> Res<OrigamiMediaFolder> {
    let m = f::install_media_folder(std::path::Path::new(&path), &release, &profile)?;
    Ok(OrigamiMediaFolder {
        found: m.found,
        total: m.total,
        missing: m.missing,
        verdict: m.verdict,
        complete: m.complete,
    })
}

/// The core. Make one per process.
#[derive(uniffi::Object)]
pub struct OrigamiCore {
    core: f::Core,
    #[cfg(feature = "fake")]
    fake: Arc<origami_engine::fake::FakeEngine>,
}

#[uniffi::export]
impl OrigamiCore {
    /// `library_root` is the folder whose children are machine directories.
    #[uniffi::constructor]
    pub fn new(
        library_root: String,
        observer: Option<Arc<dyn OrigamiObserver>>,
    ) -> Res<Arc<OrigamiCore>> {
        let engines = engines::build()?;
        let observer = observer.map(|o| Arc::new(ObserverBridge(o)) as Arc<dyn f::Observer>);
        let mut config = f::Config::new(library_root);
        config.instigator = engines.instigator;
        let core = f::Core::new(config, engines.list, observer)?;
        Ok(Arc::new(OrigamiCore {
            core,
            #[cfg(feature = "fake")]
            fake: engines.fake,
        }))
    }

    pub fn library_root(&self) -> String {
        self.core.root().to_string_lossy().into_owned()
    }

    /// Switch the library to another folder. Raises a library event; running
    /// machines keep their own paths.
    pub fn set_library_root(&self, library_root: String) {
        self.core.set_root(library_root);
    }

    pub fn engines(&self) -> Vec<OrigamiEngineInfo> {
        self.core
            .engines()
            .into_iter()
            .map(|e| OrigamiEngineInfo {
                name: e.name,
                id: e.id,
                version: e.version,
                max_heads: e.capabilities.max_heads,
                gpu_regions: e.capabilities.gpu_regions,
            })
            .collect()
    }

    /// What can be created. The first call may start the engine's program:
    /// call it off the UI thread.
    pub fn catalogue(&self) -> Res<Vec<OrigamiOffering>> {
        Ok(self
            .core
            .catalogue()?
            .into_iter()
            .map(|o| OrigamiOffering {
                id: o.id,
                label: o.label,
                engine: o.engine,
                product: o.product,
                topology: o.topology,
                topology_name: o.topology_name,
                has_display: o.has_display,
                runs_irix: o.runs_irix,
                nodes: o.nodes,
                cpus: o.cpus,
                memory_default_mib: o.memory_default_mib,
                memory_accepted_mib: o.memory_accepted_mib,
            })
            .collect())
    }

    /// The machines in the library. Reads the disk: off the UI thread.
    pub fn list_machines(&self) -> Res<Vec<OrigamiMachineSummary>> {
        Ok(self
            .core
            .list_machines()?
            .into_iter()
            .map(|m| OrigamiMachineSummary {
                key: m.key,
                name: m.name,
                product: m.product,
                product_label: m.product_label,
                topology: m.topology,
                topology_name: m.topology_name,
                cards: m
                    .cards
                    .into_iter()
                    .map(|c| OrigamiSummaryCard {
                        title: c.title,
                        value: c.value,
                    })
                    .collect(),
                has_display: m.has_display,
                runs_irix: m.runs_irix,
                nodes: m.nodes,
                memory_per_node_mib: m.memory_per_node_mib,
                state: m.state.into(),
                problem: m.problem,
                needs_upgrade: m.needs_upgrade,
                system_disk: m.system_disk,
            })
            .collect())
    }

    /// Upgrade a stopped machine made by an older Origami and say what changed.
    /// May download firmware: off the UI thread.
    pub fn upgrade_machine(&self, key: String) -> Res<Vec<OrigamiReportLine>> {
        Ok(report_lines(self.core.upgrade_machine(&key)?))
    }

    /// The OS and release on a disk image, such as `IRIX 6.5.30`, or
    /// `Unknown`. Reads the file, once per size and change time: off the UI thread.
    pub fn disk_info(&self, path: String) -> Res<String> {
        Ok(self.core.disk_info(std::path::Path::new(&path))?)
    }

    /// Whether a file can be attached and what a copy needs. `machine` is the
    /// one it is for; without it the library folder stands in. Off the UI thread.
    pub fn check_disk_image(
        &self,
        path: String,
        machine: Option<String>,
    ) -> Res<OrigamiDiskImageCheck> {
        let c = self
            .core
            .check_disk_image(std::path::Path::new(&path), machine.as_deref())?;
        Ok(OrigamiDiskImageCheck {
            bytes: c.bytes,
            is_chd: c.is_chd,
            free_bytes: c.free_bytes,
            used_by: c.used_by,
            problem: c.problem,
        })
    }

    /// The ready-made disks that fit an `OrigamiOffering::id`. Asks the
    /// disk server, or uses the last list: off the UI thread.
    pub fn ready_disks(&self, offering: String) -> Res<Vec<OrigamiReadyDisk>> {
        Ok(self
            .core
            .ready_disks(&offering)?
            .into_iter()
            .map(|d| OrigamiReadyDisk {
                name: d.name,
                version: d.version,
                bytes: d.bytes,
                cached: d.cached,
                label: d.label,
            })
            .collect())
    }

    pub fn cached_disks(&self) -> Res<Vec<OrigamiCachedDisk>> {
        Ok(self
            .core
            .cached_disks()?
            .into_iter()
            .map(|d| OrigamiCachedDisk {
                key: d.key,
                label: d.label,
                bytes: d.bytes,
            })
            .collect())
    }

    pub fn remove_cached_disk(&self, key: String) -> Res<()> {
        Ok(self.core.remove_cached_disk(&key)?)
    }

    /// The About egg's `hinv`-style host inventory. Reads the disk and may
    /// open the GPU: call off the UI thread. Nothing leaves the machine.
    pub fn hinv(&self) -> String {
        let machines = self
            .core
            .list_machines()
            .ok()
            .map(|l| l.into_iter().map(|m| (m.name, m.product)).collect());
        let gpu = crate::engines::gpu_name();
        origami_core::hinv::format(&origami_core::hinv::gather(
            &self.core.root(),
            machines,
            gpu,
        ))
    }

    /// Long. The finished operation's `result_machine` is the new key.
    pub fn create_machine(&self, request: OrigamiCreateRequest) -> Res<Arc<OrigamiOperation>> {
        let operation = self.core.create_machine(f::CreateRequest {
            name: request.name,
            offering: request.offering,
            memory_per_node_mib: request.memory_per_node_mib,
            graphics: request.graphics,
            boot_prom: request.boot_prom.map(PathBuf::from),
            io_prom: request.io_prom.map(PathBuf::from),
            engine: request.engine,
            emu_id: request.identity.as_ref().and_then(|i| i.emu_id.clone()),
            mac: request.identity.and_then(|i| i.mac),
            system_disk: request.system_disk.map(|d| match d {
                OrigamiSystemDisk::Empty { gb } => f::SystemDisk::Empty { gb },
                OrigamiSystemDisk::Image {
                    path,
                    copy,
                    overlay,
                } => f::SystemDisk::Image {
                    file: PathBuf::from(path),
                    copy,
                    overlay,
                },
                OrigamiSystemDisk::Ready { name } => f::SystemDisk::Ready { name },
            }),
        })?;
        Ok(Arc::new(OrigamiOperation(operation)))
    }

    /// The ID and Ethernet address a new machine of `offering` would get
    /// with the choices so far. Reads the library folder: off the UI thread.
    pub fn preview_new_identity(
        &self,
        offering: String,
        engine: Option<String>,
        emu_id: String,
        mac: Option<String>,
    ) -> Res<OrigamiIdentityPreview> {
        let p = self.core.preview_new_identity(
            &offering,
            engine.as_deref(),
            &emu_id,
            mac.as_deref(),
        )?;
        Ok(OrigamiIdentityPreview {
            emu_id: p.emu_id,
            mac: p.mac,
            from_id: p.from_id,
            problem: p.problem,
        })
    }

    pub fn open_machine(&self, key: String) -> Res<Arc<OrigamiMachine>> {
        Ok(Arc::new(OrigamiMachine(self.core.open_machine(&key)?)))
    }

    /// The add-ons to offer for `profile` on the machine at `key`.
    pub fn install_addon_choices(
        &self,
        key: String,
        profile: String,
    ) -> Res<Vec<OrigamiInstallAddonChoice>> {
        Ok(self
            .core
            .install_addon_choices(&key, &profile)?
            .into_iter()
            .map(|c| OrigamiInstallAddonChoice {
                name: c.name,
                label: c.label,
                note: c.note,
                checked: c.checked,
                locked: c.locked,
            })
            .collect())
    }

    /// What the machine's install setup configures; none before it is set up.
    pub fn install_plan(&self, key: String) -> Res<Option<OrigamiInstallPlan>> {
        Ok(self.core.install_plan(&key)?.map(|p| OrigamiInstallPlan {
            summary: p.summary,
            method: p.method.into(),
            release: p.release,
            profile: p.profile,
            local_media: p.local_media,
            addons: p.addons,
        }))
    }

    /// Set a stopped machine up for an install, replacing any earlier setup,
    /// and switch it to the installation network. Blocks: off the UI thread.
    pub fn install_prepare(
        &self,
        key: String,
        setup: OrigamiInstallSetup,
    ) -> Res<Vec<OrigamiReportLine>> {
        let setup = f::InstallSetup {
            method: setup.method.map(Into::into),
            release: setup.release,
            profile: setup.profile,
            media_root: setup.media_root.map(PathBuf::from),
            addons: setup
                .addons
                .into_iter()
                .map(|a| f::InstallSetupAddon {
                    name: a.name,
                    source: a.source,
                    install: a.install,
                })
                .collect(),
        };
        Ok(report_lines(self.core.install_prepare(&key, &setup)?))
    }

    /// The disk the install erases, or none when the machine has no disk.
    pub fn install_disk(&self, key: String) -> Res<Option<crate::OrigamiInstallDisk>> {
        Ok(self.core.install_disk(&key)?.map(Into::into))
    }

    /// Put the network back as it was before the setup. Blocks: off the UI thread.
    pub fn install_restore_network(&self, key: String) -> Res<Vec<OrigamiReportLine>> {
        Ok(report_lines(self.core.install_restore_network(&key)?))
    }

    /// Whether the machine still waits on the installation network, so the
    /// assistant should offer to set it back (`set_prepared` on its flow).
    pub fn install_restore_pending(&self, key: String) -> Res<bool> {
        Ok(self.core.install_restore_pending(&key)?)
    }

    /// Forget the machine's install setup. Blocks: off the UI thread.
    pub fn install_reset(&self, key: String) -> Res<()> {
        Ok(self.core.install_reset(&key)?)
    }

    /// The steps to follow at the machine, from its install setup.
    pub fn install_guide(&self, key: String) -> Res<OrigamiInstallGuide> {
        let g = self.core.install_guide(&key)?;
        let steps = |v: Vec<f::GuideStep>| v.into_iter().map(Into::into).collect();
        Ok(OrigamiInstallGuide {
            server: g.server,
            prepare: steps(g.prepare),
            installer: steps(g.installer),
            packages: steps(g.packages),
            restart: steps(g.restart),
            finish: steps(g.finish),
            package_note: g.package_note,
        })
    }

    /// Check the install media. Cancel it to stop it.
    pub fn install_check(&self, key: String, full: bool) -> Res<Arc<OrigamiOperation>> {
        Ok(Arc::new(OrigamiOperation(
            self.core.install_check(&key, full)?,
        )))
    }

    /// Type the install into the guest's serial console from `Inst>`, then
    /// log in as root and halt. The operation holds the console, so it is
    /// read-only until it ends; the machine is stopped when it succeeds. A
    /// failure carries `install_failure`; cancelling leaves the machine
    /// running.
    pub fn install_apply(&self, key: String) -> Res<Arc<OrigamiOperation>> {
        Ok(Arc::new(OrigamiOperation(
            self.core.install_apply_from(&key)?,
        )))
    }

    /// Serve the install media until cancelled; `progress` says what it
    /// serves. The server ends with the core.
    pub fn install_serve(&self, key: String) -> Res<Arc<OrigamiOperation>> {
        Ok(Arc::new(OrigamiOperation(self.core.install_serve(&key)?)))
    }

    /// Open and start, waiting until it runs. Blocks: use
    /// `OrigamiMachine::start` from a UI thread.
    pub fn start_machine(&self, key: String) -> Res<Arc<OrigamiMachine>> {
        Ok(Arc::new(OrigamiMachine(self.core.start_machine(&key)?)))
    }

    pub fn commands(&self) -> Vec<OrigamiCommand> {
        self.core
            .commands()
            .into_iter()
            .map(|c| OrigamiCommand {
                id: c.id,
                title: c.title,
                enabled_when: c.enabled_when.into(),
                shortcuts: c
                    .shortcuts
                    .into_iter()
                    .map(|s| OrigamiShortcut {
                        platform: match s.platform {
                            f::Platform::Macos => OrigamiPlatform::Macos,
                            f::Platform::Windows => OrigamiPlatform::Windows,
                            f::Platform::Linux => OrigamiPlatform::Linux,
                        },
                        accelerator: s.accelerator,
                    })
                    .collect(),
            })
            .collect()
    }

    /// Stop delivering events. Running guests keep running.
    pub fn shutdown(&self) {
        self.core.shutdown();
    }
}

/// Only in builds with the fake engine: drive a started machine.
#[cfg(feature = "fake")]
#[uniffi::export]
impl OrigamiCore {
    /// Give the started machine `key` a new picture.
    pub fn fake_advance_frame(&self, key: String) -> Res<()> {
        let machine = self.core.open_machine(&key)?;
        let session = self
            .fake
            .session_in(std::path::Path::new(machine.key()))
            .ok_or_else(|| {
                f::CoreError::new(f::ErrorKind::NotRunning, "the machine was never started")
            })?;
        session.advance_frame();
        Ok(())
    }

    /// The input events that reached the engine for the started machine
    /// `key`, in order, as `{:?}` text such as `Key { key: A, down: true }`.
    pub fn fake_input(&self, key: String) -> Res<Vec<String>> {
        let machine = self.core.open_machine(&key)?;
        let session = self
            .fake
            .session_in(std::path::Path::new(machine.key()))
            .ok_or_else(|| {
                f::CoreError::new(f::ErrorKind::NotRunning, "the machine was never started")
            })?;
        Ok(session.input().iter().map(|e| format!("{e:?}")).collect())
    }

    /// The CD changes that reached the engine for the started machine `key`,
    /// in order, as `drive image` with `eject` for no image.
    pub fn fake_media(&self, key: String) -> Res<Vec<String>> {
        let machine = self.core.open_machine(&key)?;
        let session = self
            .fake
            .session_in(std::path::Path::new(machine.key()))
            .ok_or_else(|| {
                f::CoreError::new(f::ErrorKind::NotRunning, "the machine was never started")
            })?;
        Ok(session
            .media_changes()
            .into_iter()
            .map(|(drive, image)| match image {
                Some(image) => format!("{drive} {}", image.display()),
                None => format!("{drive} eject"),
            })
            .collect())
    }

    /// Make the guest print `text` on its console.
    pub fn fake_say(&self, key: String, text: String) -> Res<()> {
        let machine = self.core.open_machine(&key)?;
        let session = self
            .fake
            .session_in(std::path::Path::new(machine.key()))
            .ok_or_else(|| {
                f::CoreError::new(f::ErrorKind::NotRunning, "the machine was never started")
            })?;
        session.say(&text);
        Ok(())
    }
}

/// One legend entry of an activity bar, and the name of its segment.
#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct OrigamiOsviewKey {
    pub label: String,
    /// 0xRRGGBB.
    pub rgb: u32,
}

/// One segment of an activity bar: a share of its length from 0 to 1, in
/// order from the left.
#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct OrigamiOsviewSegment {
    pub fraction: f32,
    pub rgb: u32,
}

/// A labelled bar: a title, a legend and one segment per legend entry.
#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct OrigamiOsviewBar {
    pub title: String,
    pub legend: Vec<OrigamiOsviewKey>,
    pub segments: Vec<OrigamiOsviewSegment>,
}

/// The activity view as the core describes it, for the app to paint: the
/// bars, the colours, how many ruler marks, and what the numbers are. With no
/// bars, `note` says why.
#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct OrigamiOsview {
    pub bars: Vec<OrigamiOsviewBar>,
    pub ticks: u32,
    pub background: u32,
    pub bar_background: u32,
    pub caption: String,
    pub note: Option<String>,
}

impl From<f::Osview> for OrigamiOsview {
    fn from(v: f::Osview) -> Self {
        Self {
            bars: v
                .bars
                .into_iter()
                .map(|b| OrigamiOsviewBar {
                    title: b.title,
                    legend: b
                        .legend
                        .into_iter()
                        .map(|k| OrigamiOsviewKey {
                            label: k.label,
                            rgb: k.rgb,
                        })
                        .collect(),
                    segments: b
                        .segments
                        .into_iter()
                        .map(|s| OrigamiOsviewSegment {
                            fraction: s.fraction,
                            rgb: s.rgb,
                        })
                        .collect(),
                })
                .collect(),
            ticks: v.ticks,
            background: v.background,
            bar_background: v.bar_background,
            caption: v.caption,
            note: v.note,
        }
    }
}

#[derive(uniffi::Object)]
pub struct OrigamiMachine(pub(crate) f::Machine);

#[uniffi::export]
impl OrigamiMachine {
    pub fn key(&self) -> String {
        self.0.key().to_string()
    }

    pub fn name(&self) -> String {
        self.0.name().to_string()
    }

    /// Never blocks: the engine's run state is a cached copy that a thread of
    /// the engine keeps current, so a timer on the UI thread may call it.
    pub fn view(&self) -> OrigamiMachineView {
        self.0.view().into()
    }

    /// The Diagnostics report, the same in every app. `status` is the word
    /// on the status pill; `updates` and `shown` are the app's counts of
    /// frame updates received and pictures drawn.
    pub fn diagnostics_report(&self, status: String, updates: u64, shown: u64) -> String {
        self.0.diagnostics_report(&status, updates, shown)
    }

    /// Long: returns at once.
    pub fn start(&self) -> Res<Arc<OrigamiOperation>> {
        Ok(Arc::new(OrigamiOperation(self.0.start()?)))
    }

    /// Long: a clean stop can take minutes.
    pub fn stop(&self, how: OrigamiStopKind) -> Res<Arc<OrigamiOperation>> {
        let how = match how {
            OrigamiStopKind::Clean => f::StopKind::Clean,
            OrigamiStopKind::Force => f::StopKind::Force,
        };
        Ok(Arc::new(OrigamiOperation(self.0.stop(how)?)))
    }

    pub fn control(&self, control: OrigamiControl) -> Res<()> {
        self.0.control(match control {
            OrigamiControl::Reset => f::Control::Reset,
            OrigamiControl::PowerButton => f::Control::PowerButton,
            OrigamiControl::Nmi => f::Control::Nmi,
            OrigamiControl::Pause => f::Control::Pause,
            OrigamiControl::Resume => f::Control::Resume,
        })?;
        Ok(())
    }

    /// Put `image` in a removable drive, or eject with None. A running machine
    /// changes at once; a stopped one's file changes.
    pub fn change_media(&self, drive: String, image: Option<String>) -> Res<()> {
        Ok(self
            .0
            .change_media(&drive, image.as_deref().map(std::path::Path::new))?)
    }

    /// Add a disk image to the stopped machine, using the file or copying it
    /// into the machine's folder. Long: a copy reports `fraction` and can be cancelled.
    pub fn import_drive(
        &self,
        path: String,
        kind: OrigamiDriveKind,
        target: u32,
        overlay: bool,
        copy: bool,
    ) -> Res<Arc<OrigamiOperation>> {
        let request = drive_request(path, kind, target, overlay);
        Ok(Arc::new(OrigamiOperation(
            self.0.import_drive(request, copy)?,
        )))
    }

    /// What the disk in `drive` holds, such as `IRIX 6.5.30`. Off the UI
    /// thread; `Busy` for a running machine that was not read before.
    pub fn drive_info(&self, drive: String) -> Res<String> {
        Ok(self.0.drive_info(&drive)?)
    }

    /// The machine file's settings. Reads the disk: off the UI thread.
    /// Where the PROM prints. Reads the machine's files: off the UI thread.
    pub fn boot_console(&self) -> Res<OrigamiBootConsole> {
        Ok(match self.0.boot_console()? {
            f::BootConsole::Serial => OrigamiBootConsole::Serial,
            f::BootConsole::Screen => OrigamiBootConsole::Screen,
        })
    }

    pub fn settings(&self) -> Res<OrigamiMachineSettings> {
        Ok(self.0.settings()?.into())
    }

    /// Move the stopped machine to `mode`; the core builds the whole change from
    /// its file. `device` is the Bridged field's text. Off the UI thread.
    pub fn set_network(
        &self,
        mode: OrigamiNetworkMode,
        device: String,
        share_address: Option<String>,
        subnet: Option<String>,
    ) -> Res<Vec<OrigamiReportLine>> {
        let report = self.0.set_network(
            mode.into(),
            &device,
            share_address.as_deref(),
            subnet.as_deref(),
        )?;
        Ok(report_lines(report))
    }

    /// Edit the machine. `Busy` unless it is stopped. Off the UI thread;
    /// read `settings` again afterwards. The lines carry advice to show.
    pub fn apply_change(&self, change: OrigamiChange) -> Res<Vec<OrigamiReportLine>> {
        Ok(report_lines(self.0.apply_change(&change.into())?))
    }

    pub fn screendump(&self, head: u32, path: String) -> Res<()> {
        Ok(self
            .0
            .screendump(head as usize, std::path::Path::new(&path))?)
    }

    pub fn send_key(&self, key: OrigamiKey, down: bool) -> Res<()> {
        Ok(self.0.send_key(key.into(), down)?)
    }

    pub fn send_pointer(&self, event: OrigamiPointer) -> Res<()> {
        let button = |b: OrigamiPointerButton| match b {
            OrigamiPointerButton::Left => f::PointerButton::Left,
            OrigamiPointerButton::Middle => f::PointerButton::Middle,
            OrigamiPointerButton::Right => f::PointerButton::Right,
        };
        let event = match event {
            OrigamiPointer::Move { dx, dy } => f::InputEvent::PointerMove { dx, dy },
            OrigamiPointer::At { head, x, y } => f::InputEvent::PointerAt { head, x, y },
            OrigamiPointer::Button { button: b, down } => f::InputEvent::Button {
                button: button(b),
                down,
            },
            OrigamiPointer::Wheel { notches } => f::InputEvent::Wheel { notches },
        };
        Ok(self.0.send_input(&[event])?)
    }

    /// Lift every key and button the guest thinks is down; call when the
    /// window loses focus. Returns how many were lifted.
    pub fn release_all_keys(&self) -> Res<u32> {
        Ok(self.0.release_all_keys()?)
    }

    /// Console output from byte `from`; pass `next` back to continue.
    pub fn console_read(&self, from: u64, max: u32) -> OrigamiConsoleChunk {
        let chunk = self.0.console_read(from, max as usize);
        OrigamiConsoleChunk {
            bytes: chunk.bytes,
            next: chunk.next,
            dropped: chunk.dropped,
            generation: chunk.generation,
            live_from: chunk.live_from,
        }
    }

    pub fn console_send(&self, bytes: Vec<u8>) -> Res<()> {
        Ok(self.0.console_send(&bytes)?)
    }

    /// The activity view to paint: bars for the latest per-CPU host load,
    /// gliding from the last one returned. Does not block; the core samples
    /// on its own thread while this keeps being called, so poll it at about
    /// 2 Hz and only while it is shown. With no bars, `note` says why.
    pub fn osview(&self) -> OrigamiOsview {
        self.0.osview().into()
    }

    /// An owned `origami_machine*` for the C frame plane, as an integer. The
    /// caller releases it with `origami_machine_release`.
    pub fn frame_plane_handle(&self) -> u64 {
        crate::frame::retain_machine(&self.0) as usize as u64
    }
}

#[derive(uniffi::Object)]
pub struct OrigamiOperation(f::Operation);

#[uniffi::export]
impl OrigamiOperation {
    pub fn id(&self) -> u64 {
        self.0.id()
    }

    pub fn title(&self) -> String {
        self.0.title().to_string()
    }

    pub fn state(&self) -> OrigamiOperationState {
        self.0.state().into()
    }

    /// Block until it finishes or `timeout_ms` passes (None waits as long as
    /// it takes). Not for a UI thread; watch for the `Operation` event.
    pub fn wait(&self, timeout_ms: Option<u64>) -> OrigamiOperationState {
        self.0.wait(timeout_ms.map(Duration::from_millis)).into()
    }

    /// False when the operation cannot be cancelled or has finished.
    pub fn cancel(&self) -> bool {
        self.0.cancel()
    }

    /// The latest line the work reported, such as `1.8 GB of 3.3 GB`.
    pub fn progress(&self) -> String {
        self.0.progress()
    }

    /// How far along it is, 0 to 1, when it says.
    /// How an automated install stopped, once it has.
    pub fn install_failure(&self) -> Option<crate::OrigamiInstallFailure> {
        self.0.install_failure().map(Into::into)
    }

    pub fn fraction(&self) -> Option<f32> {
        self.0.fraction()
    }

    /// The machine a finished create or start produced.
    pub fn result_machine(&self) -> Option<String> {
        self.0.result_machine()
    }
}
