//! The facade front ends call: [`Core`], the machine library, [`Machine`]
//! handles, [`Operation`]s, [`Event`]s and the [`Command`] table.
//!
//! Threading: every method may be called from any thread. Fast calls
//! (`view`, `heads`, `take_frame`, input, console) are safe on a UI thread.
//! Long calls (`start`, `stop`, `create_machine`) return an [`Operation`].
//! The observer is called on a thread the core owns, with no lock held.

mod child;
mod commands;
mod console;
mod core;
mod cpu;
mod disks;
mod error;
mod event;
mod flow;
mod install;
mod labels;
mod lease;
mod machine;
mod network;
mod operation;
mod osview;
mod ready;
mod settings;

pub use commands::{Command, EnabledWhen, Platform, Shortcut};
pub use core::{
    default_offering, Config, Core, CreateRequest, EngineSummary, MachineSummary, OfferingSummary,
    SystemDisk,
};
pub use cpu::{CpuLoadReport, CpuLoadSource};
pub use disks::{disk_labels, image_verdict, size_label, DiskImageCheck, DiskLabels, ImageVerdict};
pub use error::{CoreError, CoreResult, ErrorKind};
pub use event::{Event, EventKind, Observer};
pub use flow::{
    texts, ApplyState, CheckState, CloseAction, CloseQuestion, DiskState, InstallDisk, InstallFlow,
    InstallStep,
};
pub use install::{
    install_media_folder, install_releases, GuideStep, InstallAddonChoice, InstallGuide,
    InstallMethod, InstallPlan, InstallProfile, InstallRelease, InstallSetup, InstallSetupAddon,
    MediaFolder,
};
pub use labels::{
    diagnostics_report, engine_name, memory_label, pointer_status, summary_cards, SummaryCard,
};
pub use lease::{connect as connect_install_console, Cancel, InstallStart, Starter};
pub(crate) use machine::classify_exit;
pub use machine::{
    BootConsole, ConsoleChunk, ConsoleView, ExitReason, FrameHook, HeadView, HookId, Machine,
    MachineState, MachineView, StopKind,
};
pub use network::{
    bridged_setup, host_network_modes, BridgedSetup, IdentityPreview, NetworkChoice, NetworkSetting,
};
pub use operation::{InstallFailure, Operation, OperationState};
pub use osview::{Osview, OsviewBar, OsviewKey, OsviewSegment, CAPTION as OSVIEW_CAPTION};
pub use ready::{CachedDisk, ReadyDisk};
pub use settings::{
    check_share_name, suggest_share_name, ConsolePortSetting, DriveSetting, MachineSettings,
    PipeSetting, ShareSetting,
};

pub use origami_engine::{
    Control, CpuLoad, CursorInfo, Frame, FrameStats, FrameUpdate, InputEvent, Key, PointerButton,
};

#[cfg(test)]
mod tests;
