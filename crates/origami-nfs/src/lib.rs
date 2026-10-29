//! An NFS server for sharing a host directory with a guest: NFS versions 2
//! and 3, MOUNT versions 1 and 3 and portmap version 2, over UDP and TCP.
//!
//! This is a port of Instigator's Go server (its `nfs` and
//! `internal/sharefs` packages) and answers byte for byte as it does; the
//! RPC transcripts in Instigator's `nfs/testdata` are the contract.
//!
//! The protocol core ([`Server`]) takes a datagram or a byte stream and
//! knows nothing of host sockets, so a userspace network stack can carry
//! it. [`net`] binds it to host sockets, and [`ShareServer`] serves one
//! host directory that way.

mod mount;
mod nfs2;
mod nfs3;
mod portmap;
mod rpc;
mod xdr;

pub mod fs;
pub mod net;
pub mod sharefs;
pub mod tree;

#[cfg(test)]
mod testutil;

pub use fs::{
    Attr, CreateHow, DirEntry, Error, FileSystem, Key, Node, Result, SetAttr, Space, Time,
};
pub use net::{BoundPorts, Serving, ShareConfig, ShareServer};
pub use rpc::{LogLevel, Logger, Ports, Server};
pub use sharefs::{Options, Share};
