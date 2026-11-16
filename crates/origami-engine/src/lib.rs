//! Types shared by the code that runs machines and the code that shows them:
//! a machine's heads and frames, the rows that changed, memory the GPU reads
//! in place, the guest's cursor, typed input, and why a machine has no picture.
//! Nothing here names a window system or a GPU API.

mod input;
mod sink;
pub mod status;
mod types;

pub use input::{InputEvent, Key, PointerButton};
pub use sink::InputSink;
pub use types::{
    ConsoleKind, ConsoleLink, CursorInfo, Frame, FrameUpdate, GpuMemory, HeadInfo, ImportedUpdate,
    MachineId, Power, Result, Wake,
};
