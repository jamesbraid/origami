//! What to create and what to change, as plain data a front end fills in.

use origami_machine::graphics;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// A graphics pipe as `origami machines` lists them: the product, where it
/// goes, how many, and any setting other than its default.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphicsRequest {
    pub product: Option<String>,
    pub slot: Option<String>,
    pub pipes: Option<u32>,
    pub settings: BTreeMap<String, String>,
}

impl GraphicsRequest {
    pub(crate) fn request(&self) -> graphics::Request<'_> {
        graphics::Request {
            product: self.product.as_deref(),
            slot: self.slot.as_deref(),
            settings: self.settings.clone(),
            pipes: self.pipes,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdentityRequest {
    pub mac: Option<String>,
    /// 1 to 2999; without it the lowest free number.
    pub emu_id: Option<u32>,
}

/// Local firmware images instead of the verified downloads.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FirmwareRequest {
    pub boot_prom: Option<PathBuf>,
    pub io_prom: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
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

/// An existing image to attach.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DriveRequest {
    pub file: PathBuf,
    pub kind: DriveKind,
    pub target: u32,
    /// Keep a CHD disk's writes in its `.diff.chd` file.
    pub overlay: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
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

/// A machine's networking. The forwards it already has stay, and so does its
/// subnet unless one is given.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkChange {
    pub mode: NetworkMode,
    pub endpoint: Option<String>,
    pub interface: Option<String>,
    pub share_address: Option<String>,
    pub subnet: Option<String>,
    /// The machine's MAC, when it has none yet.
    pub mac: Option<String>,
}

/// A machine to create.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachineSpec {
    /// Where it goes; must not exist.
    pub dir: PathBuf,
    /// The preset (offering) id from the catalogue.
    pub preset: String,
    pub processor: Option<String>,
    /// At least this many nodes; graphics pipes may need more.
    pub nodes: Option<u32>,
    pub memory_per_node_mib: Option<u32>,
    pub graphics: GraphicsRequest,
    /// Inputs the preset takes, such as `board-id-word`.
    pub settings: BTreeMap<String, String>,
    pub identity: IdentityRequest,
    pub firmware: FirmwareRequest,
    /// Applied after creation, as [`crate::Change::SetNetwork`].
    pub network: Option<NetworkChange>,
    /// Attached after creation, as [`crate::Change::AttachDrive`].
    pub drives: Vec<DriveRequest>,
}

impl MachineSpec {
    /// A spec for `preset` in `dir` that takes every default.
    pub fn new(dir: impl Into<PathBuf>, preset: impl Into<String>) -> Self {
        Self {
            dir: dir.into(),
            preset: preset.into(),
            processor: None,
            nodes: None,
            memory_per_node_mib: None,
            graphics: GraphicsRequest::default(),
            settings: BTreeMap::new(),
            identity: IdentityRequest::default(),
            firmware: FirmwareRequest::default(),
            network: None,
            drives: vec![],
        }
    }
}
