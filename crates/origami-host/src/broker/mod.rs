//! The console broker: one console line shared by many clients.
//!
//! Output goes into a [`ring`] that any number of readers follow at their own
//! pace. Input goes through one mutex holding the stream, the lease holder
//! and whether the holder left a line unfinished. A lease belongs to one
//! [`Connection`], never to a token in the byte stream.
//!
//! Nothing here opens a socket or starts QEMU; the stream is anything that
//! implements [`InputSink`], so the same type serves the runner and the tests.

mod input;
mod pump;
pub mod ring;

pub use input::{
    Broker, Config, ConnId, Connection, InputError, InputSink, LeaseGrant, LeaseInfo, Notice,
    Notifier, SendOutcome, CTRL_U,
};
pub use pump::{pump, run_producer, RawPort};
pub use ring::{ReadOut, Reader, Ring, RingWriter, WaitResult};
