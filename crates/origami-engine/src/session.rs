//! What a running machine shows and accepts.

use crate::Frame;
use crate::HeadInfo;
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// Where a session's pictures come from.
#[derive(Clone)]
pub enum DisplayEndpoint {
    /// No display: a headless run.
    None,
    /// A display v3 socket the viewer connects to.
    Socket(PathBuf),
    /// Frames in this process.
    Frames(Arc<dyn FrameSource>),
}

impl std::fmt::Debug for DisplayEndpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DisplayEndpoint::None => f.write_str("None"),
            DisplayEndpoint::Socket(path) => f.debug_tuple("Socket").field(path).finish(),
            DisplayEndpoint::Frames(_) => f.write_str("Frames(..)"),
        }
    }
}

/// An engine's in-process pictures. The caller owns the pixel buffer, so
/// nothing is copied twice.
pub trait FrameSource: Send + Sync {
    /// The machine's heads in order.
    fn heads(&self) -> Vec<HeadInfo>;

    /// The rows of `head` that changed since the last call, or None when
    /// nothing did. Copied rows go into `pixels` (cleared first). One
    /// consumer per head.
    fn take_frame(&self, head: usize, pixels: &mut Vec<u8>) -> Option<Frame>;

    /// The guest's cursor on `head`; the image only when `known_gen` differs.
    /// None for an engine that draws the cursor into the picture.
    fn cursor(&self, _head: usize, _known_gen: u32) -> Option<crate::CursorInfo> {
        None
    }

    /// Whether anyone is looking. A source that is told asks its engine for
    /// no wake-ups while hidden and one when shown again if it changed.
    fn set_visible(&self, _visible: bool) {}

    /// Why the source has no picture, for a status line; None when it has one
    /// or cannot say.
    fn why_blank(&self) -> Option<String> {
        None
    }

    /// What the source has moved so far, if it counts.
    fn stats(&self) -> Option<crate::FrameStats> {
        None
    }
}

/// A serial line of the machine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConsoleInfo {
    pub name: String,
    /// The line the guest's console is.
    pub primary: bool,
}

/// A console's bytes, both ways.
pub trait ConsoleIo: Read + Write + Send {
    /// Separate the two directions, so one thread can block reading while
    /// another writes.
    fn split(self: Box<Self>) -> io::Result<(Box<dyn Read + Send>, Box<dyn Write + Send>)>;
}

impl ConsoleIo for std::net::TcpStream {
    fn split(self: Box<Self>) -> io::Result<(Box<dyn Read + Send>, Box<dyn Write + Send>)> {
        let reader = self.try_clone()?;
        Ok((Box::new(reader), self))
    }
}

/// What the host asks of a running guest.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Control {
    Reset,
    PowerButton,
    Nmi,
    Pause,
    Resume,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stop {
    /// Ask the guest's machine to quit and wait for the exit, which can take
    /// minutes while a CHD disk folds its changes. `None` waits as long as it
    /// takes; on a timeout the stop is reported failed and nothing is killed.
    Clean { timeout: Option<Duration> },
    /// End the machine at once.
    Force,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunState {
    /// Created, not yet running: started paused.
    Prelaunch,
    Running,
    Paused,
    /// The engine's program lives and did not say what state it is in.
    Unknown(String),
    /// The engine's program is gone.
    Exited {
        /// None when a signal ended it.
        code: Option<i32>,
        /// The signal that ended it, where the platform says.
        signal: Option<i32>,
        /// The last line it wrote to its error output, when it failed.
        last_error: Option<String>,
    },
}

/// What a session says about itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Facts {
    pub state: RunState,
    /// The engine program's process id, while it runs.
    pub pid: Option<u32>,
}
