//! The install operations against a runner over the fake runtime. They
//! start many processes at once, so they live apart from the tests of
//! `tests/core`, which take the machine edit lock in this process: a child
//! forked while the lock is held keeps it until it execs.

#[path = "../support/mod.rs"]
mod support;

mod install;

use origami_qemu::{QemuEngine, Runtime};
use std::path::Path;
use std::sync::OnceLock;

/// One runtime directory for the file; each test works in its own folder.
fn root() -> &'static Path {
    static ROOT: OnceLock<tempfile::TempDir> = OnceLock::new();
    let root = ROOT.get_or_init(|| {
        let root = tempfile::tempdir().unwrap();
        std::env::set_var("ORIGAMI_RUNTIME_DIR", support::fake_runtime(root.path()));
        std::env::set_var("ORIGAMI_CACHE_DIR", root.path().join("cache"));
        root
    });
    root.path()
}

fn engine() -> QemuEngine {
    QemuEngine::new(Runtime::locate().unwrap())
}
