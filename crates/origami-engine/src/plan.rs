use origami_machine::shares::Ports;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;

/// What a launch would run, as data. Callers hold it, compare it and give
/// it back to the engine that made it; only that engine reads the payload.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LaunchPlan {
    engine: String,
    dir: PathBuf,
    payload: Value,
}

impl LaunchPlan {
    pub fn new(engine: &str, dir: PathBuf, payload: Value) -> Self {
        Self {
            engine: engine.into(),
            dir,
            payload,
        }
    }

    /// The id of the engine that made the plan.
    pub fn engine(&self) -> &str {
        &self.engine
    }

    /// The machine directory it was made for.
    pub fn dir(&self) -> &std::path::Path {
        &self.dir
    }

    /// For the engine that made it.
    pub fn payload(&self) -> &Value {
        &self.payload
    }
}

/// The ports and paths the host chose for a launch.
#[derive(Clone, Debug)]
pub struct Wiring {
    pub display: DisplayRequest,
    /// The loopback port the engine serves the primary console on.
    pub console_port: u16,
    /// The loopback port of the engine's control channel (QMP for QEMU). An
    /// engine with no use for one ignores it.
    pub control_port: u16,
    /// Names the engine's process, so the host finds it again.
    pub name: String,
    /// Start with the guest halted, until a `Control::Resume`.
    pub paused: bool,
    /// The share switch's ports, when the machine's network goes through it.
    pub shares: Option<Ports>,
    /// Options for this launch only, in the engine's own words.
    pub once: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DisplayRequest {
    /// A window the engine opens.
    Window,
    Vnc {
        port: u16,
    },
    /// The display v3 socket for a viewer in another program. `gpu` is the
    /// Vulkan `deviceUUID` of the viewer's adapter.
    Shared {
        gpu: Option<[u8; 16]>,
    },
    None,
}
