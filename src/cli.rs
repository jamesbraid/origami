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
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// List available machine presets
    Machines,
    /// Show version and Git identity for each bundled executable
    Version,
    /// Create a machine configuration and acquire its verified PROM
    #[command(after_help = "Example: origami create o200 --preset origin200-1")]
    Create(CreateArgs),
    /// Check a machine configuration
    Validate(MachineArgs),
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
        /// Serial line to connect, named as in QEMU's catalogue (such as l1
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
    /// Ethernet address an Origin 300 or Fuel serves as its identity
    #[arg(long, help_heading = "Hardware options")]
    pub mac: Option<String>,
    #[command(flatten)]
    pub hardware: HardwareArgs,
}

#[derive(Debug, Default, Args)]
#[command(next_help_heading = "Hardware options")]
pub struct HardwareArgs {
    /// Override the board identification word
    #[arg(long)]
    pub board_id_word: Option<String>,
    /// Override the IOC3 PCI subsystem ID
    #[arg(long)]
    pub ioc3_subsystem_id: Option<String>,
    /// Override the L1 identity reply byte
    #[arg(long)]
    pub l1_type_code: Option<String>,
    /// Override the L1 firmware revision (major.minor.patch)
    #[arg(long)]
    pub l1_revision: Option<String>,
    /// Override the Bedrock revision
    #[arg(long)]
    pub bedrock_revision: Option<String>,
}

impl HardwareArgs {
    pub fn inputs(&self) -> BTreeMap<String, String> {
        [
            ("board-id-word", &self.board_id_word),
            ("ioc3-subsystem-id", &self.ioc3_subsystem_id),
            ("l1-type-code", &self.l1_type_code),
            ("l1-revision", &self.l1_revision),
            ("bedrock-revision", &self.bedrock_revision),
        ]
        .into_iter()
        .filter_map(|(name, value)| value.as_ref().map(|value| (name.into(), value.clone())))
        .collect()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vnc_options_are_typed_and_validate_port_ranges() {
        let cli = Cli::try_parse_from([
            "origami",
            "run",
            "machine",
            "--display",
            "vnc",
            "--vnc-port",
            "5991",
        ])
        .unwrap();
        let Some(Command::Run(args)) = cli.command else {
            panic!("expected run");
        };
        assert_eq!(
            args.launch.display.resolve("none").unwrap(),
            Display::Vnc { port: 5991 }
        );
        assert_eq!(
            DisplayArgs::default().resolve("none").unwrap(),
            Display::None
        );
        assert_eq!(
            DisplayArgs::default().resolve("rad4").unwrap(),
            Display::Local
        );
        for port in ["5899", "65536", "abc"] {
            assert!(Cli::try_parse_from([
                "origami",
                "run",
                "machine",
                "--display",
                "vnc",
                "--vnc-port",
                port
            ])
            .is_err());
        }
        assert!(DisplayArgs {
            display: Some(DisplayMode::Local),
            vnc_port: Some(5991)
        }
        .resolve("rad4")
        .is_err());
    }

    #[test]
    fn version_flags_require_an_exclusive_report() {
        for flag in ["--version", "-V"] {
            assert!(Cli::try_parse_from(["origami", flag]).unwrap().version);
            assert!(Cli::try_parse_from(["origami", flag, "machines"]).is_err());
        }
    }

    #[test]
    fn repeated_package_selections_are_preserved() {
        let cli = Cli::try_parse_from([
            "origami",
            "install-addon",
            "machine",
            "--name",
            "extras",
            "--source",
            "extras.iso",
            "--install",
            "extras.sw",
            "--install",
            "extras.man",
        ])
        .unwrap();
        let Some(Command::InstallAddon { install, .. }) = cli.command else {
            panic!("expected install-addon");
        };
        assert_eq!(install, ["extras.sw", "extras.man"]);
    }

    #[test]
    fn private_network_requires_an_endpoint() {
        for args in [
            vec!["origami", "network-set", "machine", "--mode", "private"],
            vec![
                "origami",
                "network-set",
                "machine",
                "--mode",
                "private",
                "--mac",
                "08:00:69:12:34:56",
            ],
        ] {
            assert!(Cli::try_parse_from(args).is_err());
        }
        for extra in [&[][..], &["--mac", "08:00:69:12:34:56"][..]] {
            let mut args = vec![
                "origami",
                "network-set",
                "machine",
                "--mode",
                "private",
                "--endpoint",
                "tcp:127.0.0.1:4242",
            ];
            args.extend(extra);
            let cli = Cli::try_parse_from(args).unwrap();
            assert!(matches!(
                cli.command,
                Some(Command::NetworkSet {
                    mode: NetworkMode::Private,
                    ..
                })
            ));
        }
    }
}
