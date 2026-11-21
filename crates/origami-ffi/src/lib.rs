//! Bindings for `origami-core`, in two planes.
//!
//! The control plane is UniFFI: `OrigamiCore`, `OrigamiMachine`,
//! `OrigamiOperation`, the observer, records and `CoreError`, for Swift and
//! C#. The frame plane is a small C ABI (`include/origami_frame.h`) for
//! pixels, the wake-up, input and presenting into a window, which UniFFI
//! cannot express without a copy per frame. `OrigamiMachine::frame_plane_handle`
//! joins them.

mod control;
#[cfg(feature = "render")]
mod cube;
mod engines;
mod flow;
mod frame;
mod keys;
mod terminal;
mod wake;

pub use control::*;
#[cfg(feature = "render")]
pub use cube::*;
pub use flow::*;
pub use frame::*;
pub use keys::{origami_key, OrigamiKey};
pub use terminal::*;

uniffi::setup_scaffolding!();
