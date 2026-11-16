use super::status::Facts;
use super::InputEvent;
use std::any::Any;
use std::error::Error;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

/// A machine an engine is running. Ids are never reused within an engine.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MachineId(pub u32);

/// What an engine reports about one display head without moving any pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HeadInfo {
    pub width: u32,
    pub height: u32,
    /// Moves on every change of the picture, so a caller that remembers the
    /// last one it drew knows whether anything is new. It never moves
    /// backwards and means nothing else.
    pub seq: u64,
    /// False while the head has no picture, as before the guest sets a mode.
    pub active: bool,
}

/// The rows of a head that changed since the last [`Engine::take_frame`] of
/// that head.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrameUpdate {
    pub seq: u64,
    pub width: u32,
    pub height: u32,
    /// The changed lines as runs `y0..y1`, in order and not overlapping. The
    /// pixel buffer holds the rows of each run one after another, tight, 4
    /// bytes a pixel (blue, green, red, unused).
    pub runs: Vec<(u32, u32)>,
    /// The head's size changed, so the picture is a new one and the caller's
    /// copy of the old one is void (and `runs` is every line).
    pub resized: bool,
}

impl FrameUpdate {
    /// Total lines in `runs`.
    pub fn lines(&self) -> u32 {
        self.runs.iter().map(|(a, b)| b - a).sum()
    }
}

/// Memory the GPU reads in place: an engine imported the machine's scanout
/// memory into the device the UI draws with, and what it hands out names it.
/// The UI downcasts it to the type its own GPU layer imports; dropping the
/// last handle releases the GPU's hold on the memory.
pub trait GpuMemory: Send + Sync {
    fn as_any(&self) -> &dyn Any;
}

/// The changed rows of a head, read where the machine wrote them. `update`
/// names the rows and carries no pixels; the UI draws them from `memory`.
#[derive(Clone)]
pub struct ImportedUpdate {
    pub update: FrameUpdate,
    pub memory: Arc<dyn GpuMemory>,
    /// Where row 0 of the plane starts in `memory`, in bytes.
    pub offset: u64,
    /// Bytes from one row of the plane to the next.
    pub stride: u32,
    /// Row 0 of the plane is the bottom line of the picture.
    pub bottom_up: bool,
}

impl std::fmt::Debug for ImportedUpdate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImportedUpdate")
            .field("update", &self.update)
            .field("offset", &self.offset)
            .field("stride", &self.stride)
            .field("bottom_up", &self.bottom_up)
            .finish_non_exhaustive()
    }
}

/// What [`Engine::take_frame`] hands out.
#[derive(Clone)]
pub enum Frame {
    /// The rows were copied out into the caller's pixel buffer.
    Rows(FrameUpdate),
    /// The rows are in memory the GPU already has; nothing was copied.
    Imported(ImportedUpdate),
}

/// The guest's cursor on one head, for a viewer that shows it as the host
/// cursor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CursorInfo {
    pub on: bool,
    /// The hot spot, in head pixels.
    pub x: i32,
    pub y: i32,
    pub hot_x: i32,
    pub hot_y: i32,
    pub width: u32,
    pub height: u32,
    /// Moves when the image or its hot spot changes.
    pub gen: u32,
    /// The image, 4 bytes a pixel as 0xAARRGGBB little-endian words, rows
    /// `width * 4` apart. Present only when `gen` differs from the one the
    /// caller already has.
    pub image: Option<Vec<u8>>,
}

/// What a machine's controls ask of its engine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Power {
    /// Start a machine that is not running, with the same display as before.
    Start,
    /// Ask the guest's machine to quit and wait until it has exited, which
    /// can take minutes while a CHD disk folds its changes into the image.
    Stop,
    /// End the machine at once, with no clean exit.
    ForceStop,
    /// Reset the guest as its reset button would.
    Reset,
}

/// Which console the PROM uses, a setting of a stopped machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConsoleKind {
    Serial,
    Graphics,
}

/// The state of the serial console's connection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConsoleLink {
    Connected,
    /// Not connected, and why.
    Down(String),
}

/// Called from an engine's own threads when any head of a machine has
/// something new. It must be cheap and never block; the caller then asks
/// for the frames on its own thread.
pub type Wake = Arc<dyn Fn(MachineId) + Send + Sync>;

/// Something that can run machines. Implemented for QEMU; iris is meant to
/// follow, which is why nothing here assumes a process or a socket.
pub trait Engine: Send + Sync {
    /// A short stable name for the engine, as a machine would select it.
    fn id(&self) -> &'static str;

    /// Where `wake` is called from when a machine has new frames.
    fn set_wake(&self, wake: Wake);

    /// Start the machine kept in `dir`, or attach to it when it is already
    /// running. The machine runs until [`Engine::stop`], whatever views come
    /// and go.
    fn start(&self, dir: &Path) -> Result<MachineId>;

    /// False once the machine has gone away on its own (the guest powered
    /// off, QEMU exited). Engines call the wake callback when it changes.
    fn is_running(&self, machine: MachineId) -> bool;

    /// Stop the machine cleanly and forget it.
    fn stop(&self, machine: MachineId) -> Result<()>;

    /// The machine's heads in order, with their sizes and sequence numbers.
    fn heads(&self, machine: MachineId) -> Result<Vec<HeadInfo>>;

    /// The rows of `head` that changed since the last call, or `None` when
    /// nothing did. When the engine has the head's memory on the GPU the rows
    /// are named and not copied ([`Frame::Imported`]); otherwise they are
    /// copied into `pixels` (cleared first). One consumer per head: a second
    /// caller would see only what the first left.
    fn take_frame(
        &self,
        machine: MachineId,
        head: usize,
        pixels: &mut Vec<u8>,
    ) -> Result<Option<Frame>>;

    /// How the machine's pictures reach the GPU, in a few words for a debug
    /// display ("vulkan import", "shared memory copy"), or None before it
    /// has any.
    fn display_backend(&self, _machine: MachineId) -> Option<String> {
        None
    }

    /// Tell the engine whether anyone is looking, so it can skip work for a
    /// machine nobody sees. Changes keep accumulating while invisible; the
    /// next [`Engine::take_frame`] returns them. Calling it again with the
    /// same value does nothing.
    fn set_visible(&self, machine: MachineId, visible: bool);

    /// The size in pixels of the area a head is shown in. Only a hint:
    /// engines that cannot render at another size ignore it.
    fn set_size(&self, _machine: MachineId, _head: usize, _width: u32, _height: u32) {}

    /// The guest cursor of `head`, or None where the engine has none. The
    /// image is returned only when it differs from `known_gen`.
    fn cursor(&self, _machine: MachineId, _head: usize, _known_gen: u32) -> Option<CursorInfo> {
        None
    }

    /// Deliver one typed input event to the guest. Never blocks on the guest.
    fn send_input(&self, machine: MachineId, event: InputEvent) -> Result<()>;

    /// The machine's directory, where its logs are.
    fn dir(&self, machine: MachineId) -> Option<PathBuf>;

    /// Where the machine's display connection stands, for naming why there
    /// is no picture. The runner log's error is not here: the caller reads
    /// that file and adds it.
    fn facts(&self, machine: MachineId) -> Facts;

    /// Lines for a debug display: the protocol's state and counters.
    fn debug(&self, _machine: MachineId) -> Vec<String> {
        Vec::new()
    }

    /// Start, stop or reset the machine. It returns when that is done, which
    /// for a stop can be minutes, so call it from a thread that can wait.
    fn power(&self, machine: MachineId, action: Power) -> Result<()>;

    /// Send bytes to the guest's serial console. Fails while the console is
    /// not connected.
    fn console_send(&self, machine: MachineId, bytes: &[u8]) -> Result<()>;

    /// Move what the console has sent since the last call onto `out`, and
    /// say whether it is connected. Output that arrives while nobody calls
    /// is kept, up to a limit.
    fn console_take(&self, machine: MachineId, out: &mut Vec<u8>) -> ConsoleLink;

    /// The console the PROM uses. Readable only while the machine is stopped.
    fn console_kind(&self, machine: MachineId) -> Result<ConsoleKind>;

    /// Choose the console the PROM uses. Only a stopped machine takes it.
    fn set_console_kind(&self, machine: MachineId, kind: ConsoleKind) -> Result<()>;
}
