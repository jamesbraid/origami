use std::any::Any;
use std::error::Error;
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

/// What a display client has moved, for a test of zero copy.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FrameStats {
    pub frames: u64,
    /// Lines handed out, copied or imported.
    pub lines: u64,
    /// Lines the GPU reads in place, a part of `lines`.
    pub imported_lines: u64,
    /// Pixel bytes the client copied out of shared memory.
    pub copied_bytes: u64,
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
