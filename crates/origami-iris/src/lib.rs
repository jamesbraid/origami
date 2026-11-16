//! The iris engine: machines the iris emulator runs, behind the engine
//! interface.
//!
//! iris runs linked into the runner process (`origami _serve`), never in the
//! process that launches it. [`IrisEngine`] is the launching side and links no
//! iris code; the `runner` feature adds [`runner`], which builds the machine
//! in the runner and hosts it.

mod catalogue;
pub mod config;
mod engine;
pub mod nvram;
mod session;

#[cfg(feature = "runner")]
pub mod runner;

/// The engine id, and the key of this engine's data in the catalogue.
pub const ENGINE: &str = "iris";

pub use engine::IrisEngine;
