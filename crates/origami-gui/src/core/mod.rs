//! The part of the GUI that knows nothing about windows.
//!
//! An [`Engine`] runs machines and hands out their frames; a UI draws what an
//! engine reports and sends typed input back. The machine belongs to the
//! engine, not to any view: starting it, stopping it and attaching a view
//! to it are separate acts, so a view can come and go while the guest runs.
//! Nothing in this module names Slint or wgpu, so another front end can sit on
//! the same engines.
//!
//! Two boundaries inside an engine are meant to be swapped: where frames come
//! from ([`Engine::take_frame`], which the QEMU engine answers by copying rows
//! out of a shared-memory slot) and how input travels ([`InputSink`], which
//! the QEMU engine implements over QMP). Neither is the final transport.

mod engine;
mod input;
mod sink;

pub use engine::{CursorInfo, Engine, FrameUpdate, HeadInfo, MachineId, Result, Wake};
pub use input::{InputEvent, Key, PointerButton};
pub use sink::InputSink;
