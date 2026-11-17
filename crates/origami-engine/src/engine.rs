//! The interface between the product and a machine emulator.
//!
//! An [`Engine`] knows what machines it can run and how to prepare, check,
//! plan and launch one. Launching gives a [`Session`], the live machine.
//! Engines own their threads; every method may be called from any thread.

use crate::{
    Capabilities, Catalogue, ConsoleInfo, ConsoleIo, Control, DisplayEndpoint, Facts, InputEvent,
    LaunchPlan, MachineId, Report, Result, Stop, Wake, Wiring,
};
use origami_machine::dir::MachineDir;
use origami_machine::model::Machine;
use origami_machine::stores::FirmwareSet;
use origami_machine::Offering;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// What an engine says about itself, without starting anything.
#[derive(Clone, Debug, PartialEq)]
pub struct EngineInfo {
    /// `qemu`, `iris`, `fake`: the key of the engine's data in the catalogue.
    pub id: String,
    /// The engine program's version, where it is known without running it.
    pub version: Option<String>,
    pub capabilities: Capabilities,
}

pub trait Engine: Send + Sync {
    fn info(&self) -> EngineInfo;

    /// The offerings this engine runs, with its data for each. The first
    /// call may start a program, so a caller keeps the result.
    fn catalogue(&self) -> Result<Arc<Catalogue>>;

    /// The firmware images that create `offering`'s stores: the files given,
    /// else the engine's verified downloads of the kinds it reads.
    fn firmware(
        &self,
        offering: &Offering,
        boot_prom: Option<&Path>,
        io_prom: Option<&Path>,
    ) -> Result<FirmwareSet>;

    /// Check `m` against the rules every engine shares and the engine's own.
    /// Reads the machine directory, never writes it.
    fn validate(&self, m: &Machine, dir: &MachineDir) -> Report;

    /// Refuse a create this engine cannot finish, before it downloads
    /// anything. Reads nothing of the machine.
    fn preflight_create(&self) -> Result<()> {
        Ok(())
    }

    /// Refuse engine options in `m` that would cut the product off from the
    /// machine, without the rest of [`Engine::validate`].
    fn check_options(&self, _m: &Machine) -> Result<()> {
        Ok(())
    }

    /// Write every store of `o` into the directory `state`, which this call
    /// creates, or none of them.
    fn prepare_stores(&self, o: &Offering, proms: &FirmwareSet, state: &Path) -> Result<()>;

    /// Create an empty disk of `size_mib` MiB in the engine's preferred
    /// format, at `stem` plus that format's extension, and return its path.
    /// Refuses a path that exists.
    fn create_disk(&self, stem: &Path, size_mib: u64) -> Result<PathBuf>;

    /// What a launch would run. Pure: equal inputs give equal plans, and
    /// nothing on disk or in the network is touched.
    fn plan(&self, m: &Machine, dir: &MachineDir, w: &Wiring) -> Result<LaunchPlan>;

    /// Start the machine `plan` describes and return once it answers.
    /// `wake` is called from an engine thread when the session has news,
    /// including its exit.
    fn launch(&self, plan: LaunchPlan, wake: Wake) -> Result<Arc<dyn Session>>;
}

pub trait Session: Send + Sync {
    /// The id the session passes to its `Wake`.
    fn id(&self) -> MachineId;

    fn display(&self) -> DisplayEndpoint;

    fn consoles(&self) -> Vec<ConsoleInfo>;

    /// The console `name`. Succeeds once per console and session: the host
    /// opens it and shares it among its clients.
    fn open_console(&self, name: &str) -> Result<Box<dyn ConsoleIo>>;

    fn control(&self, c: Control) -> Result<()>;

    /// Deliver `events` to the guest's keyboard and pointer, in order. An
    /// engine whose input goes another way (a display socket) refuses.
    fn send_input(&self, _events: &[InputEvent]) -> Result<()> {
        Err("this engine takes input another way".into())
    }

    /// Put `image` in the removable drive `drive`, or take the medium out.
    /// Refuses a drive that is not removable.
    fn media(&self, drive: &str, image: Option<&Path>) -> Result<()>;

    /// Save the picture of `head` to `path`.
    fn screendump(&self, head: usize, path: &Path) -> Result<()>;

    fn stop(&self, how: Stop) -> Result<()>;

    /// The session's state. A UI thread polls it, so it must not wait on the
    /// engine's program: keep it current from a thread of the session.
    fn status(&self) -> Facts;
}
