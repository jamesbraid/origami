//! The runner, `origami-host`: it owns a running machine, shares its console
//! among clients through a broker, and serves the control socket. `client`
//! is the other side, for the command line.

pub mod broker;
pub mod client;
mod host;
mod link;
mod runner;
mod tty;

pub use host::Host;
pub use link::{Link, LinkSink};
pub use runner::{serve, Exit, ServeArgs};
