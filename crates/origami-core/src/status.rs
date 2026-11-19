use crate::facade::{classify_exit, ExitReason, StopKind};
use crate::Report;
use origami_machine::control::Exit;

/// Whether a machine runs, as `origami status` reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    Running {
        pid: u32,
    },
    /// Something holds the machine's edit lock, such as a foreground run.
    Starting,
    /// With how the last run ended, when it left a note.
    Stopped(Option<Exit>),
}

impl Status {
    pub fn report(&self) -> Report {
        let mut report = Report::default();
        report.result(match self {
            Self::Running { pid } => format!("running: pid {pid}"),
            Self::Starting => "running: foreground or starting".into(),
            Self::Stopped(None) => "stopped".into(),
            Self::Stopped(Some(exit)) => {
                let requested = exit.requested.as_deref().map(|how| match how {
                    "force" => StopKind::Force,
                    _ => StopKind::Clean,
                });
                let (reason, detail) =
                    classify_exit(exit.code, exit.signal, exit.last_error.clone(), requested);
                // The apps' wording, with the plain word first for scripts.
                let with = |what: &str| match &detail {
                    Some(detail) => format!("{what}: {detail}"),
                    None => what.to_string(),
                };
                match reason {
                    ExitReason::GuestPoweredOff => "stopped (the guest powered off)".into(),
                    ExitReason::HostStopped => "stopped (stop requested)".into(),
                    ExitReason::Crashed => format!("stopped unexpectedly ({})", with("crashed")),
                    ExitReason::RunnerError => format!(
                        "stopped unexpectedly ({})",
                        with("the runner ended without saying why")
                    ),
                }
            }
        });
        report
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Kind;

    fn line(exit: Exit) -> String {
        Status::Stopped(Some(exit))
            .report()
            .of(Kind::Result)
            .next()
            .unwrap()
            .to_string()
    }

    #[test]
    fn a_stopped_machine_says_why_when_a_note_records_it() {
        let ended = |code, signal, requested: Option<&str>| Exit {
            code,
            signal,
            last_error: None,
            requested: requested.map(Into::into),
        };
        assert_eq!(
            Status::Stopped(None)
                .report()
                .of(Kind::Result)
                .next()
                .unwrap(),
            "stopped"
        );
        assert_eq!(
            line(ended(Some(0), None, None)),
            "stopped (the guest powered off)"
        );
        assert_eq!(
            line(ended(Some(0), None, Some("stop"))),
            "stopped (stop requested)"
        );
        assert_eq!(
            line(ended(None, Some(9), Some("force"))),
            "stopped (stop requested)"
        );
        assert_eq!(
            line(ended(None, Some(11), None)),
            "stopped unexpectedly (crashed: signal 11)"
        );
        assert_eq!(
            line(ended(None, None, None)),
            "stopped unexpectedly (the runner ended without saying why)"
        );
    }
}
