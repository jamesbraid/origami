//! Following a machine's log files, with a run of identical lines kept as one.
//!
//! A failing QEMU can write the same error tens of thousands of times (one
//! viewer bug filled `runner.log` with 65,371 copies), which buries every
//! other line in any view that shows the file as it is. [`Collapsed`] keeps
//! each run of identical lines once, with a count; [`Follower`] feeds it from
//! a file as the file grows.

use std::collections::VecDeque;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// One line of a log, or a run of the same line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    pub text: String,
    /// How many times in a row it was written; 1 for a line seen once.
    pub count: u64,
}

impl Line {
    /// The line as a view shows it: the text, then the count of a repeat.
    #[cfg(test)]
    pub fn display(&self) -> String {
        if self.count > 1 {
            format!("{}  \u{d7}{}", self.text, self.count)
        } else {
            self.text.clone()
        }
    }
}

/// Lines with consecutive repeats merged, keeping the newest `max` entries.
#[derive(Clone, Debug)]
pub struct Collapsed {
    lines: VecDeque<Line>,
    max: usize,
    /// Lines pushed, before merging.
    raw: u64,
    /// Entries dropped off the front for want of room.
    dropped: u64,
    /// Times the lines were cleared.
    clears: u64,
}

impl Collapsed {
    pub fn new(max: usize) -> Self {
        Collapsed {
            lines: VecDeque::new(),
            max: max.max(1),
            raw: 0,
            dropped: 0,
            clears: 0,
        }
    }

    /// Add a line. A blank one is ignored; a line equal to the last one only
    /// raises its count.
    pub fn push(&mut self, text: &str) {
        if text.trim().is_empty() {
            return;
        }
        self.raw += 1;
        if let Some(last) = self.lines.back_mut() {
            if last.text == text {
                last.count += 1;
                return;
            }
        }
        if self.lines.len() == self.max {
            self.lines.pop_front();
            self.dropped += 1;
        }
        self.lines.push_back(Line {
            text: text.to_string(),
            count: 1,
        });
    }

    pub fn clear(&mut self) {
        self.lines.clear();
        self.raw = 0;
        self.dropped = 0;
        self.clears += 1;
    }

    /// How many times the lines have been cleared, which renumbers them.
    pub fn clears(&self) -> u64 {
        self.clears
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }

    pub fn get(&self, i: usize) -> Option<&Line> {
        self.lines.get(i)
    }

    /// The entry numbered `n` counting from the first one ever kept, which
    /// stays the same entry as older ones fall off the front.
    pub fn get_numbered(&self, n: u64) -> Option<&Line> {
        self.lines
            .get(usize::try_from(n.checked_sub(self.dropped)?).ok()?)
    }

    /// The number of the first entry held.
    pub fn first_number(&self) -> u64 {
        self.dropped
    }

    /// Lines written, counting every repeat.
    pub fn raw_lines(&self) -> u64 {
        self.raw
    }

    /// The newest entry for which `pred` holds.
    pub fn last_where(&self, pred: impl Fn(&str) -> bool) -> Option<&Line> {
        self.lines.iter().rev().find(|l| pred(&l.text))
    }
}

/// A place in a log: an entry's number (see [`Collapsed::get_numbered`]) and
/// a character of its text.
pub type LogPos = (u64, usize);

/// A drag over the text of one log, from where it began to where it is now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LogSelection {
    /// 0 for the runner log, 1 for the serial log.
    pub pane: usize,
    pub anchor: LogPos,
    pub head: LogPos,
}

impl LogSelection {
    /// The selection, first place before last, both included. None for a
    /// click that selected nothing.
    pub fn range(&self) -> Option<(LogPos, LogPos)> {
        let (a, b) = (self.anchor, self.head);
        if a == b {
            return None;
        }
        Some(if a <= b { (a, b) } else { (b, a) })
    }

    /// Whether character `col` of entry `entry` is selected.
    pub fn covers(&self, pane: usize, entry: u64, col: usize) -> bool {
        pane == self.pane
            && self
                .range()
                .is_some_and(|(a, b)| (entry, col) >= a && (entry, col) <= b)
    }

    /// Every entry held, for select all. The head is one past the last
    /// character so a log of one character is not a click.
    pub fn all(pane: usize, lines: &Collapsed) -> Option<LogSelection> {
        let last = lines.len().checked_sub(1)?;
        let first = lines.first_number();
        let end = lines.get(last)?.text.chars().count();
        Some(LogSelection {
            pane,
            anchor: (first, 0),
            head: (first + last as u64, end),
        })
    }
}

/// The selected text, a line for each entry. A run of repeats is one entry,
/// so it is copied once; with `with_counts` an entry written more than once
/// ends with ` (xN)` when its end is inside the selection.
pub fn selected_text(lines: &Collapsed, sel: &LogSelection, with_counts: bool) -> Option<String> {
    let ((e0, c0), (e1, c1)) = sel.range()?;
    let mut out: Vec<String> = Vec::new();
    for e in e0..=e1 {
        let Some(line) = lines.get_numbered(e) else {
            continue;
        };
        let chars: Vec<char> = line.text.chars().collect();
        let from = if e == e0 { c0 } else { 0 };
        let to = if e == e1 { c1 + 1 } else { usize::MAX };
        let mut text: String = chars
            .iter()
            .skip(from)
            .take(to.saturating_sub(from))
            .collect();
        if with_counts && line.count > 1 && to >= chars.len() {
            text.push_str(&format!(" (x{})", line.count));
        }
        out.push(text);
    }
    let text = out.join("\n");
    (!text.trim().is_empty()).then_some(text)
}

/// Merge the repeats in `lines`, for a caller that has them all at once.
#[cfg(test)]
pub fn collapse<'a>(lines: impl IntoIterator<Item = &'a str>) -> Vec<Line> {
    let mut c = Collapsed::new(usize::MAX);
    for l in lines {
        c.push(l);
    }
    c.lines.into_iter().collect()
}

/// Whether a log line reports a failure, for naming the cause of a machine
/// that does not come up. A guess from the words, since QEMU's messages are
/// free text.
pub fn is_error_line(text: &str) -> bool {
    let t = text.to_ascii_lowercase();
    [
        "error",
        "failed",
        "cannot",
        "can't",
        "unable to",
        "fatal",
        "panic",
        "abort",
        "invalid",
    ]
    .iter()
    .any(|w| t.contains(w))
}

/// Text a terminal would show for `s`: escape sequences and control
/// characters (other than tab) removed.
pub fn plain(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\u{1b}' => match chars.next() {
                // CSI: parameters, then a final byte in @..~.
                Some('[') => {
                    for n in chars.by_ref() {
                        if ('\u{40}'..='\u{7e}').contains(&n) {
                            break;
                        }
                    }
                }
                // OSC: up to BEL or ST (ESC \).
                Some(']') => {
                    while let Some(n) = chars.next() {
                        if n == '\u{7}' {
                            break;
                        }
                        if n == '\u{1b}' {
                            chars.next();
                            break;
                        }
                    }
                }
                // Two-byte sequences (charset selection takes a third).
                Some('(' | ')') => {
                    chars.next();
                }
                _ => {}
            },
            '\t' => out.push(' '),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

/// How much of a file's end a first read takes: a log can be gigabytes, and
/// a viewer wants what was written lately.
const FIRST_READ: u64 = 64 << 20;

/// Follows one file: each [`Follower::poll`] reads what it gained since the
/// last and folds it into [`Follower::lines`].
pub struct Follower {
    path: PathBuf,
    offset: u64,
    started: bool,
    partial: Vec<u8>,
    pub lines: Collapsed,
    /// Bytes at the start of a very large file that were not read.
    pub skipped: u64,
    /// Whether the text is passed through [`plain`], for a terminal's output.
    strip_escapes: bool,
}

impl Follower {
    pub fn new(path: PathBuf, keep: usize, strip_escapes: bool) -> Self {
        Follower {
            path,
            offset: 0,
            started: false,
            partial: Vec::new(),
            lines: Collapsed::new(keep),
            skipped: 0,
            strip_escapes,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Read up to `budget` new bytes. True when the lines changed. A file
    /// that is missing leaves everything as it was; one that shrank (a
    /// truncation) starts over.
    pub fn poll(&mut self, budget: usize) -> bool {
        let Ok(mut file) = File::open(&self.path) else {
            return false;
        };
        let Ok(len) = file.metadata().map(|m| m.len()) else {
            return false;
        };
        let mut changed = false;
        if len < self.offset {
            self.offset = 0;
            self.started = false;
            self.partial.clear();
            self.lines.clear();
            self.skipped = 0;
            changed = true;
        }
        if !self.started {
            self.started = true;
            if len > FIRST_READ {
                self.offset = len - FIRST_READ;
                self.skipped = self.offset;
                // Begin at a line start: drop up to the first newline.
                let mut skip = true;
                self.partial.clear();
                if file.seek(SeekFrom::Start(self.offset)).is_err() {
                    return changed;
                }
                let mut one = [0u8; 4096];
                while skip {
                    match file.read(&mut one) {
                        Ok(0) | Err(_) => return changed,
                        Ok(n) => {
                            self.offset += n as u64;
                            if let Some(p) = one[..n].iter().position(|&b| b == b'\n') {
                                self.offset -= (n - p - 1) as u64;
                                skip = false;
                            }
                        }
                    }
                }
            }
        }
        if len == self.offset {
            return changed;
        }
        if file.seek(SeekFrom::Start(self.offset)).is_err() {
            return changed;
        }
        let want = ((len - self.offset) as usize).min(budget.max(1));
        let mut buf = vec![0u8; want];
        let got = file.read(&mut buf).unwrap_or(0);
        buf.truncate(got);
        self.offset += got as u64;
        self.feed(&buf);
        changed || got > 0
    }

    fn feed(&mut self, bytes: &[u8]) {
        self.partial.extend_from_slice(bytes);
        while let Some(p) = self.partial.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.partial.drain(..=p).collect();
            self.push_line(&line[..line.len() - 1]);
        }
        // A line that never ends (a prompt) still shows once it is long
        // enough to matter; a log's last line is otherwise held back.
        if self.partial.len() > 4096 {
            let line = std::mem::take(&mut self.partial);
            self.push_line(&line);
        }
    }

    fn push_line(&mut self, raw: &[u8]) {
        let s = String::from_utf8_lossy(raw);
        // A carriage return inside a line rewrites it; the last piece stays.
        let s = s.trim_end_matches('\r');
        let s = s.rsplit('\r').next().unwrap_or(s);
        if self.strip_escapes {
            self.lines.push(plain(s).trim_end());
        } else {
            self.lines.push(s.trim_end());
        }
    }

    /// The unfinished last line, which a view shows after the others.
    pub fn pending(&self) -> Option<String> {
        if self.partial.is_empty() {
            return None;
        }
        let s = String::from_utf8_lossy(&self.partial);
        let s = s.rsplit('\r').next().unwrap_or(&s);
        let s = if self.strip_escapes {
            plain(s)
        } else {
            s.to_string()
        };
        (!s.trim().is_empty()).then_some(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn a_run_of_identical_lines_is_one_entry_with_its_count() {
        let mut lines = vec!["start"];
        lines.extend(std::iter::repeat_n("error: region fd not passed", 65_371));
        lines.push("done");
        let out = collapse(lines);
        assert_eq!(out.len(), 3);
        assert_eq!(out[1].count, 65_371);
        assert_eq!(out[1].display(), "error: region fd not passed  \u{d7}65371");
        assert_eq!(out[0].display(), "start");
    }

    #[test]
    fn only_consecutive_repeats_merge() {
        let out = collapse(["a", "b", "a", "a"]);
        let got: Vec<_> = out.iter().map(|l| (l.text.as_str(), l.count)).collect();
        assert_eq!(got, [("a", 1), ("b", 1), ("a", 2)]);
    }

    #[test]
    fn blank_lines_are_dropped_and_do_not_split_a_run() {
        let out = collapse(["x", "", "  ", "x"]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].count, 2);
    }

    #[test]
    fn the_oldest_entries_go_first_when_full() {
        let mut c = Collapsed::new(3);
        for l in ["a", "b", "c", "d"] {
            c.push(l);
        }
        assert_eq!(c.len(), 3);
        assert_eq!(c.get(0).unwrap().text, "b");
        assert_eq!(c.raw_lines(), 4);
    }

    #[test]
    fn the_last_error_is_the_newest_distinct_error_line() {
        let mut c = Collapsed::new(100);
        c.push("qemu: error: first");
        for _ in 0..1000 {
            c.push("qemu: failed to pass region 3");
        }
        c.push("info: nothing wrong");
        let last = c.last_where(is_error_line).unwrap();
        assert_eq!(last.text, "qemu: failed to pass region 3");
        assert_eq!(last.count, 1000);
    }

    #[test]
    fn error_words_are_found_whatever_their_case() {
        assert!(is_error_line("Cannot open display"));
        assert!(is_error_line("VK_ERROR_OUT_OF_MEMORY"));
        assert!(!is_error_line("guest reached login"));
    }

    #[test]
    fn terminal_escapes_are_stripped_for_a_log_view() {
        assert_eq!(plain("\u{1b}[1;32mlogin:\u{1b}[0m \u{7}"), "login: ");
        assert_eq!(plain("a\u{1b}]0;title\u{7}b"), "ab");
        assert_eq!(plain("x\ty"), "x y");
    }

    #[test]
    fn a_follower_reads_what_a_file_gains_and_survives_truncation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("runner.log");
        let mut f = Follower::new(path.clone(), 100, false);
        assert!(!f.poll(1 << 20), "a missing file is not a change");
        std::fs::write(&path, "one\ntwo\ntwo\ntw").unwrap();
        assert!(f.poll(1 << 20));
        assert_eq!(f.lines.len(), 2);
        assert_eq!(f.lines.get(1).unwrap().count, 2);
        assert_eq!(f.pending().as_deref(), Some("tw"));
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        file.write_all(b"o\nthree\n").unwrap();
        assert!(f.poll(1 << 20));
        assert_eq!(f.lines.len(), 3);
        assert_eq!(f.lines.get(1).unwrap().text, "two");
        assert_eq!(f.lines.get(2).unwrap().text, "three");
        assert!(!f.poll(1 << 20));
        std::fs::write(&path, "fresh\n").unwrap();
        assert!(f.poll(1 << 20));
        assert_eq!(f.lines.len(), 1);
        assert_eq!(f.lines.get(0).unwrap().text, "fresh");
    }

    #[test]
    fn a_budget_spreads_a_big_file_over_several_polls() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.log");
        let mut text = String::new();
        for _ in 0..50_000 {
            text.push_str("error: the same thing\n");
        }
        std::fs::write(&path, &text).unwrap();
        let mut f = Follower::new(path, 100, false);
        let mut polls = 0;
        while f.poll(100_000) {
            polls += 1;
            assert!(polls < 100);
        }
        assert!(polls > 1, "the budget never bit");
        assert_eq!(f.lines.len(), 1);
        assert_eq!(f.lines.get(0).unwrap().count, 50_000);
    }

    #[test]
    fn a_carriage_return_rewrites_a_line_and_crlf_ends_one() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("serial.log");
        std::fs::write(&path, "10%\r50%\r100%\r\nlogin: ").unwrap();
        let mut f = Follower::new(path, 100, true);
        f.poll(1 << 20);
        assert_eq!(f.lines.get(0).unwrap().text, "100%");
        assert_eq!(f.pending().as_deref(), Some("login: "));
    }

    #[test]
    fn a_selection_copies_the_text_of_its_entries_and_optionally_their_counts() {
        let mut c = Collapsed::new(100);
        for l in ["alpha", "beta", "beta", "beta", "gamma delta"] {
            c.push(l);
        }
        let all = LogSelection::all(1, &c).unwrap();
        assert_eq!(
            selected_text(&c, &all, false).as_deref(),
            Some("alpha\nbeta\ngamma delta")
        );
        assert_eq!(
            selected_text(&c, &all, true).as_deref(),
            Some("alpha\nbeta (x3)\ngamma delta")
        );
        // A drag from inside the first line to inside the last, either way round.
        for (anchor, head) in [((0, 2), (2, 4)), ((2, 4), (0, 2))] {
            let sel = LogSelection {
                pane: 0,
                anchor,
                head,
            };
            assert_eq!(
                selected_text(&c, &sel, true).as_deref(),
                Some("pha\nbeta (x3)\ngamma")
            );
            assert!(sel.covers(0, 1, 0));
            assert!(!sel.covers(1, 1, 0), "another pane");
            assert!(!sel.covers(0, 0, 1));
            assert!(sel.covers(0, 2, 4));
            assert!(!sel.covers(0, 2, 5));
        }
        // A cut inside the repeated line is only that text, with no count.
        let part = LogSelection {
            pane: 0,
            anchor: (1, 1),
            head: (1, 2),
        };
        assert_eq!(selected_text(&c, &part, true).as_deref(), Some("et"));
        // A click selects nothing.
        let click = LogSelection {
            pane: 0,
            anchor: (1, 1),
            head: (1, 1),
        };
        assert_eq!(selected_text(&c, &click, true), None);
        assert_eq!(LogSelection::all(0, &Collapsed::new(3)), None);
    }

    #[test]
    fn entries_keep_their_numbers_as_old_ones_fall_off() {
        let mut c = Collapsed::new(2);
        for l in ["one", "two", "three"] {
            c.push(l);
        }
        assert_eq!(c.first_number(), 1);
        assert_eq!(c.get_numbered(0), None);
        assert_eq!(c.get_numbered(2).map(|l| l.text.as_str()), Some("three"));
        // A selection that began on a line now gone copies what is left.
        let sel = LogSelection {
            pane: 0,
            anchor: (0, 0),
            head: (2, 4),
        };
        assert_eq!(
            selected_text(&c, &sel, false).as_deref(),
            Some("two\nthree")
        );
    }
}
