//! What a front end calls: the machine library, its operations and (later)
//! the facade with events. It depends on no engine adapter; front ends pass
//! engines in.
//!
//! Operations take the engine they run against and the canonical machine
//! directory ([`directory`]), do their work and return data: a [`Report`] of
//! what to tell the user, or the loaded machine. They print nothing.
//! Edits are [`Change`] values applied by [`apply_changes`] under the
//! machine's edit lock; new machines are [`MachineSpec`] values given to
//! [`create`].

mod change;
mod create;
mod first_boot;
mod inspect;
mod install;
mod report;
mod spec;
mod status;
mod upgrade;

pub use change::{apply_change, apply_changes, nvram_read, Change};
pub use create::create;
pub use first_boot::apply_first_boot;
pub use inspect::{check, directory, listing, open, show, validate, Loaded};
pub use install::{install_addon, install_check, install_init, InstallAddon, InstallInit};
pub use origami_machine::PortForward;
pub use report::{Incomplete, Kind, Line, Report, Stream};
pub use spec::{
    DriveKind, DriveRequest, FirmwareRequest, GraphicsRequest, IdentityRequest, MachineSpec,
    NetworkChange, NetworkMode, Protocol,
};
pub use status::Status;
pub use upgrade::upgrade;

pub mod facade;
pub mod tokens;
