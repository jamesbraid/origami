//! The facade front ends call: [`Core`], the machine library, [`Machine`]
//! handles, [`Operation`]s, [`Event`]s and the [`Command`] table.
//!
//! Threading: every method may be called from any thread. Fast calls
//! (`view`, `heads`, `take_frame`, input, console) are safe on a UI thread.
//! Long calls (`start`, `stop`, `create_machine`) return an [`Operation`].
//! The observer is called on a thread the core owns, with no lock held.

mod alerts;
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
mod shown;
mod ui_text;

pub use alerts::{
    alert, empty_state, toast, trash_name, Alert, AlertButton, AlertKind, ButtonRole, EmptyKind,
    EmptyState, Toast, ToastKind,
};
pub use commands::{
    commands_for, display_keys, shortcut_groups, Command, EnabledWhen, Platform, Shortcut,
    ShortcutGroup, ShortcutItem,
};
pub use core::{
    default_offering, Config, Core, CreateRequest, EngineSummary, MachineSummary, OfferingSummary,
    Removed, SystemDisk,
};
pub use cpu::{CpuLoadReport, CpuLoadSource};
pub use disks::{
    classify_drop, disk_labels, disk_options, drop_caption, image_verdict, ready_caption,
    size_label, DiskChoice, DiskImageCheck, DiskLabels, DiskOption, DropKind, ImageVerdict,
};
pub use error::{CoreError, CoreResult, ErrorFix, ErrorKind};
pub use event::{Event, EventKind, Observer};
pub use flow::{
    step_titles, texts, ApplyState, CheckState, CloseAction, CloseQuestion, DiskState, InstallDisk,
    InstallFlow, InstallStep, StepState, StepTitle,
};
pub use install::{
    guide_header, install_footer, install_media_folder, install_releases, waiting_for, GuideStep,
    InstallAddonChoice, InstallGuide, InstallMethod, InstallPlan, InstallProfile, InstallRelease,
    InstallSetup, InstallSetupAddon, MediaFolder, StepNote, Typed,
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
    bridged_setup, host_network_modes, BridgedSetup, IdentityField, IdentityPreview, NetworkChoice,
    NetworkSetting,
};
pub use operation::{InstallFailure, Operation, OperationState};
pub use osview::{Osview, OsviewBar, OsviewKey, OsviewSegment, CAPTION as OSVIEW_CAPTION};
pub use ready::{CachedDisk, ReadyDisk};
pub use settings::{
    check_share_name, startup_value, suggest_share_name, ConsolePortSetting, DriveRow,
    DriveSetting, MachineSettings, PipeSetting, ShareSetting,
};
pub use shown::{
    file_manager, library_line, library_line_for, machine_card, row, row_description, state_label,
    status_bar, ActionKind, Card, CardAction, LibraryLine, Row, ShownState, StateLabel, StatusBar,
    Tone,
};
pub use ui_text::{machine_id_caption, version_line, UiString};

pub use origami_engine::{
    Control, CpuLoad, CursorInfo, Frame, FrameStats, FrameUpdate, InputEvent, Key, PointerButton,
};

#[cfg(test)]
mod tests;
