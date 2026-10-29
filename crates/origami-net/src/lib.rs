//! A host on a guest's Ethernet network that serves an NFS share.
//!
//! QEMU's `stream` netdev carries Ethernet frames over a socket, each
//! with a 4-byte big-endian length in front. A [`Switch`] listens for two
//! such clients: the guest's NIC (A) and the rest of the machine's network
//! (B, a hub port beside slirp, a tap device or the private install
//! network). The switch sends the guest's frames either to the userspace
//! stack, to B, or to both, and everything from B and from the stack to
//! the guest. The stack ([`Stack`]) owns one address on the network and
//! serves portmap, MOUNT and NFS from an `origami-nfs` [`Server`] on UDP
//! and TCP ports 111, 635 and 2049.
//!
//! [`Server`]: origami_nfs::Server

mod frame;
mod stack;
mod switch;

pub use frame::{route, Route};
pub use stack::{Stack, StackConfig};
pub use switch::{Switch, SwitchConfig};

/// The ports portmap, MOUNT and NFS answer on.
pub const PORTS: [u16; 3] = [111, 635, 2049];
