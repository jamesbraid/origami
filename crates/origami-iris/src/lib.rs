//! The iris engine: machines the iris emulator runs, behind the engine
//! interface.
//!
//! iris runs linked into the runner process (`origami _serve`), never in the
//! process that launches it. [`IrisEngine`] is the launching side and links no
//! iris code; the `runner` feature adds [`runner`], which builds the machine
//! in the runner and hosts it.

mod catalogue;
pub mod config;
#[cfg(target_os = "linux")]
pub mod display;
mod engine;
pub mod nvram;
mod session;

#[cfg(all(feature = "runner", target_os = "linux"))]
mod keys;
#[cfg(all(feature = "runner", target_os = "linux"))]
mod publish;
#[cfg(feature = "runner")]
pub mod runner;

/// The engine id, and the key of this engine's data in the catalogue.
pub const ENGINE: &str = "iris";

pub use engine::IrisEngine;

/// Where a machine in `dir` serves its display, in a directory of its own because the
/// producer binds only in one nobody else can write; `/tmp` when the path is too long.
pub fn display_socket(dir: &std::path::Path) -> std::path::PathBuf {
    let own = dir.join("state").join("display").join("v4.sock");
    // A sockaddr_un holds 104 bytes on macOS and 108 on Linux.
    if own.as_os_str().len() <= 100 {
        return own;
    }
    std::path::PathBuf::from("/tmp")
        .join(origami_qemu::runtime::shm_name(dir))
        .join("v4.sock")
}
