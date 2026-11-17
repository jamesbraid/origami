//! Types shared by the code that runs machines and the code that shows them:
//! the [`Engine`] and [`Session`] interfaces, a machine's heads and frames,
//! the rows that changed, memory the GPU reads in place, the guest's cursor,
//! typed input, and why a machine has no picture.
//! Nothing here names a window system or a GPU API.

mod caps;
mod engine;
mod input;
mod plan;
mod report;
mod session;
mod sink;
pub mod status;
mod stores;
mod types;

#[cfg(feature = "fake")]
pub mod contract;
#[cfg(feature = "fake")]
pub mod fake;

pub use caps::{
    Capabilities, ConsoleTransport, DiskFormat, DisplayKind, DriveFormats, DriveKind, IdentityKind,
};
pub use engine::{Engine, EngineInfo, Session};
pub use input::{InputEvent, Key, PointerButton};
pub use plan::{DisplayRequest, LaunchPlan, Wiring};
pub use report::{Problem, Report, Severity};
pub use session::{
    ConsoleInfo, ConsoleIo, Control, DisplayEndpoint, Facts, FrameSource, Polled, RunState, Stop,
};
pub use sink::InputSink;
pub use stores::EngineStores;
pub use types::{
    ConsoleKind, ConsoleLink, CursorInfo, Frame, FrameStats, FrameUpdate, GpuMemory, HeadInfo,
    ImportedUpdate, MachineId, Power, Result, Wake,
};

/// The offerings an engine runs, with its data for each.
pub type Catalogue = origami_machine::Machines;
