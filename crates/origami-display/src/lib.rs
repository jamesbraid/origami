//! A client of the display protocol, version 3: a machine's pictures arrive
//! as shared memory named by messages on a local socket, and typed input goes
//! back on the same socket. Any engine that speaks the protocol can use it.
//!
//! The boundary is small on purpose, so a reader for a later protocol version
//! can sit behind the same shape: the transport that connects and runs the
//! link, region mapping, the frame and damage source ([`Display`]), the
//! cursor, and the input sender. The wire types are in [`proto`].
//! Nothing here names a window system or a GPU API.

mod control;
mod display;
mod import;
mod input;
pub mod proto;
mod region;
mod socket_input;
mod transport;

pub use display::{Display, Stats, Taken};
pub use import::Importer;
pub use region::{Handle, Memory};
pub use socket_input::SocketInput;
pub use transport::{connect_socket, run, Conn, Exit, Link, Timing, Uplink};
