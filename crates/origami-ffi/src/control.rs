//! The UniFFI control plane over the facade. Names carry an `Origami` prefix
//! so they cannot collide with platform frameworks.

use crate::engines;
use crate::keys::OrigamiKey;
use origami_core::facade as f;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

macro_rules! errors {
    ($($variant:ident => $kind:ident),* $(,)?) => {
        /// What went wrong. The variant is the kind a program branches on;
        /// `message` is a sentence safe to show, `detail` names the field or
        /// path where there is one.
        #[derive(Debug, thiserror::Error, uniffi::Error)]
        pub enum CoreError {
            $(#[error("{message}")] $variant { message: String, detail: Option<String> }),*
        }

        impl From<f::CoreError> for CoreError {
            fn from(e: f::CoreError) -> Self {
                match e.kind {
                    $(f::ErrorKind::$kind => CoreError::$variant { message: e.message, detail: e.detail }),*
                }
            }
        }

        /// The kind of an error, for a failed operation.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
        pub enum OrigamiErrorKind { $($variant),* }

        impl From<f::ErrorKind> for OrigamiErrorKind {
            fn from(k: f::ErrorKind) -> Self {
                match k { $(f::ErrorKind::$kind => OrigamiErrorKind::$variant),* }
            }
        }
    };
}

errors! {
    NotFound => NotFound,
    Invalid => Invalid,
    Busy => Busy,
    NotRunning => NotRunning,
    Unsupported => Unsupported,
    NeedsUpgrade => NeedsUpgrade,
    Io => Io,
    Protocol => Protocol,
    Engine => Engine,
    Cancelled => Cancelled,
    Internal => Internal,
}

type Res<T> = Result<T, CoreError>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum OrigamiEventKind {
    State,
    Display,
    Console,
    Operation,
    Library,
}

impl From<f::EventKind> for OrigamiEventKind {
    fn from(k: f::EventKind) -> Self {
        match k {
            f::EventKind::State => Self::State,
            f::EventKind::Display => Self::Display,
            f::EventKind::Console => Self::Console,
            f::EventKind::Operation => Self::Operation,
            f::EventKind::Library => Self::Library,
        }
    }
}

/// What changed on one machine (or, with no machine, on the core). Level
/// triggered: pull the state you care about.
#[derive(Clone, Debug, uniffi::Record)]
pub struct OrigamiEvent {
    pub machine: Option<String>,
    pub kinds: Vec<OrigamiEventKind>,
}

/// Implemented by the app. Called on a thread the core owns, never on a UI
/// thread, with no core lock held: hop to the UI thread inside it, and feel
/// free to call back into the core. Do not block.
#[uniffi::export(with_foreign)]
pub trait OrigamiObserver: Send + Sync {
    fn on_event(&self, event: OrigamiEvent);
}

struct ObserverBridge(Arc<dyn OrigamiObserver>);

impl f::Observer for ObserverBridge {
    fn on_event(&self, event: f::Event) {
        self.0.on_event(OrigamiEvent {
            machine: event.machine,
            kinds: event.kinds.into_iter().map(Into::into).collect(),
        });
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum OrigamiMachineState {
    NotRunning,
    Starting,
    Running,
    Paused,
    Stopping,
    Exited,
}

impl From<f::MachineState> for OrigamiMachineState {
    fn from(s: f::MachineState) -> Self {
        match s {
            f::MachineState::NotRunning => Self::NotRunning,
            f::MachineState::Starting => Self::Starting,
            f::MachineState::Running => Self::Running,
            f::MachineState::Paused => Self::Paused,
            f::MachineState::Stopping => Self::Stopping,
            f::MachineState::Exited => Self::Exited,
        }
    }
}

impl From<OrigamiMachineState> for f::MachineState {
    fn from(s: OrigamiMachineState) -> Self {
        match s {
            OrigamiMachineState::NotRunning => Self::NotRunning,
            OrigamiMachineState::Starting => Self::Starting,
            OrigamiMachineState::Running => Self::Running,
            OrigamiMachineState::Paused => Self::Paused,
            OrigamiMachineState::Stopping => Self::Stopping,
            OrigamiMachineState::Exited => Self::Exited,
        }
    }
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct OrigamiEngineInfo {
    pub id: String,
    pub version: Option<String>,
    pub max_heads: u32,
    pub gpu_regions: bool,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct OrigamiOffering {
    pub id: String,
    pub engine: String,
    pub product: String,
    pub topology: String,
    pub nodes: u32,
    pub cpus: u32,
    pub memory_default_mib: u32,
    pub memory_accepted_mib: Vec<u32>,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct OrigamiMachineSummary {
    pub key: String,
    pub name: String,
    pub product: String,
    pub topology: Option<String>,
    pub nodes: u32,
    pub memory_per_node_mib: u32,
    pub state: OrigamiMachineState,
    pub problem: Option<String>,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct OrigamiCreateRequest {
    pub name: String,
    pub offering: String,
    pub memory_per_node_mib: Option<u32>,
    pub boot_prom: Option<String>,
    pub io_prom: Option<String>,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct OrigamiHeadView {
    pub index: u32,
    pub width: u32,
    pub height: u32,
    pub seq: u64,
    pub active: bool,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct OrigamiConsoleView {
    pub connected: bool,
    pub down_reason: Option<String>,
    pub total: u64,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct OrigamiMachineView {
    pub key: String,
    pub name: String,
    pub product: String,
    pub engine: String,
    pub state: OrigamiMachineState,
    pub exit_code: Option<i32>,
    pub last_error: Option<String>,
    pub reason: String,
    pub heads: Vec<OrigamiHeadView>,
    pub console: OrigamiConsoleView,
    pub operation: Option<u64>,
}

impl From<f::MachineView> for OrigamiMachineView {
    fn from(v: f::MachineView) -> Self {
        Self {
            key: v.key,
            name: v.name,
            product: v.product,
            engine: v.engine,
            state: v.state.into(),
            exit_code: v.exit_code,
            last_error: v.last_error,
            reason: v.reason,
            heads: v
                .heads
                .into_iter()
                .map(|h| OrigamiHeadView {
                    index: h.index,
                    width: h.width,
                    height: h.height,
                    seq: h.seq,
                    active: h.active,
                })
                .collect(),
            console: OrigamiConsoleView {
                connected: v.console.connected,
                down_reason: v.console.down_reason,
                total: v.console.total,
            },
            operation: v.operation,
        }
    }
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct OrigamiConsoleChunk {
    pub bytes: Vec<u8>,
    pub next: u64,
    pub dropped: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum OrigamiStopKind {
    Clean,
    Force,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum OrigamiControl {
    Reset,
    PowerButton,
    Nmi,
    Pause,
    Resume,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum OrigamiPointerButton {
    Left,
    Middle,
    Right,
}

/// A pointer event. `At` is absolute, in a head's pixels; `Move` is relative
/// guest counts for a captured pointer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum OrigamiPointer {
    Move {
        dx: i32,
        dy: i32,
    },
    At {
        head: u32,
        x: i32,
        y: i32,
    },
    Button {
        button: OrigamiPointerButton,
        down: bool,
    },
    Wheel {
        notches: i32,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum OrigamiOperationState {
    Running,
    Succeeded,
    Failed {
        kind: OrigamiErrorKind,
        message: String,
    },
    Cancelled,
}

impl From<f::OperationState> for OrigamiOperationState {
    fn from(s: f::OperationState) -> Self {
        match s {
            f::OperationState::Running => Self::Running,
            f::OperationState::Succeeded => Self::Succeeded,
            f::OperationState::Failed(e) => Self::Failed {
                kind: e.kind.into(),
                message: e.message,
            },
            f::OperationState::Cancelled => Self::Cancelled,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum OrigamiPlatform {
    Macos,
    Windows,
    Linux,
}

/// When a command is available, given the selected machine's state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum OrigamiEnabledWhen {
    Always,
    MachineSelected,
    MachineStopped,
    MachineLive,
    MachineRunning,
    MachinePaused,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct OrigamiShortcut {
    pub platform: OrigamiPlatform,
    pub accelerator: String,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct OrigamiCommand {
    pub id: String,
    pub title: String,
    pub enabled_when: OrigamiEnabledWhen,
    pub shortcuts: Vec<OrigamiShortcut>,
}

impl From<f::EnabledWhen> for OrigamiEnabledWhen {
    fn from(w: f::EnabledWhen) -> Self {
        match w {
            f::EnabledWhen::Always => Self::Always,
            f::EnabledWhen::MachineSelected => Self::MachineSelected,
            f::EnabledWhen::MachineStopped => Self::MachineStopped,
            f::EnabledWhen::MachineLive => Self::MachineLive,
            f::EnabledWhen::MachineRunning => Self::MachineRunning,
            f::EnabledWhen::MachinePaused => Self::MachinePaused,
        }
    }
}

impl From<OrigamiEnabledWhen> for f::EnabledWhen {
    fn from(w: OrigamiEnabledWhen) -> Self {
        match w {
            OrigamiEnabledWhen::Always => Self::Always,
            OrigamiEnabledWhen::MachineSelected => Self::MachineSelected,
            OrigamiEnabledWhen::MachineStopped => Self::MachineStopped,
            OrigamiEnabledWhen::MachineLive => Self::MachineLive,
            OrigamiEnabledWhen::MachineRunning => Self::MachineRunning,
            OrigamiEnabledWhen::MachinePaused => Self::MachinePaused,
        }
    }
}

/// Whether a command is available when the selected machine is in `state`
/// (None for no selection).
#[uniffi::export]
pub fn origami_command_enabled(
    when: OrigamiEnabledWhen,
    state: Option<OrigamiMachineState>,
) -> bool {
    f::EnabledWhen::from(when).holds(state.map(Into::into))
}

/// The ABI version of the frame plane (`origami_frame.h`).
#[uniffi::export]
pub fn origami_frame_plane_version() -> u32 {
    crate::frame::ABI_VERSION
}

/// The core. Make one per process.
#[derive(uniffi::Object)]
pub struct OrigamiCore {
    core: f::Core,
    #[cfg(feature = "fake")]
    fake: Arc<origami_engine::fake::FakeEngine>,
}

#[uniffi::export]
impl OrigamiCore {
    /// `library_root` is the folder whose children are machine directories.
    #[uniffi::constructor]
    pub fn new(
        library_root: String,
        observer: Option<Arc<dyn OrigamiObserver>>,
    ) -> Res<Arc<OrigamiCore>> {
        let engines = engines::build()?;
        let observer = observer.map(|o| Arc::new(ObserverBridge(o)) as Arc<dyn f::Observer>);
        let core = f::Core::new(f::Config::new(library_root), engines.list, observer)?;
        Ok(Arc::new(OrigamiCore {
            core,
            #[cfg(feature = "fake")]
            fake: engines.fake,
        }))
    }

    pub fn library_root(&self) -> String {
        self.core.root().to_string_lossy().into_owned()
    }

    pub fn engines(&self) -> Vec<OrigamiEngineInfo> {
        self.core
            .engines()
            .into_iter()
            .map(|e| OrigamiEngineInfo {
                id: e.id,
                version: e.version,
                max_heads: e.capabilities.max_heads,
                gpu_regions: e.capabilities.gpu_regions,
            })
            .collect()
    }

    /// What can be created. The first call may start the engine's program:
    /// call it off the UI thread.
    pub fn catalogue(&self) -> Res<Vec<OrigamiOffering>> {
        Ok(self
            .core
            .catalogue()?
            .into_iter()
            .map(|o| OrigamiOffering {
                id: o.id,
                engine: o.engine,
                product: o.product,
                topology: o.topology,
                nodes: o.nodes,
                cpus: o.cpus,
                memory_default_mib: o.memory_default_mib,
                memory_accepted_mib: o.memory_accepted_mib,
            })
            .collect())
    }

    /// The machines in the library. Reads the disk: off the UI thread.
    pub fn list_machines(&self) -> Res<Vec<OrigamiMachineSummary>> {
        Ok(self
            .core
            .list_machines()?
            .into_iter()
            .map(|m| OrigamiMachineSummary {
                key: m.key,
                name: m.name,
                product: m.product,
                topology: m.topology,
                nodes: m.nodes,
                memory_per_node_mib: m.memory_per_node_mib,
                state: m.state.into(),
                problem: m.problem,
            })
            .collect())
    }

    /// Long. The finished operation's `result_machine` is the new key.
    pub fn create_machine(&self, request: OrigamiCreateRequest) -> Res<Arc<OrigamiOperation>> {
        let operation = self.core.create_machine(f::CreateRequest {
            name: request.name,
            offering: request.offering,
            memory_per_node_mib: request.memory_per_node_mib,
            boot_prom: request.boot_prom.map(PathBuf::from),
            io_prom: request.io_prom.map(PathBuf::from),
        })?;
        Ok(Arc::new(OrigamiOperation(operation)))
    }

    pub fn open_machine(&self, key: String) -> Res<Arc<OrigamiMachine>> {
        Ok(Arc::new(OrigamiMachine(self.core.open_machine(&key)?)))
    }

    /// Open and start, waiting until it runs. Blocks: use
    /// `OrigamiMachine::start` from a UI thread.
    pub fn start_machine(&self, key: String) -> Res<Arc<OrigamiMachine>> {
        Ok(Arc::new(OrigamiMachine(self.core.start_machine(&key)?)))
    }

    pub fn commands(&self) -> Vec<OrigamiCommand> {
        self.core
            .commands()
            .into_iter()
            .map(|c| OrigamiCommand {
                id: c.id,
                title: c.title,
                enabled_when: c.enabled_when.into(),
                shortcuts: c
                    .shortcuts
                    .into_iter()
                    .map(|s| OrigamiShortcut {
                        platform: match s.platform {
                            f::Platform::Macos => OrigamiPlatform::Macos,
                            f::Platform::Windows => OrigamiPlatform::Windows,
                            f::Platform::Linux => OrigamiPlatform::Linux,
                        },
                        accelerator: s.accelerator,
                    })
                    .collect(),
            })
            .collect()
    }

    /// Stop delivering events. Running guests keep running.
    pub fn shutdown(&self) {
        self.core.shutdown();
    }
}

/// Only in builds with the fake engine: drive a started machine.
#[cfg(feature = "fake")]
#[uniffi::export]
impl OrigamiCore {
    /// Give the started machine `key` a new picture.
    pub fn fake_advance_frame(&self, key: String) -> Res<()> {
        let machine = self.core.open_machine(&key)?;
        let session = self
            .fake
            .session_in(std::path::Path::new(machine.key()))
            .ok_or_else(|| {
                f::CoreError::new(f::ErrorKind::NotRunning, "the machine was never started")
            })?;
        session.advance_frame();
        Ok(())
    }

    /// Make the guest print `text` on its console.
    pub fn fake_say(&self, key: String, text: String) -> Res<()> {
        let machine = self.core.open_machine(&key)?;
        let session = self
            .fake
            .session_in(std::path::Path::new(machine.key()))
            .ok_or_else(|| {
                f::CoreError::new(f::ErrorKind::NotRunning, "the machine was never started")
            })?;
        session.say(&text);
        Ok(())
    }
}

#[derive(uniffi::Object)]
pub struct OrigamiMachine(pub(crate) f::Machine);

#[uniffi::export]
impl OrigamiMachine {
    pub fn key(&self) -> String {
        self.0.key().to_string()
    }

    pub fn name(&self) -> String {
        self.0.name().to_string()
    }

    pub fn view(&self) -> OrigamiMachineView {
        self.0.view().into()
    }

    /// Long: returns at once.
    pub fn start(&self) -> Res<Arc<OrigamiOperation>> {
        Ok(Arc::new(OrigamiOperation(self.0.start()?)))
    }

    /// Long: a clean stop can take minutes.
    pub fn stop(&self, how: OrigamiStopKind) -> Res<Arc<OrigamiOperation>> {
        let how = match how {
            OrigamiStopKind::Clean => f::StopKind::Clean,
            OrigamiStopKind::Force => f::StopKind::Force,
        };
        Ok(Arc::new(OrigamiOperation(self.0.stop(how)?)))
    }

    pub fn control(&self, control: OrigamiControl) -> Res<()> {
        self.0.control(match control {
            OrigamiControl::Reset => f::Control::Reset,
            OrigamiControl::PowerButton => f::Control::PowerButton,
            OrigamiControl::Nmi => f::Control::Nmi,
            OrigamiControl::Pause => f::Control::Pause,
            OrigamiControl::Resume => f::Control::Resume,
        })?;
        Ok(())
    }

    /// Put `image` in a removable drive, or eject with None.
    pub fn change_media(&self, drive: String, image: Option<String>) -> Res<()> {
        Ok(self
            .0
            .change_media(&drive, image.as_deref().map(std::path::Path::new))?)
    }

    pub fn screendump(&self, head: u32, path: String) -> Res<()> {
        Ok(self
            .0
            .screendump(head as usize, std::path::Path::new(&path))?)
    }

    pub fn send_key(&self, key: OrigamiKey, down: bool) -> Res<()> {
        Ok(self.0.send_key(key.into(), down)?)
    }

    pub fn send_pointer(&self, event: OrigamiPointer) -> Res<()> {
        let button = |b: OrigamiPointerButton| match b {
            OrigamiPointerButton::Left => f::PointerButton::Left,
            OrigamiPointerButton::Middle => f::PointerButton::Middle,
            OrigamiPointerButton::Right => f::PointerButton::Right,
        };
        let event = match event {
            OrigamiPointer::Move { dx, dy } => f::InputEvent::PointerMove { dx, dy },
            OrigamiPointer::At { head, x, y } => f::InputEvent::PointerAt { head, x, y },
            OrigamiPointer::Button { button: b, down } => f::InputEvent::Button {
                button: button(b),
                down,
            },
            OrigamiPointer::Wheel { notches } => f::InputEvent::Wheel { notches },
        };
        Ok(self.0.send_input(&[event])?)
    }

    /// Lift every key and button the guest thinks is down; call when the
    /// window loses focus. Returns how many were lifted.
    pub fn release_all_keys(&self) -> Res<u32> {
        Ok(self.0.release_all_keys()?)
    }

    /// Console output from byte `from`; pass `next` back to continue.
    pub fn console_read(&self, from: u64, max: u32) -> OrigamiConsoleChunk {
        let chunk = self.0.console_read(from, max as usize);
        OrigamiConsoleChunk {
            bytes: chunk.bytes,
            next: chunk.next,
            dropped: chunk.dropped,
        }
    }

    pub fn console_send(&self, bytes: Vec<u8>) -> Res<()> {
        Ok(self.0.console_send(&bytes)?)
    }

    /// An owned `origami_machine*` for the C frame plane, as an integer. The
    /// caller releases it with `origami_machine_release`.
    pub fn frame_plane_handle(&self) -> u64 {
        crate::frame::retain_machine(&self.0) as usize as u64
    }
}

#[derive(uniffi::Object)]
pub struct OrigamiOperation(f::Operation);

#[uniffi::export]
impl OrigamiOperation {
    pub fn id(&self) -> u64 {
        self.0.id()
    }

    pub fn title(&self) -> String {
        self.0.title().to_string()
    }

    pub fn state(&self) -> OrigamiOperationState {
        self.0.state().into()
    }

    /// Block until it finishes or `timeout_ms` passes (None waits as long as
    /// it takes). Not for a UI thread; watch for the `Operation` event.
    pub fn wait(&self, timeout_ms: Option<u64>) -> OrigamiOperationState {
        self.0.wait(timeout_ms.map(Duration::from_millis)).into()
    }

    /// False when the operation cannot be cancelled or has finished.
    pub fn cancel(&self) -> bool {
        self.0.cancel()
    }

    /// The machine a finished create or start produced.
    pub fn result_machine(&self) -> Option<String> {
        self.0.result_machine()
    }
}
