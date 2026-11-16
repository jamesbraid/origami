//! The control protocol a runner serves on its loopback control socket.
//!
//! The framing is the one iris's command socket uses: one JSON object per
//! line. A request is `{"cmd": ..., "args": {...}}`, answered by
//! `{"ok": bool, "data": ..., "error": ..., "kind": ...}`. A server may also
//! push `{"event": ...}` lines between replies. The `console` verb switches
//! the connection to raw bytes after its reply.
//!
//! [`Client`] and [`Server`] are plain `std::net` and threads; nothing here
//! needs an async runtime.

mod client;
mod frame;
mod message;
mod server;

pub use client::{Client, ClientError};
pub use frame::{write_line, FrameError, LineReader, RawStream, MAX_LINE};
pub use message::{
    BootOffsetData, ConsoleData, ErrorKind, Event, PingData, Reply, Request, Verb, PROTOCOL,
};
pub use server::{Conn, EventSender, Handler, Outcome, Server};
