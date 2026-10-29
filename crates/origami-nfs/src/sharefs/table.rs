//! The path table: numbers the paths of a share for filehandles on a host
//! without stable inode numbers. It lives in a file of lines, each
//! `set ID PATH` with PATH quoted as Go quotes strings, or `del ID`.
//! A `next ID` line, written when the table is compacted, keeps ids from
//! ID up from being handed out again: an id freed by a removal is never
//! reused, so a handle to a removed file cannot reach a later one. Opening
//! the table replays the file and rewrites it compacted; changes after
//! that are appended, so the numbering survives a restart.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::Path;
use std::sync::Mutex;

pub(super) struct PathTable {
    inner: Mutex<Inner>,
}

struct Inner {
    f: File,
    maps: Maps,
}

#[derive(Default)]
struct Maps {
    next: u64,
    by_id: HashMap<u64, Vec<u8>>,
    by_path: HashMap<Vec<u8>, u64>,
}

/// Whether `p` is `base` or inside it.
pub(super) fn under(p: &[u8], base: &[u8]) -> bool {
    p == base || (p.len() > base.len() && p.starts_with(base) && p[base.len()] == b'/')
}

fn bad(file: &Path, line: usize, what: impl std::fmt::Display) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("{}:{}: {}", file.display(), line, what),
    )
}

impl Maps {
    fn set(&mut self, id: u64, p: Vec<u8>) {
        if let Some(old) = self.by_id.get(&id) {
            self.by_path.remove(old);
        }
        self.by_path.insert(p.clone(), id);
        self.by_id.insert(id, p);
        self.next = self.next.max(id + 1);
    }

    fn del(&mut self, id: u64) {
        if let Some(p) = self.by_id.remove(&id) {
            self.by_path.remove(&p);
        }
    }
}

impl PathTable {
    pub fn open(file: &Path) -> io::Result<PathTable> {
        let maps = load(file)?;
        // Rewrite the replayed table beside the old one and swap it in.
        let mut tmp = file.as_os_str().to_owned();
        tmp.push(".new");
        let tmp = std::path::PathBuf::from(tmp);
        let mut ids: Vec<u64> = maps.by_id.keys().copied().collect();
        ids.sort_unstable();
        let mut text = format!("next {}\n", maps.next).into_bytes();
        for id in ids {
            text.extend_from_slice(&set_line(id, &maps.by_id[&id]));
        }
        let written = fs::write(&tmp, &text).and_then(|_| fs::rename(&tmp, file));
        if let Err(e) = written {
            let _ = fs::remove_file(&tmp);
            return Err(e);
        }
        let f = OpenOptions::new().append(true).open(file)?;
        Ok(PathTable {
            inner: Mutex::new(Inner { f, maps }),
        })
    }

    /// Returns `p`'s id, numbering `p` if the table hasn't seen it.
    pub fn id_for(&self, p: &[u8]) -> io::Result<u64> {
        let mut t = self.inner.lock().unwrap();
        if let Some(&id) = t.maps.by_path.get(p) {
            return Ok(id);
        }
        let id = t.maps.next;
        t.f.write_all(&set_line(id, p))?;
        t.maps.set(id, p.to_vec());
        Ok(id)
    }

    pub fn path(&self, id: u64) -> Option<Vec<u8>> {
        self.inner.lock().unwrap().maps.by_id.get(&id).cloned()
    }

    /// Forgets `p` and everything under it.
    pub fn remove(&self, p: &[u8]) {
        let mut t = self.inner.lock().unwrap();
        let mut ids: Vec<u64> = t
            .maps
            .by_id
            .iter()
            .filter(|(_, q)| under(q, p))
            .map(|(&id, _)| id)
            .collect();
        ids.sort_unstable();
        for id in ids {
            // A failed append is not fatal: the table in memory stays
            // right for this run.
            let _ = t.f.write_all(format!("del {id}\n").as_bytes());
            t.maps.del(id);
        }
    }

    /// Renames `p`, and everything under it, to `q`, keeping their ids.
    pub fn rename(&self, p: &[u8], q: &[u8]) {
        let mut t = self.inner.lock().unwrap();
        let mut moves: Vec<(u64, Vec<u8>)> = t
            .maps
            .by_id
            .iter()
            .filter(|(_, old)| under(old, p))
            .map(|(&id, old)| {
                let mut np = q.to_vec();
                np.extend_from_slice(&old[p.len()..]);
                (id, np)
            })
            .collect();
        moves.sort_unstable();
        for (id, np) in moves {
            let _ = t.f.write_all(&set_line(id, &np));
            t.maps.set(id, np);
        }
    }
}

fn set_line(id: u64, p: &[u8]) -> Vec<u8> {
    let mut line = format!("set {id} ").into_bytes();
    line.extend_from_slice(&quote(p));
    line.push(b'\n');
    line
}

/// Replays the table file, which may not exist yet.
fn load(file: &Path) -> io::Result<Maps> {
    let mut maps = Maps {
        next: 1,
        ..Maps::default()
    };
    if let Some(dir) = file.parent().filter(|d| !d.as_os_str().is_empty()) {
        let mut b = fs::DirBuilder::new();
        b.recursive(true);
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut b, 0o700);
        b.create(dir)?;
    }
    let data = match fs::read(file) {
        Ok(d) => d,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(maps),
        Err(e) => return Err(e),
    };
    let mut lines: Vec<&[u8]> = data.split(|&b| b == b'\n').collect();
    if lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    for (i, line) in lines.iter().enumerate() {
        let n = i + 1;
        let mut f = line.splitn(3, |&b| b == b' ');
        let (Some(op), Some(id)) = (f.next(), f.next()) else {
            return Err(bad(file, n, "malformed line"));
        };
        let id: u64 = std::str::from_utf8(id)
            .ok()
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| bad(file, n, "invalid id"))?;
        match (op, f.next()) {
            (b"set", Some(q)) => {
                let p = unquote(q).ok_or_else(|| bad(file, n, "invalid quoted path"))?;
                maps.set(id, p);
            }
            (b"del", _) => maps.del(id),
            (b"next", None) => maps.next = maps.next.max(id),
            _ => return Err(bad(file, n, "malformed line")),
        }
    }
    Ok(maps)
}

/// Quotes `s` as Go's strconv.Quote does: backslash escapes for control
/// characters, `\x` for bytes that aren't UTF-8, `\u` and `\U` for
/// characters that aren't printable. Whether a non-ASCII character counts
/// as printable is approximated (Go consults its Unicode tables), which
/// can change how an unusual name is written but never what it reads back
/// as, since [`unquote`] takes every form.
pub(super) fn quote(s: &[u8]) -> Vec<u8> {
    let mut out = vec![b'"'];
    let mut rest = s;
    while !rest.is_empty() {
        let (ch, width) = decode(rest);
        match ch {
            None => {
                out.extend_from_slice(format!("\\x{:02x}", rest[0]).as_bytes());
            }
            Some(c) => push_quoted(&mut out, c),
        }
        rest = &rest[width..];
    }
    out.push(b'"');
    out
}

/// Decodes the first character of `s`, or reports the first byte as
/// invalid with width 1.
fn decode(s: &[u8]) -> (Option<char>, usize) {
    let len = match s[0] {
        0x00..=0x7f => 1,
        0xc2..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf4 => 4,
        _ => return (None, 1),
    };
    if s.len() < len {
        return (None, 1);
    }
    match std::str::from_utf8(&s[..len]) {
        Ok(t) => (t.chars().next(), len),
        Err(_) => (None, 1),
    }
}

fn is_print(c: char) -> bool {
    if c.is_ascii() {
        return (' '..='~').contains(&c);
    }
    let u = c as u32;
    !(c.is_control()
        || c.is_whitespace()
        || matches!(u, 0xad | 0x34f | 0x61c | 0x115f..=0x1160 | 0x17b4..=0x17b5 | 0x180b..=0x180f
            | 0x200b..=0x200f | 0x2028..=0x202e | 0x2060..=0x206f | 0x3164 | 0xfe00..=0xfe0f
            | 0xfeff | 0xffa0 | 0xfff0..=0xffff | 0xe000..=0xf8ff | 0xf0000..=0x10ffff))
}

fn push_quoted(out: &mut Vec<u8>, c: char) {
    if c == '"' || c == '\\' {
        out.push(b'\\');
        out.push(c as u8);
    } else if is_print(c) {
        let mut b = [0u8; 4];
        out.extend_from_slice(c.encode_utf8(&mut b).as_bytes());
    } else {
        match c {
            '\u{7}' => out.extend_from_slice(b"\\a"),
            '\u{8}' => out.extend_from_slice(b"\\b"),
            '\u{c}' => out.extend_from_slice(b"\\f"),
            '\n' => out.extend_from_slice(b"\\n"),
            '\r' => out.extend_from_slice(b"\\r"),
            '\t' => out.extend_from_slice(b"\\t"),
            '\u{b}' => out.extend_from_slice(b"\\v"),
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                out.extend_from_slice(format!("\\x{:02x}", c as u32).as_bytes())
            }
            c if (c as u32) < 0x10000 => {
                out.extend_from_slice(format!("\\u{:04x}", c as u32).as_bytes())
            }
            c => out.extend_from_slice(format!("\\U{:08x}", c as u32).as_bytes()),
        }
    }
}

/// Undoes [`quote`]: a double-quoted string in Go syntax. None if `q`
/// isn't one.
pub(super) fn unquote(q: &[u8]) -> Option<Vec<u8>> {
    if q.len() < 2 || q[0] != b'"' || q[q.len() - 1] != b'"' {
        return None;
    }
    let s = &q[1..q.len() - 1];
    let mut out = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        match s[i] {
            b'"' | b'\n' => return None,
            b'\\' => {
                i += 1;
                let e = *s.get(i)?;
                i += 1;
                match e {
                    b'a' => out.push(7),
                    b'b' => out.push(8),
                    b'f' => out.push(12),
                    b'n' => out.push(b'\n'),
                    b'r' => out.push(b'\r'),
                    b't' => out.push(b'\t'),
                    b'v' => out.push(11),
                    b'\\' => out.push(b'\\'),
                    b'"' => out.push(b'"'),
                    b'x' => {
                        out.push(hex(s.get(i..i + 2)?)? as u8);
                        i += 2;
                    }
                    b'0'..=b'7' => {
                        let d = s.get(i..i + 2)?;
                        let v = (e - b'0') as u32 * 64 + oct(d[0])? * 8 + oct(d[1])?;
                        if v > 255 {
                            return None;
                        }
                        out.push(v as u8);
                        i += 2;
                    }
                    b'u' | b'U' => {
                        let n = if e == b'u' { 4 } else { 8 };
                        let c = char::from_u32(hex(s.get(i..i + n)?)?)?;
                        let mut b = [0u8; 4];
                        out.extend_from_slice(c.encode_utf8(&mut b).as_bytes());
                        i += n;
                    }
                    _ => return None,
                }
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    Some(out)
}

fn hex(d: &[u8]) -> Option<u32> {
    let mut v: u32 = 0;
    for &b in d {
        v = v.checked_mul(16)?.checked_add((b as char).to_digit(16)?)?;
    }
    Some(v)
}

fn oct(b: u8) -> Option<u32> {
    (b as char).to_digit(8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quote_matches_go() {
        // Strings and their strconv.Quote forms.
        for (raw, want) in [
            (&b"plain"[..], &br#""plain""#[..]),
            (b"c d\n", br#""c d\n""#),
            (b"tab\there", br#""tab\there""#),
            (b"q\"b\\", br#""q\"b\\""#),
            (b"\x00\x1f\x7f", br#""\x00\x1f\x7f""#),
            (b"\xff\xfe", br#""\xff\xfe""#),
            ("caf\u{e9}".as_bytes(), "\"caf\u{e9}\"".as_bytes()),
            ("\u{200b}".as_bytes(), br#""\u200b""#),
            ("\u{1f4a9}".as_bytes(), "\"\u{1f4a9}\"".as_bytes()),
        ] {
            assert_eq!(
                quote(raw),
                want,
                "quote({:?})",
                String::from_utf8_lossy(raw)
            );
            assert_eq!(unquote(want).unwrap(), raw);
        }
    }

    #[test]
    fn unquote_takes_every_go_escape() {
        assert_eq!(
            unquote(br#""\a\b\f\n\r\t\v\\\"""#).unwrap(),
            b"\x07\x08\x0c\n\r\t\x0b\\\""
        );
        assert_eq!(
            unquote(br#""\x41\101\u00e9\U0001f4a9""#).unwrap(),
            "AA\u{e9}\u{1f4a9}".as_bytes()
        );
        for bad in [
            &br#"a"#[..],
            br#""a"#,
            br#""a"b""#,
            br#""\q""#,
            br#""\400""#,
            br#""\x4""#,
            br#""\ud800""#,
        ] {
            assert!(unquote(bad).is_none(), "{:?}", String::from_utf8_lossy(bad));
        }
    }
}
