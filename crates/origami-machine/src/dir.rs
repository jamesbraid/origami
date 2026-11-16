//! A machine's directory and the places in it.

use std::path::{Path, PathBuf};

/// The directory a machine lives in: `machine.toml`, `state/`, `drives/`
/// and `logs/`. Holding one says nothing about whether it exists yet.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct MachineDir(PathBuf);

impl MachineDir {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self(path.into())
    }

    pub fn path(&self) -> &Path {
        &self.0
    }

    /// Where the machine's stores go.
    pub fn state(&self) -> PathBuf {
        self.0.join("state")
    }

    pub fn drives(&self) -> PathBuf {
        self.0.join("drives")
    }

    pub fn logs(&self) -> PathBuf {
        self.0.join("logs")
    }

    /// `value` as the machine file means it: relative to this directory
    /// unless absolute.
    pub fn resolve(&self, value: &str) -> PathBuf {
        crate::resolve(&self.0, value)
    }
}

impl AsRef<Path> for MachineDir {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}
