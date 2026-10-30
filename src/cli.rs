use clap::{Args, Parser, Subcommand, ValueEnum};
use origami::runtime::Display;
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(
    name = "origami",
    about = "Experimental SGI emulation",
    arg_required_else_help = true,
    args_conflicts_with_subcommands = true
)]
pub struct Cli {
    /// Show version and Git identity for each bundled executable
    #[arg(short = 'V', long, exclusive = true)]
    pub version: bool,
    #[command(subcommand)]
    pub command: Option<Action>,
}

#[derive(Debug, Subcommand)]
pub enum Action {
    /// List machine presets, or show one preset's graphics and processor choices
    #[command(after_help = "Example: origami machines octane2-vpro")]
    Machines {
        /// Show this preset in detail
        #[arg(value_name = "PRESET", conflicts_with = "all")]
        preset: Option<String>,
        /// Show every preset in detail
        #[arg(long)]
        all: bool,
    },
    /// Show version and Git identity for each bundled executable
    Version,
    /// Create a machine configuration and acquire its verified PROM
    #[command(after_help = "Example: origami create o200 --preset origin200-1")]
    Create(Box<CreateArgs>),
    /// Check a machine configuration
    Validate(MachineArgs),
    /// Upgrade a stopped machine created by an earlier Origami
    Upgrade {
        #[arg(value_name = "DIR")]
        dir: PathBuf,
        /// Local boot PROM instead of a download, for a machine that needs one
        #[arg(long, value_name = "FILE")]
        prom: Option<PathBuf>,
        /// Local IO PROM for a machine with a BASEIO or GIGAchannel board
        #[arg(long, value_name = "FILE")]
        io_prom: Option<PathBuf>,
    },
    /// Show a machine configuration
    Show(MachineArgs),
    /// Print the QEMU command without starting the guest
    ShowCommand(LaunchArgs),
    /// Start a guest
    #[command(
        after_help = "Examples:\n  origami run o200\n  origami run o200 --display vnc --vnc-port 5900"
    )]
    Run(RunArgs),
    #[command(name = "_serve", hide = true)]
    Serve(LaunchArgs),
    /// Show whether a machine is running
    Status(MachineArgs),
    /// Connect to a background guest's serial console
    Console(MachineArgs),
    /// Stop a background guest
    Stop(MachineArgs),
    /// Create and attach a new system disk
    DriveCreate {
        #[arg(value_name = "DIR")]
        dir: PathBuf,
        #[arg(value_name = "SIZE-MiB", value_parser = clap::value_parser!(u64).range(1..=131072))]
        size: u64,
    },
    /// Attach an existing disk, CD or tape image
    DriveAttach {
        #[arg(value_name = "DIR")]
        dir: PathBuf,
        #[arg(value_name = "FILE")]
        file: PathBuf,
        #[arg(long = "type", value_enum)]
        kind: DriveKind,
        #[arg(long)]
        target: u32,
    },
    /// Detach a drive without deleting its image
    DriveDetach {
        #[arg(value_name = "DIR")]
        dir: PathBuf,
        name: String,
    },
    /// Change a stopped machine's graphics, where its topology allows that
    #[command(after_help = "No graphics can change after a machine is created yet; \
                      create a new machine instead.")]
    GraphicsSet {
        #[arg(value_name = "DIR")]
        dir: PathBuf,
        #[command(flatten)]
        graphics: GraphicsArgs,
    },
    /// Choose the serial port a stopped guest's console uses
    ConsoleSet {
        #[arg(value_name = "DIR")]
        dir: PathBuf,
        /// Serial line to connect, named as in QEMU's machine list (such as l1
        /// or ioc3_a); omit to use the machine's default console
        #[arg(long)]
        port: Option<String>,
    },
    /// List or change a stopped machine's PROM environment in its NVRAM
    #[command(after_help = "Examples:\n  origami nvram octane\n  \
                      origami nvram octane get console\n  \
                      origami nvram octane set console=d AutoLoad=Yes")]
    Nvram(NvramArgs),
    /// Change networking on a stopped guest
    NetworkSet {
        #[arg(value_name = "DIR")]
        dir: PathBuf,
        #[arg(long, value_enum)]
        mode: NetworkMode,
        /// Instigator's private network endpoint, or the socket_vmnet socket;
        /// a private network without one is hosted by `run`
        #[arg(long, required_if_eq("mode", "vmnet"))]
        endpoint: Option<String>,
        /// Tap device on Linux, or TAP-Windows6 adapter name on Windows
        #[arg(long, required_if_eq("mode", "tap"))]
        interface: Option<String>,
        /// Where shares are served on a tap or vmnet network, such as
        /// 192.168.1.50/24: a free address on that network
        #[arg(long, value_name = "ADDRESS/PREFIX")]
        share_address: Option<String>,
        /// The private /24 that user and private networking use, such as
        /// 10.42.0.0/24 (default 10.98.0.0/24); it stays set across mode changes
        #[arg(long, value_name = "A.B.C.0/24")]
        subnet: Option<String>,
        /// Machine MAC, when it has none yet
        #[arg(long)]
        mac: Option<String>,
    },
    /// Forward a host loopback port to a guest port
    NetworkForwardAdd {
        #[arg(value_name = "DIR")]
        dir: PathBuf,
        name: String,
        #[arg(long, value_enum)]
        protocol: Protocol,
        #[arg(long, value_parser = clap::value_parser!(u16).range(1..))]
        host_port: u16,
        #[arg(long, value_parser = clap::value_parser!(u16).range(1..))]
        guest_port: u16,
    },
    /// Remove a port forward
    NetworkForwardRemove {
        #[arg(value_name = "DIR")]
        dir: PathBuf,
        name: String,
    },
    /// Share a host folder with the guest over NFS, or remove a share
    #[command(
        after_help = "Examples:\n  origami share o200 src ~/code\n  origami share o200 src --remove"
    )]
    Share {
        #[arg(value_name = "DIR")]
        dir: PathBuf,
        /// The name the guest mounts, as ADDRESS:/NAME
        name: String,
        /// The host folder to share
        #[arg(value_name = "HOSTDIR", required_unless_present = "remove")]
        host_dir: Option<PathBuf>,
        /// Remove the share
        #[arg(long, conflicts_with = "host_dir")]
        remove: bool,
    },
    /// Configure install media and private networking without starting the guest
    InstallInit {
        #[arg(value_name = "DIR")]
        dir: PathBuf,
        #[arg(long, value_name = "DIR")]
        media_root: Option<PathBuf>,
        /// Machine MAC, when it has none yet
        #[arg(long)]
        mac: Option<String>,
        #[arg(long, value_enum, default_value = "standard")]
        profile: Profile,
        /// IRIX release to install
        #[arg(long, value_enum, default_value = "6.5.30")]
        release: Release,
    },
    /// Add an optional package source to an installation
    InstallAddon {
        #[arg(value_name = "DIR")]
        dir: PathBuf,
        /// Add-on name; mipspro and rad4 need no --source
        #[arg(long)]
        name: String,
        #[arg(long, value_name = "PATH-OR-URL", requires = "install")]
        source: Option<String>,
        #[arg(long, action = clap::ArgAction::Append, requires = "source")]
        install: Vec<String>,
        #[arg(long, requires = "source")]
        base: Option<String>,
        #[arg(long, requires = "source")]
        dist: Option<String>,
        /// Local media for a named add-on, the directory containing mipspro/
        #[arg(long, value_name = "DIR", conflicts_with = "source")]
        media_root: Option<PathBuf>,
    },
    /// Check install media without installing IRIX; remote media are only probed
    InstallCheck {
        #[command(flatten)]
        machine: MachineArgs,
        /// Also read remote media and assemble the install tree, as install-serve does
        #[arg(long)]
        full: bool,
    },
    /// Serve installation media until stopped
    InstallServe(MachineArgs),
    /// At Inst>, install the selected profile and its add-ons, then restart
    InstallApply {
        #[arg(value_name = "DIR")]
        dir: PathBuf,
    },
}

#[derive(Debug, Args)]
pub struct MachineArgs {
    /// Machine directory
    #[arg(value_name = "DIR")]
    pub dir: PathBuf,
}

#[derive(Debug, Args)]
pub struct NvramArgs {
    /// Machine directory
    #[arg(value_name = "DIR")]
    pub dir: PathBuf,
    #[command(subcommand)]
    pub action: Option<NvramAction>,
}

#[derive(Debug, Subcommand)]
pub enum NvramAction {
    /// Show one variable
    Get { name: String },
    /// Set variables and recompute the checksum
    Set {
        #[arg(value_name = "NAME=VALUE", required = true, value_parser = parse_setting)]
        settings: Vec<(String, String)>,
    },
}

#[derive(Debug, Args)]
pub struct CreateArgs {
    /// Destination machine directory
    #[arg(value_name = "DIR")]
    pub dir: PathBuf,
    /// Machine preset from origami machines
    #[arg(long)]
    pub preset: String,
    #[command(flatten)]
    pub options: CreateOptions,
}

#[derive(Debug, Default, Args)]
pub struct CreateOptions {
    /// Local boot PROM instead of a download
    #[arg(long, value_name = "FILE")]
    pub prom: Option<PathBuf>,
    /// Local IO PROM for presets with a BASEIO or GIGAchannel board
    #[arg(long, value_name = "FILE")]
    pub io_prom: Option<PathBuf>,
    /// Processor module, for presets that offer a choice (see origami machines)
    #[arg(long, value_name = "ID")]
    pub processor: Option<String>,
    /// Memory per node in MiB
    #[arg(long, value_name = "MiB")]
    pub memory_per_node: Option<u32>,
    #[command(flatten)]
    pub graphics: GraphicsArgs,
    /// The machine's Ethernet address, for its identity and onboard network
    #[arg(long, help_heading = "Hardware options")]
    pub mac: Option<String>,
    #[command(flatten)]
    pub hardware: HardwareArgs,
}

/// A graphics pipe as `origami machines` lists them.
#[derive(Debug, Default, Args)]
#[command(next_help_heading = "Graphics options")]
pub struct GraphicsArgs {
    /// Graphics product, such as si, v12 or ir2, or none for no graphics;
    /// omit for the preset's
    #[arg(long, value_name = "PRODUCT")]
    pub graphics: Option<String>,
    /// Slot for the pipe, such as io3 or widget-11; omit for the preset's
    /// or the machine's usual slot
    #[arg(long, value_name = "SLOT")]
    pub graphics_slot: Option<String>,
    /// How many pipes of that product the machine fits, such as 4 for an Onyx
    /// 3400; see origami machines PRESET for its limit
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u32).range(1..))]
    pub graphics_pipes: Option<u32>,
    /// A pipe setting other than its default, such as rm-count=1; repeatable
    #[arg(long = "graphics-setting", value_name = "NAME=VALUE", value_parser = parse_setting)]
    pub graphics_settings: Vec<(String, String)>,
}

impl GraphicsArgs {
    pub fn settings(&self) -> Result<BTreeMap<String, String>, String> {
        let mut settings = BTreeMap::new();
        for (key, value) in &self.graphics_settings {
            if settings.insert(key.clone(), value.clone()).is_some() {
                return Err(format!("--graphics-setting {key} given twice"));
            }
        }
        Ok(settings)
    }
}

#[derive(Debug, Default, Args)]
#[command(next_help_heading = "Hardware options")]
pub struct HardwareArgs {
    /// Override a machine list value or set a machine input, such as
    /// board-id-word=0x4000 or r12000-prid=0xe24; repeatable
    #[arg(long = "set", value_name = "PROPERTY=VALUE", value_parser = parse_setting)]
    pub settings: Vec<(String, String)>,
}

fn parse_setting(text: &str) -> Result<(String, String), String> {
    let (key, value) = text
        .split_once('=')
        .ok_or_else(|| format!("expected PROPERTY=VALUE, got {text}"))?;
    if key.is_empty() {
        return Err(format!("expected PROPERTY=VALUE, got {text}"));
    }
    Ok((key.into(), value.into()))
}

impl HardwareArgs {
    pub fn inputs(&self) -> Result<BTreeMap<String, String>, String> {
        let mut inputs = BTreeMap::new();
        for (key, value) in &self.settings {
            if inputs.insert(key.clone(), value.clone()).is_some() {
                return Err(format!("--set {key} given twice"));
            }
        }
        Ok(inputs)
    }
}

#[derive(Debug, Args)]
pub struct LaunchArgs {
    #[command(flatten)]
    pub machine: MachineArgs,
    #[command(flatten)]
    pub display: DisplayArgs,
    /// Extra QEMU argument, appended after the `[qemu] extra-args` of
    /// machine.toml (repeatable; unsupported and not validated)
    #[arg(long = "qemu-arg", value_name = "ARG", allow_hyphen_values = true)]
    pub qemu_args: Vec<String>,
}

#[derive(Debug, Args)]
pub struct RunArgs {
    #[command(flatten)]
    pub launch: LaunchArgs,
    /// Run with console and installer control connections
    #[arg(long)]
    pub background: bool,
}

#[derive(Debug, Default, Args)]
pub struct DisplayArgs {
    /// Local window, VNC server or no graphics
    #[arg(long, value_enum)]
    pub display: Option<DisplayMode>,
    /// VNC TCP port (default 5900)
    #[arg(long, requires = "display", value_parser = clap::value_parser!(u16).range(5900..))]
    pub vnc_port: Option<u16>,
}

impl DisplayArgs {
    pub fn resolve(&self, graphics: bool) -> origami::Result<Display> {
        let mode = self.display.unwrap_or(if graphics {
            DisplayMode::Local
        } else {
            DisplayMode::None
        });
        if self.vnc_port.is_some() && mode != DisplayMode::Vnc {
            return Err("--vnc-port requires --display vnc".into());
        }
        Ok(match mode {
            DisplayMode::Local => Display::Local,
            DisplayMode::None => Display::None,
            DisplayMode::Vnc => Display::Vnc {
                port: self.vnc_port.unwrap_or(5900),
            },
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum DisplayMode {
    Local,
    Vnc,
    None,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum DriveKind {
    Disk,
    Cdrom,
    Tape,
}
impl DriveKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Disk => "disk",
            Self::Cdrom => "cdrom",
            Self::Tape => "tape",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum NetworkMode {
    User,
    None,
    Private,
    Tap,
    Vmnet,
}
impl NetworkMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::None => "none",
            Self::Private => "private",
            Self::Tap => "tap",
            Self::Vmnet => "vmnet",
        }
    }
}
#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum Protocol {
    Tcp,
    Udp,
}
impl Protocol {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Tcp => "tcp",
            Self::Udp => "udp",
        }
    }
}
#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum Release {
    #[value(name = "6.5.22")]
    R6_5_22,
    #[value(name = "6.5.30")]
    R6_5_30,
}
impl Release {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::R6_5_22 => "6.5.22",
            Self::R6_5_30 => "6.5.30",
        }
    }
}
#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum Profile {
    Minimal,
    Standard,
    #[value(hide = true)]
    Base,
    #[value(hide = true)]
    Desktop,
    #[value(hide = true)]
    Development,
}
impl Profile {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Minimal | Self::Base => "minimal",
            Self::Standard | Self::Desktop => "standard",
            Self::Development => "development",
        }
    }

    /// The name an earlier release used, when this is one.
    pub fn old_name(self) -> Option<&'static str> {
        match self {
            Self::Base => Some("base"),
            Self::Desktop => Some("desktop"),
            _ => None,
        }
    }
}
