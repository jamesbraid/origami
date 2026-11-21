//! Keeps a flood of one QEMU error from filling `runner.log`: a repeated line
//! becomes a count, and a run's output stops at a cap.

use std::io::{BufRead, BufReader, Read};

/// The most bytes of QEMU output one run writes to the log.
const CAP: usize = 16 << 20;

/// The log's size above which the next run starts a fresh one.
pub(crate) const ROTATE_AT: u64 = 16 << 20;

#[derive(Default)]
pub(crate) struct Collapse {
    last: String,
    repeats: u64,
    written: usize,
    capped: bool,
}

impl Collapse {
    /// Pass `line` to `out` unless it repeats the last one or the cap is hit.
    pub(crate) fn line(&mut self, line: &str, out: &mut impl FnMut(&str)) {
        if self.repeats > 0 || !self.last.is_empty() {
            if line == self.last {
                self.repeats += 1;
                return;
            }
            self.flush(out);
        }
        self.last = line.to_owned();
        self.emit(line, out);
    }

    /// Report a pending repeat count; call at the end of the stream.
    pub(crate) fn flush(&mut self, out: &mut impl FnMut(&str)) {
        if self.repeats > 0 {
            let note = format!("(the last line repeated {} more times)", self.repeats);
            self.repeats = 0;
            self.emit(&note, out);
        }
    }

    fn emit(&mut self, line: &str, out: &mut impl FnMut(&str)) {
        if self.written + line.len() > CAP {
            if !self.capped {
                self.capped = true;
                out("(QEMU's output passed the log cap; the rest is dropped)");
            }
            return;
        }
        self.written += line.len() + 1;
        out(line);
    }
}

/// Move a log over the rotation size aside, so a new run starts a small one.
pub(crate) fn rotate(log: &std::path::Path) {
    if std::fs::metadata(log).is_ok_and(|m| m.len() > ROTATE_AT) {
        let _ = std::fs::rename(log, log.with_extension("log.1"));
    }
}

/// Call `each` with every line of `stream`, bytes that are not UTF-8 replaced,
/// so one stray byte does not end the reading.
pub(crate) fn lines_lossy(stream: impl Read, mut each: impl FnMut(String)) {
    let mut reader = BufReader::new(stream);
    let mut raw = Vec::new();
    while matches!(reader.read_until(b'\n', &mut raw), Ok(n) if n > 0) {
        let text = String::from_utf8_lossy(&raw);
        each(text.trim_end_matches(['\n', '\r']).to_owned());
        raw.clear();
    }
}

/// Copy QEMU's error output to this process's, collapsed, until it closes.
/// Returns the last non-blank line.
pub(crate) fn relay(stream: impl Read) -> Option<String> {
    let mut collapse = Collapse::default();
    let mut out = |line: &str| eprintln!("{line}");
    let mut last = None;
    lines_lossy(stream, |line| {
        collapse.line(&line, &mut out);
        if !line.trim().is_empty() {
            last = Some(line);
        }
    });
    collapse.flush(&mut out);
    last
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(lines: &[&str]) -> Vec<String> {
        let mut got = Vec::new();
        let mut out = |l: &str| got.push(l.to_owned());
        let mut collapse = Collapse::default();
        for line in lines {
            collapse.line(line, &mut out);
        }
        collapse.flush(&mut out);
        got
    }

    #[test]
    fn a_repeated_line_becomes_a_count() {
        let got = run(&["a", "boom", "boom", "boom", "b", "boom"]);
        assert_eq!(
            got,
            [
                "a",
                "boom",
                "(the last line repeated 2 more times)",
                "b",
                "boom"
            ]
        );
        assert_eq!(
            run(&["x", "x"]),
            ["x", "(the last line repeated 1 more times)"]
        );
    }

    #[test]
    fn a_line_that_is_not_utf8_does_not_end_the_reading() {
        let mut got = Vec::new();
        lines_lossy(&b"one\r\nbad \xff\xfe byte\nthree"[..], |l| got.push(l));
        assert_eq!(got, ["one", "bad \u{fffd}\u{fffd} byte", "three"]);
    }

    #[test]
    fn a_big_log_is_set_aside_and_a_small_one_kept() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("runner.log");
        std::fs::write(&log, "small").unwrap();
        rotate(&log);
        assert!(log.exists());
        std::fs::write(&log, vec![b'x'; ROTATE_AT as usize + 1]).unwrap();
        rotate(&log);
        assert!(!log.exists() && dir.path().join("runner.log.1").exists());
    }

    #[test]
    fn output_stops_at_the_cap_with_one_note() {
        let long = "y".repeat(1 << 20);
        let lines: Vec<String> = (0..40).map(|i| format!("{i}{long}")).collect();
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        let got = run(&refs);
        assert!(got.len() < 20 && got.last().unwrap().contains("log cap"));
        assert_eq!(got.iter().filter(|l| l.contains("log cap")).count(), 1);
    }
}
