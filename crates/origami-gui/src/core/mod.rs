//! The part of the GUI that knows nothing about windows.
//!
//! Machines are run by `origami_core::facade::Core`; this module holds what
//! the views add on top: the terminal model, the log followers and the
//! engine-neutral types they share with the renderer. Nothing here names Slint
//! or wgpu, so another front end can sit on the same facade.

pub mod logs;
pub mod terminal;

pub use origami_engine::status;
pub use origami_engine::{
    ConsoleKind, ConsoleLink, CursorInfo, Frame, InputEvent, Key, PointerButton, Power,
};
