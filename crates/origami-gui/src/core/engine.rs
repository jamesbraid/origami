use super::InputEvent;
use std::error::Error;
use std::path::Path;
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

    /// The rows of `head` that changed since the last call, copied into
    /// `pixels` (cleared first), or `None` when nothing did. One consumer
    /// per head: a second caller would see only what the first left.
    fn take_frame(
        &self,
        machine: MachineId,
        head: usize,
        pixels: &mut Vec<u8>,
    ) -> Result<Option<FrameUpdate>>;

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
}
