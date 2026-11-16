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
mod error;
mod event;
mod install;
mod lease;
mod machine;
mod operation;

pub use commands::{Command, EnabledWhen, Platform, Shortcut};
pub use core::{Config, Core, CreateRequest, EngineSummary, MachineSummary, OfferingSummary};
pub use error::{CoreError, CoreResult, ErrorKind};
pub use event::{Event, EventKind, Observer};
pub use lease::{connect as connect_install_console, Cancel, InstallStart, Starter};
pub use machine::{
    ConsoleChunk, ConsoleView, FrameHook, HeadView, HookId, Machine, MachineState, MachineView,
    StopKind,
};
pub use operation::{Operation, OperationState};

pub use origami_engine::{
    Control, CursorInfo, Frame, FrameStats, FrameUpdate, InputEvent, Key, PointerButton,
};

#[cfg(test)]
mod tests;
