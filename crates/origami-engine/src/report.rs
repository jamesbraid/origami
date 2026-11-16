use crate::Result;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Problem {
    pub severity: Severity,
    pub message: String,
}

/// What validating a machine found.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub problems: Vec<Problem>,
}

impl Report {
    pub fn error(message: impl Into<String>) -> Self {
        let mut report = Self::default();
        report.push(Severity::Error, message);
        report
    }

    pub fn push(&mut self, severity: Severity, message: impl Into<String>) {
        self.problems.push(Problem {
            severity,
            message: message.into(),
        });
    }

    pub fn errors(&self) -> impl Iterator<Item = &Problem> {
        self.problems
            .iter()
            .filter(|problem| problem.severity == Severity::Error)
    }

    pub fn warnings(&self) -> impl Iterator<Item = &Problem> {
        self.problems
            .iter()
            .filter(|problem| problem.severity == Severity::Warning)
    }

    pub fn is_ok(&self) -> bool {
        self.errors().next().is_none()
    }

    /// Ok when there is no error, else the first error's message.
    pub fn into_result(self) -> Result<()> {
        match self.errors().next() {
            Some(problem) => Err(problem.message.clone().into()),
            None => Ok(()),
        }
    }
}
