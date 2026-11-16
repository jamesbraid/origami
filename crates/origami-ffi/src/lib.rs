//! Bindings for `origami-core`, in two planes.
//!
//! The control plane is UniFFI: `OrigamiCore`, `OrigamiMachine`,
//! `OrigamiOperation`, the observer, records and `CoreError`, for Swift and
//! C#. The frame plane is a small C ABI (`include/origami_frame.h`) for
//! pixels, the wake-up, input and presenting into a window, which UniFFI
//! cannot express without a copy per frame. `OrigamiMachine::frame_plane_handle`
//! joins them.

mod control;
mod engines;
mod frame;
mod keys;
mod wake;

pub use control::*;
pub use frame::*;
pub use keys::{origami_key, OrigamiKey};

uniffi::setup_scaffolding!();
