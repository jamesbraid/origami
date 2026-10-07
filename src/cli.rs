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
    /// List available machine presets
    Machines,
    /// Show version and Git identity for each bundled executable
    Version,
    /// Create a machine configuration and acquire its verified PROM
    #[command(after_help = "Example: origami create o200 --preset origin200-1")]
    Create(Box<CreateArgs>),
    /// Check a machine configuration
    Validate(MachineArgs),
    /// Upgrade a stopped machine created by Origami 0.1
    Upgrade {
        #[arg(value_name = "DIR")]
        dir: PathBuf,
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
    /// Choose the serial port a stopped guest's console uses
    ConsoleSet {
        #[arg(value_name = "DIR")]
        dir: PathBuf,
        /// Serial line to connect, named as in QEMU's machine list (such as l1
        /// or ioc3_a); omit to use the machine's default console
        #[arg(long)]
        port: Option<String>,
    },
    /// Change networking on a stopped guest
    NetworkSet {
        #[arg(value_name = "DIR")]
        dir: PathBuf,
        #[arg(long, value_enum)]
        mode: NetworkMode,
        #[arg(long, required_if_eq("mode", "private"))]
        endpoint: Option<String>,
        /// Machine MAC, when it has none yet
        #[arg(long, requires = "endpoint")]
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
    /// Configure install media and private networking without starting the guest
    InstallInit {
        #[arg(value_name = "DIR")]
        dir: PathBuf,
        #[arg(long, value_name = "DIR")]
        media_root: Option<PathBuf>,
        /// Machine MAC, when it has none yet
        #[arg(long)]
        mac: Option<String>,
        #[arg(long, value_enum, default_value = "desktop")]
        profile: Profile,
    },
    /// Add an optional package source to an installation
    InstallAddon {
        #[arg(value_name = "DIR")]
        dir: PathBuf,
        #[arg(long)]
        name: String,
        #[arg(long, value_name = "PATH-OR-URL")]
        source: String,
        #[arg(long, required = true, action = clap::ArgAction::Append)]
        install: Vec<String>,
        #[arg(long)]
        base: Option<String>,
        #[arg(long)]
        dist: Option<String>,
    },
    /// Open media and assemble the install tree without installing IRIX
    InstallCheck(MachineArgs),
    /// Serve installation media until stopped
    InstallServe(MachineArgs),
    /// At Inst>, install the selected profile and optional add-on
    InstallApply {
        #[arg(value_name = "DIR")]
        dir: PathBuf,
        #[arg(long)]
        addon: Option<String>,
    },
    /// At Inst>, configure RAD4, build the installed kernel and reboot
    InstallFinish(MachineArgs),
}

#[derive(Debug, Args)]
pub struct MachineArgs {
    /// Machine directory
    #[arg(value_name = "DIR")]
    pub dir: PathBuf,
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
    /// Memory per node in MiB
    #[arg(long, value_name = "MiB")]
    pub memory_per_node: Option<u32>,
    /// Graphics device supported by the preset
    #[arg(long)]
    pub graphics: Option<String>,
    /// The machine's Ethernet address, for its identity and onboard network
    #[arg(long, help_heading = "Hardware options")]
    pub mac: Option<String>,
    #[command(flatten)]
    pub hardware: HardwareArgs,
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
    pub fn resolve(&self, graphics: &str) -> origami::Result<Display> {
        let mode = self.display.unwrap_or(if graphics == "none" {
            DisplayMode::None
        } else {
            DisplayMode::Local
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
}
impl NetworkMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::None => "none",
            Self::Private => "private",
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
pub enum Profile {
    Base,
    Desktop,
    Development,
    LegacyDevelopment,
}
impl Profile {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Base => "base",
            Self::Desktop => "desktop",
            Self::Development => "development",
            Self::LegacyDevelopment => "legacy-development",
        }
    }
}
