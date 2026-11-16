//! A host-neutral producer for display protocol version 4: the host is pulled
//! for lines at sample time, or pushes whole pictures when asked.

mod control;
mod engine;
mod flow;
mod input;
mod lines;
pub mod reference;
mod server;
mod storage;

pub use engine::{now_ns, Config, Engine, HeadSpec, Host, InputEvent};
pub use lines::{Lines, Marks, Waker};
pub use server::{Handle, Producer};
