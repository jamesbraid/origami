use crate::Control;

/// What an engine can do, for a front end that offers only that.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Capabilities {
    pub display: DisplayKind,
    /// The most graphics heads a machine has.
    pub max_heads: u32,
    /// Whether scanout memory can be imported into the viewer's GPU.
    pub gpu_regions: bool,
    /// The controls a session accepts.
    pub controls: Vec<Control>,
    pub disk_formats: Vec<DriveFormats>,
    pub console: ConsoleTransport,
    pub identity: IdentityKind,
    /// Whether the engine can attach to a machine it did not start.
    pub attach: bool,
    /// The products whose guest audio the engine models.
    pub audio: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DisplayKind {
    /// No picture leaves the engine; a screen dump is the only view.
    None,
    /// A display v3 socket.
    Socket,
    /// Frames handed over in the process.
    InProcess,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DriveKind {
    Disk,
    Cdrom,
    Tape,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DiskFormat {
    Raw,
    Qcow2,
    Chd,
}

impl DiskFormat {
    /// The file name extension images of this format carry.
    pub fn extension(self) -> &'static str {
        match self {
            DiskFormat::Raw => "raw",
            DiskFormat::Qcow2 => "qcow2",
            DiskFormat::Chd => "chd",
        }
    }
}

/// The formats an engine reads for one kind of drive.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DriveFormats {
    pub kind: DriveKind,
    pub formats: Vec<DiskFormat>,
    /// The format a new image of this kind is made in.
    pub preferred: DiskFormat,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConsoleTransport {
    /// Raw bytes over loopback TCP.
    RawTcp,
}

/// How an engine builds a machine's serial numbers and station address
/// (`interchange.rst`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdentityKind {
    Qemu = 1,
    Iris = 2,
}
