//! The one error type front ends see. `kind` is what a program branches on;
//! `message` is a sentence safe to show.

use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    NotFound,
    Invalid,
    /// Running, starting, stopping, or an edit lock held.
    Busy,
    NotRunning,
    Unsupported,
    NeedsUpgrade,
    Io,
    Protocol,
    Engine,
    Cancelled,
    Internal,
}

/// What a person can do about an error, where the app can offer it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ErrorFix {
    #[default]
    None,
    OpenSettings,
    ChooseFirmware,
    ChooseDisk,
}

/// `message` is one plain sentence for people: no paths, flags or file names
/// of Origami's own. Everything technical goes in `detail`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CoreError {
    pub kind: ErrorKind,
    pub message: String,
    pub detail: Option<String>,
    pub fix: ErrorFix,
}

pub type CoreResult<T> = Result<T, CoreError>;

impl CoreError {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            detail: None,
            fix: ErrorFix::None,
        }
    }

    pub fn with_fix(mut self, fix: ErrorFix) -> Self {
        self.fix = fix;
        self
    }

    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    /// An error from the engine boundary, whose errors are plain text.
    pub fn engine(error: Box<dyn std::error::Error + Send + Sync>) -> Self {
        Self::new(ErrorKind::Engine, error.to_string())
    }

    /// An error from the machine library, whose errors are plain text too.
    /// The kinds are recovered from the library's own wording here, in one
    /// place, until the library sets them at the source.
    pub fn machine(error: Box<dyn std::error::Error + Send + Sync>) -> Self {
        let message = error.to_string();
        let kind = if message.starts_with("stop the machine before") {
            ErrorKind::Busy
        } else if message.contains("upgrade it once with origami upgrade")
            || message.contains("; upgrade it once")
        {
            ErrorKind::NeedsUpgrade
        } else if message.starts_with("cannot read") {
            ErrorKind::NotFound
        } else if error.downcast_ref::<std::io::Error>().is_some() {
            ErrorKind::Io
        } else {
            ErrorKind::Invalid
        };
        Self::new(kind, message)
    }
}

impl fmt::Display for CoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for CoreError {}

impl From<std::io::Error> for CoreError {
    fn from(error: std::io::Error) -> Self {
        let kind = if error.kind() == std::io::ErrorKind::NotFound {
            ErrorKind::NotFound
        } else {
            ErrorKind::Io
        };
        Self::new(kind, error.to_string())
    }
}
