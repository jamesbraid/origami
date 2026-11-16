use crate::Report;

/// Whether a machine runs, as `origami status` reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Running {
        pid: u32,
    },
    /// Something holds the machine's edit lock, such as a foreground run.
    Starting,
    Stopped,
}

impl Status {
    pub fn report(self) -> Report {
        let mut report = Report::default();
        report.result(match self {
            Self::Running { pid } => format!("running: pid {pid}"),
            Self::Starting => "running: foreground or starting".into(),
            Self::Stopped => "stopped".into(),
        });
        report
    }
}
