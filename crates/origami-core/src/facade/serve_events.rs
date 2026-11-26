//! What Instigator's media server reports while it runs: JSON Lines from
//! 0.6.0 (`serve --json`), log text before that.

use super::child::OwnedChild;
use serde::Deserialize;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// The events the install flow follows; Instigator's other types and fields are ignored.
#[derive(Debug, PartialEq, Eq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum Event {
    Ready,
    Bootp {
        result: String,
    },
    Tftp {
        file: String,
        phase: String,
        size: Option<u64>,
        bytes_sent: Option<u64>,
        result: Option<String>,
    },
    Inst {
        phase: String,
        result: Option<String>,
    },
    Stopped {
        error: Option<String>,
    },
    #[serde(other)]
    Other,
}

/// The event on one line of `serve --json` output; None for a human log line
/// on the same stream, another schema version or a type with missing fields.
pub(super) fn parse_json(line: &str) -> Option<Event> {
    let value: serde_json::Value = serde_json::from_str(line.trim()).ok()?;
    (value.get("v")?.as_u64()? == 1).then_some(())?;
    serde_json::from_value(value).ok()
}

/// The event behind one log line of an Instigator before 0.6.0, such as
/// `2026-11-20T10:00:00Z INFO  tftp: 10.98.0.65:1024: sending "dist/sa" (3407872 bytes) from port 4097`.
pub(super) fn parse_text(line: &str) -> Option<Event> {
    let (_, rest) = line.trim().split_once(' ')?;
    let (level, message) = rest.trim_start().split_once(' ')?;
    let message = message.trim_start();
    if let Some(error) = message.strip_prefix("instigator: ") {
        return (level == "ERROR").then(|| Event::Stopped {
            error: Some(error.into()),
        });
    }
    if message.starts_with("serving; stop with") {
        return Some(Event::Ready);
    }
    if message.starts_with("bootp: answering ") {
        return Some(Event::Bootp {
            result: "answered".into(),
        });
    }
    // The client is `address:port`, so the first ": " ends it.
    let (_, after) = message.strip_prefix("tftp: ")?.split_once(": ")?;
    if let Some(sending) = after.strip_prefix("sending ") {
        let (file, rest) = sending.strip_prefix('"')?.split_once('"')?;
        let size = rest
            .trim()
            .strip_prefix('(')?
            .split_once(' ')?
            .0
            .parse()
            .ok()?;
        return Some(Event::Tftp {
            file: file.into(),
            phase: "start".into(),
            size: Some(size),
            bytes_sent: None,
            result: None,
        });
    }
    let (file, rest) = after.strip_prefix('"')?.split_once('"')?;
    let result = match rest.trim() {
        "done" => "ok",
        r if r.starts_with("sent; final block unacked") => "unacked_final",
        _ => return None,
    };
    Some(Event::Tftp {
        file: file.into(),
        phase: "end".into(),
        size: None,
        bytes_sent: None,
        result: Some(result.into()),
    })
}

/// Instigator's version from `instigator --version` output such as
/// `instigator version v0.6.0`.
fn parse_version(text: &str) -> Option<(u32, u32, u32)> {
    let version = text.lines().next()?.strip_prefix("instigator version ")?;
    let mut parts = version.trim().trim_start_matches('v').split('.');
    let mut number = || -> Option<u32> {
        let part = parts.next()?;
        part[..part
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(part.len())]
            .parse()
            .ok()
    };
    Some((number()?, number()?, number()?))
}

/// Whether the Instigator at `path` writes `serve --json`. One that does
/// not say its version in a few seconds is taken as older.
pub(super) fn writes_json(path: &Path) -> bool {
    let mut command = Command::new(path);
    command
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let Ok(mut child) = OwnedChild::spawn(&mut command) else {
        return false;
    };
    let end = Instant::now() + Duration::from_secs(5);
    while !matches!(child.try_wait(), Ok(Some(_))) {
        if Instant::now() >= end {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let mut text = String::new();
    if let Some(mut out) = child.child.stdout.take() {
        let _ = out.read_to_string(&mut text);
    }
    parse_version(&text).is_some_and(|version| version >= (0, 6, 0))
}

/// Complete lines as they are appended to a file.
pub(super) struct LogTail {
    path: PathBuf,
    file: Option<File>,
    partial: Vec<u8>,
}

impl LogTail {
    pub(super) fn new(path: PathBuf) -> Self {
        Self {
            path,
            file: None,
            partial: Vec::new(),
        }
    }

    /// The lines written since the last call.
    pub(super) fn lines(&mut self) -> Vec<String> {
        if self.file.is_none() {
            self.file = File::open(&self.path).ok();
        }
        if let Some(file) = &mut self.file {
            let _ = file.read_to_end(&mut self.partial);
        }
        let Some(end) = self.partial.iter().rposition(|&b| b == b'\n') else {
            return Vec::new();
        };
        let text = String::from_utf8_lossy(&self.partial[..end]).into_owned();
        self.partial.drain(..=end);
        text.lines().map(str::to_owned).collect()
    }
}

/// How the operation shows an event: a new status line and a new fraction
/// (`Some(None)` clears it).
#[derive(Debug, Default, PartialEq)]
pub(super) struct Update {
    pub(super) line: Option<String>,
    pub(super) fraction: Option<Option<f32>>,
}

/// What the install flow knows of the server so far.
pub(super) struct Status {
    ready: String,
    loading: Option<(&'static str, u64)>,
    /// The server's own account of why it failed.
    pub(super) error: Option<String>,
}

/// What a transfer carries, for the transfers the person waits on.
fn loaded(file: &str) -> Option<&'static str> {
    let name = file.rsplit('/').next().unwrap_or(file);
    if file.ends_with("dist/sa") {
        Some("the installer")
    } else if name.starts_with("fx.") {
        Some("the disk tool")
    } else {
        None
    }
}

impl Status {
    /// `ready` is the line to show once the server serves.
    pub(super) fn new(ready: String) -> Self {
        Self {
            ready,
            loading: None,
            error: None,
        }
    }

    pub(super) fn apply(&mut self, event: Event) -> Update {
        let line = |text: &str| Update {
            line: Some(text.into()),
            fraction: None,
        };
        match event {
            Event::Ready => line(&self.ready.clone()),
            Event::Bootp { result } if result == "answered" => {
                line("The machine found the install server.")
            }
            Event::Tftp {
                file,
                phase,
                size,
                bytes_sent,
                result,
            } => {
                let Some(what) = loaded(&file) else {
                    return Update::default();
                };
                match phase.as_str() {
                    "start" => {
                        self.loading = Some((what, size.unwrap_or(0)));
                        self.progress(what, 0)
                    }
                    "progress" => {
                        let total = size.unwrap_or(self.loading.map_or(0, |(_, total)| total));
                        self.loading = Some((what, total));
                        self.progress(what, bytes_sent.unwrap_or(0))
                    }
                    "end" => {
                        self.loading = None;
                        // The PROM stops reading the installer once it has what it needs.
                        let ok = matches!(
                            result.as_deref(),
                            Some("ok" | "unacked_final" | "client_stopped")
                        );
                        Update {
                            line: Some(if ok {
                                format!("Loaded {what}.")
                            } else {
                                format!("The machine could not load {what}.")
                            }),
                            fraction: Some(None),
                        }
                    }
                    _ => Update::default(),
                }
            }
            Event::Inst { phase, result } => match (phase.as_str(), result.as_deref()) {
                ("session_start", _) => line("The installer is talking to the server."),
                ("go_returned", _) => line("The installer is back at its prompt."),
                ("session_end", Some("error")) => {
                    line("The installer's connection to the server failed.")
                }
                _ => Update::default(),
            },
            Event::Stopped { error } => {
                self.error = error;
                Update::default()
            }
            _ => Update::default(),
        }
    }

    fn progress(&self, what: &str, sent: u64) -> Update {
        let total = self.loading.map_or(0, |(_, total)| total);
        if total == 0 {
            return Update {
                line: Some(format!("Loading {what}")),
                fraction: None,
            };
        }
        let done = (sent as f64 / total as f64).clamp(0.0, 1.0);
        Update {
            line: Some(format!("Loading {what}, {}%", (done * 100.0) as u32)),
            fraction: Some(Some(done as f32)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// What `instigator serve --json` writes for an Indy's first start, with a human
    /// log line among the events.
    const JSON_RUN: &str = r#"{"v":1,"time":"2026-11-24T10:00:00.000000000Z","type":"ready","server_ip":"10.98.0.2","network":"socket","sets":[{"name":"6.5.30","dist":"/6.5.30/dist"}]}
2026-11-24T10:00:00Z INFO  serving; stop with SIGINT/SIGTERM
{"v":1,"time":"2026-11-24T10:00:30.000000000Z","type":"bootp","client":"indy","mac":"08:00:69:12:34:56","ip":"10.98.0.65","file":"/6.5.30/stand/fx.64","result":"answered"}
{"v":1,"time":"2026-11-24T10:01:00.000000000Z","type":"tftp","client":"indy","file":"6.5.30/dist/sa","phase":"start","size":1000}
{"v":1,"time":"2026-11-24T10:01:01.000000000Z","type":"tftp","client":"indy","file":"6.5.30/dist/sa","phase":"progress","size":1000,"bytes_sent":420}
{"v":1,"time":"2026-11-24T10:01:02.000000000Z","type":"tftp","client":"indy","file":"6.5.30/dist/sa","phase":"end","size":1000,"bytes_sent":640,"result":"client_stopped"}
{"v":1,"time":"2026-11-24T10:01:03.000000000Z","type":"tftp","client":"indy","file":"6.5.30/dist/unix.IP22","phase":"end","result":"notfound"}
{"v":1,"time":"2026-11-24T10:02:00.000000000Z","type":"inst","client":"indy","phase":"session_start"}
{"v":1,"time":"2026-11-24T10:03:00.000000000Z","type":"future","anything":1}
{"v":1,"time":"2026-11-24T10:09:00.000000000Z","type":"stopped","reason":"guest_disconnected"}
"#;

    /// Instigator 0.5.0's log of the same start.
    const TEXT_RUN: &str = r#"2026-11-20T10:00:00Z INFO  serving; stop with SIGINT/SIGTERM
2026-11-20T10:00:30Z INFO  bootp: answering 08:00:69:12:34:56 (indy): ip=10.98.0.65 file="/6.5.30/stand/fx.64"
2026-11-20T10:01:00Z INFO  tftp: 10.98.0.65:1024: sending "6.5.30/dist/sa" (1000 bytes) from port 4097
2026-11-20T10:01:02Z INFO  tftp: 10.98.0.65:1024: "6.5.30/dist/sa" done
2026-11-20T10:09:00Z ERROR instigator: private network disconnected: EOF
"#;

    fn events(parse: fn(&str) -> Option<Event>, run: &str) -> Vec<Event> {
        run.lines().filter_map(parse).collect()
    }

    #[test]
    fn json_events_are_read_and_the_rest_is_ignored() {
        let seen = events(parse_json, JSON_RUN);
        // The log line is skipped; the unknown type is kept as `Other`.
        assert_eq!(seen.len(), 9);
        assert_eq!(seen[0], Event::Ready);
        assert_eq!(seen[7], Event::Other);
        assert_eq!(seen[8], Event::Stopped { error: None });
        assert_eq!(parse_json(r#"{"v":2,"type":"ready"}"#), None);
        assert_eq!(parse_json(r#"{"v":1,"type":"tftp"}"#), None);
    }

    #[test]
    fn the_status_follows_the_miniroot_load_and_ends_clean() {
        let mut status = Status::new("serving".into());
        let updates: Vec<Update> = events(parse_json, JSON_RUN)
            .into_iter()
            .map(|e| status.apply(e))
            .collect();
        let lines: Vec<_> = updates.iter().map(|u| u.line.as_deref()).collect();
        assert_eq!(
            lines,
            [
                Some("serving"),
                Some("The machine found the install server."),
                Some("Loading the installer, 0%"),
                Some("Loading the installer, 42%"),
                // The PROM stopping early is success, and a probe that is not found is not news.
                Some("Loaded the installer."),
                None,
                Some("The installer is talking to the server."),
                None,
                None,
            ]
        );
        assert_eq!(updates[3].fraction, Some(Some(0.42)));
        assert_eq!(updates[4].fraction, Some(None));
        assert_eq!(status.error, None);
    }

    #[test]
    fn a_refused_transfer_and_a_startup_error_are_reported() {
        let mut status = Status::new("serving".into());
        let refused = parse_json(
            r#"{"v":1,"type":"tftp","file":"6.5.30/dist/sa","phase":"end","result":"aborted"}"#,
        );
        assert_eq!(
            status.apply(refused.unwrap()).line.as_deref(),
            Some("The machine could not load the installer.")
        );
        let stopped =
            r#"{"v":1,"type":"stopped","reason":"error","error":"cannot listen: address in use"}"#;
        status.apply(parse_json(stopped).unwrap());
        assert_eq!(
            status.error.as_deref(),
            Some("cannot listen: address in use")
        );
    }

    #[test]
    fn older_log_text_gives_the_same_events_without_a_percentage() {
        let seen = events(parse_text, TEXT_RUN);
        assert_eq!(seen.len(), 5);
        let mut status = Status::new("serving".into());
        let updates: Vec<_> = seen.into_iter().map(|e| status.apply(e)).collect();
        assert_eq!(
            updates[2].line.as_deref(),
            Some("Loading the installer, 0%")
        );
        assert_eq!(updates[3].line.as_deref(), Some("Loaded the installer."));
        assert_eq!(
            status.error.as_deref(),
            Some("private network disconnected: EOF")
        );
        assert_eq!(parse_text("not a log line"), None);
    }

    #[test]
    fn the_version_decides_whether_json_is_written() {
        assert_eq!(
            parse_version("instigator version v0.6.0\n"),
            Some((0, 6, 0))
        );
        assert_eq!(parse_version("instigator version v0.5.1"), Some((0, 5, 1)));
        assert_eq!(
            parse_version("instigator version v1.10.2-3-gabc\n"),
            Some((1, 10, 2))
        );
        assert_eq!(
            parse_version("instigator version 0.0.0 (origami fake)"),
            Some((0, 0, 0))
        );
        assert_eq!(parse_version("usage: instigator"), None);
    }

    #[test]
    fn a_log_tail_returns_whole_lines_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("server.log");
        std::fs::write(&path, "one\ntw").unwrap();
        let mut tail = LogTail::new(path.clone());
        assert_eq!(tail.lines(), ["one"]);
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"o\nthree\n")
            .unwrap();
        assert_eq!(tail.lines(), ["two", "three"]);
        assert!(tail.lines().is_empty());
    }
}
