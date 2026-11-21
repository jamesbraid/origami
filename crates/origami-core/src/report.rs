//! What an operation tells the user, as data.

use origami_machine::Error;
use serde::{Deserialize, Serialize};
use std::fmt;

/// Which output a line belongs on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stream {
    Out,
    Err,
}

/// What a line is, which decides its stream and its prefix.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    /// What the operation did, such as `created DIR`.
    Result,
    /// More about the machine or the result.
    Detail,
    /// Advice the user may need to act on later.
    Advice,
    /// A finding that does not stop the operation; on the error stream.
    Warning,
    /// A change that touched only Origami's own records.
    Note,
    /// A change the guest sees at its next boot.
    GuestNote,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Line {
    pub kind: Kind,
    pub text: String,
}

impl Line {
    /// What a person is told, or none for the lines kept for the log. Every
    /// app shows this and nothing else.
    pub fn shown(&self) -> Option<String> {
        match self.kind {
            Kind::Result | Kind::Detail => None,
            Kind::Note | Kind::Advice => Some(self.text.clone()),
            Kind::GuestNote => Some(format!(
                "{}; the guest sees this at its next boot",
                self.text
            )),
            Kind::Warning => Some(format!("Warning: {}", self.text)),
        }
    }

    /// A command the line tells the user to run, for a Copy button.
    pub fn copy_text(&self) -> Option<&'static str> {
        let command = crate::change::CLEAR_LEASE;
        (self.kind == Kind::Advice && self.text.contains(command)).then_some(command)
    }

    pub fn stream(&self) -> Stream {
        match self.kind {
            Kind::Warning => Stream::Err,
            _ => Stream::Out,
        }
    }
}

impl fmt::Display for Line {
    /// The line as the command line prints it.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.kind {
            Kind::Result | Kind::Detail | Kind::Advice => f.write_str(&self.text),
            Kind::Warning => write!(f, "warning: {}", self.text),
            Kind::Note => write!(f, "note: {}", self.text),
            Kind::GuestNote => write!(
                f,
                "note: {}; the guest sees this at its next boot",
                self.text
            ),
        }
    }
}

/// The lines an operation has to say, in order.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Report {
    pub lines: Vec<Line>,
}

impl Report {
    /// A report of one line, such as `stopped` or `reset`.
    pub fn one(text: impl Into<String>) -> Self {
        let mut report = Self::default();
        report.result(text);
        report
    }

    pub fn push(&mut self, kind: Kind, text: impl Into<String>) {
        self.lines.push(Line {
            kind,
            text: text.into(),
        });
    }

    pub fn result(&mut self, text: impl Into<String>) {
        self.push(Kind::Result, text);
    }

    pub fn detail(&mut self, text: impl Into<String>) {
        self.push(Kind::Detail, text);
    }

    pub fn warning(&mut self, text: impl Into<String>) {
        self.push(Kind::Warning, text);
    }

    pub fn extend(&mut self, other: Report) {
        self.lines.extend(other.lines);
    }

    pub fn warnings(&self) -> impl Iterator<Item = &str> {
        self.of(Kind::Warning)
    }

    /// The text of the lines of `kind`.
    pub fn of(&self, kind: Kind) -> impl Iterator<Item = &str> {
        self.lines
            .iter()
            .filter(move |line| line.kind == kind)
            .map(|line| line.text.as_str())
    }

    /// Every line as `(stream, printed text)`.
    pub fn rendered(&self) -> impl Iterator<Item = (Stream, String)> + '_ {
        self.lines
            .iter()
            .map(|line| (line.stream(), line.to_string()))
    }
}

/// An operation that failed after it had something to say. The error is
/// what a caller reports; the report is what to print first.
#[derive(Debug)]
pub struct Incomplete {
    pub report: Report,
    pub error: Error,
}

impl fmt::Display for Incomplete {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.error.fmt(f)
    }
}

impl std::error::Error for Incomplete {}

/// Run `body` on a fresh report; if it fails, keep what it had said.
pub(crate) fn collect(
    body: impl FnOnce(&mut Report) -> Result<(), Error>,
) -> Result<Report, Incomplete> {
    let mut report = Report::default();
    match body(&mut report) {
        Ok(()) => Ok(report),
        Err(error) => Err(Incomplete { report, error }),
    }
}
