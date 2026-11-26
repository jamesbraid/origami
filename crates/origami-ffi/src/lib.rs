//! Bindings for `origami-core`, in two planes.
//!
//! The control plane is UniFFI: `OrigamiCore`, `OrigamiMachine`,
//! `OrigamiOperation`, the observer, records and `CoreError`, for Swift and
//! C#. The frame plane is a small C ABI (`include/origami_frame.h`) for
//! pixels, the wake-up, input and presenting into a window, which UniFFI
//! cannot express without a copy per frame. `OrigamiMachine::frame_plane_handle`
//! joins them.

macro_rules! mirror {
    ($name:ident, $core:ty, $($v:ident),*) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
        pub enum $name { $($v),* }
        impl From<$core> for $name {
            fn from(v: $core) -> Self { match v { $(<$core>::$v => Self::$v),* } }
        }
        impl From<$name> for $core {
            fn from(v: $name) -> Self { match v { $($name::$v => <$core>::$v),* } }
        }
    };
}

mod control;
#[cfg(feature = "render")]
mod cube;
mod engines;
mod flow;
mod frame;
mod keys;
mod terminal;
mod ux;
mod wake;

pub use control::*;
#[cfg(feature = "render")]
pub use cube::*;
pub use flow::*;
pub use frame::*;
pub use keys::{origami_key, OrigamiKey};
pub use terminal::*;
pub use ux::*;

uniffi::setup_scaffolding!();
