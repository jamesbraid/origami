//! A read-only tree held in memory, served the way install media are:
//! nodes numbered by position, handles carrying the number. It exists so
//! the protocol code can be tested against a known tree.

use std::collections::{BTreeMap, HashMap};

use crate::fs::{Attr, DirEntry, Error, FileSystem, Key, Node, Result, Time};

/// What a tree entry holds.
#[derive(Clone, Debug)]
pub enum Content {
    Dir,
    File(Vec<u8>),
    /// A symlink and its target.
    Link(Vec<u8>),
}

/// One entry of a [`MemTree`]. `path` is relative to the root, `.` for the
/// root itself.
#[derive(Clone, Debug)]
pub struct TreeEntry {
    pub path: String,
    /// Permission bits, without the type.
    pub mode: u32,
    pub mtime: i64,
    pub content: Content,
}

struct Rec {
    path: String,
    id: u64,
    mode: u32,
    mtime: u32,
    content: Content,
}

/// A read-only filesystem over entries held in memory. Nodes are numbered
/// from 1 in depth-first order, visiting directory entries sorted by name,
/// so `.` is 1.
pub struct MemTree {
    by_path: HashMap<String, Rec>,
    by_id: HashMap<u64, String>,
    children: HashMap<String, Vec<String>>,
}

fn dir_of(p: &str) -> &str {
    match p.rfind('/') {
        Some(i) => &p[..i],
        None => ".",
    }
}

impl MemTree {
    pub fn new(entries: impl IntoIterator<Item = TreeEntry>) -> MemTree {
        let mut raw: BTreeMap<String, TreeEntry> = BTreeMap::new();
        for e in entries {
            raw.insert(e.path.clone(), e);
        }
        // A parent nobody listed is a directory with no permissions to
        // speak of, as io/fs's MapFS makes it.
        let mut missing = Vec::new();
        for p in raw.keys() {
            let mut d = dir_of(p);
            while d != "." && !raw.contains_key(d) {
                missing.push(d.to_string());
                d = dir_of(d);
            }
        }
        for d in missing.into_iter().chain([".".to_string()]) {
            raw.entry(d.clone()).or_insert(TreeEntry {
                path: d,
                mode: 0o555,
                mtime: 0,
                content: Content::Dir,
            });
        }
        let mut kids: HashMap<String, Vec<String>> = HashMap::new();
        for p in raw.keys().filter(|p| p.as_str() != ".") {
            kids.entry(dir_of(p).to_string())
                .or_default()
                .push(p.clone());
        }
        let name = |p: &str| p.rsplit('/').next().unwrap().to_string();
        for v in kids.values_mut() {
            v.sort_by_key(|p| name(p));
        }

        let mut t = MemTree {
            by_path: HashMap::new(),
            by_id: HashMap::new(),
            children: HashMap::new(),
        };
        let mut next = 1u64;
        let mut stack = vec![".".to_string()];
        while let Some(p) = stack.pop() {
            let e = raw.remove(&p).unwrap();
            t.by_id.insert(next, p.clone());
            t.by_path.insert(
                p.clone(),
                Rec {
                    path: p.clone(),
                    id: next,
                    mode: e.mode & 0o7777,
                    mtime: e.mtime as u32,
                    content: e.content,
                },
            );
            next += 1;
            if let Some(k) = kids.remove(&p) {
                stack.extend(k.iter().rev().cloned());
                t.children.insert(p, k);
            }
        }
        t
    }

    fn node(&self, rec: &Rec) -> Node {
        let (type_bits, size) = match &rec.content {
            Content::Dir => (0o040000, 0),
            Content::Link(t) => (0o120000, t.len() as u64),
            Content::File(d) => (0o100000, d.len() as u64),
        };
        let mode = type_bits | rec.mode;
        let t = Time::new(rec.mtime as i64, 0);
        Node {
            attr: Attr {
                mode,
                nlink: 1,
                size,
                used: (size + 511) & !511,
                fileid: rec.id,
                atime: t,
                mtime: t,
                ctime: t,
            },
            path: rec.path.clone().into_bytes(),
            raw_mode: mode,
            key: Key {
                ino: rec.id,
                ..Key::default()
            },
            describe: None,
            owner: 0,
        }
    }

    fn rec(&self, n: &Node) -> Option<&Rec> {
        self.by_path.get(std::str::from_utf8(&n.path).ok()?)
    }
}

/// Turns an NFS mount path into a tree name: no leading or trailing slash,
/// `.` for the root.
fn normalize(p: &[u8]) -> Vec<u8> {
    let start = p.iter().position(|&b| b != b'/').unwrap_or(p.len());
    let end = p.iter().rposition(|&b| b != b'/').map_or(start, |i| i + 1);
    let t = &p[start..end];
    if t.is_empty() {
        b".".to_vec()
    } else {
        t.to_vec()
    }
}

impl FileSystem for MemTree {
    fn mount_root(&self, path: &[u8]) -> Result<Node> {
        let p = String::from_utf8(normalize(path)).map_err(|_| Error::NotFound)?;
        self.by_path
            .get(&p)
            .map(|r| self.node(r))
            .ok_or(Error::NotFound)
    }

    fn node_by_id(&self, id: u64) -> Result<Node> {
        let p = self.by_id.get(&id).ok_or(Error::Stale)?;
        Ok(self.node(&self.by_path[p]))
    }

    fn lookup(&self, dir: &Node, name: &[u8]) -> Result<Node> {
        let d = self.rec(dir).filter(|r| matches!(r.content, Content::Dir));
        let d = d.ok_or(Error::NotDir)?;
        // A name that isn't one ordinary entry is refused before the join:
        // joining ".." onto a mounted subtree would reach its parent.
        if name.is_empty() || name == b"." || name == b".." || name.contains(&b'/') {
            return Err(Error::NotFound);
        }
        let name = std::str::from_utf8(name).map_err(|_| Error::NotFound)?;
        let p = if d.path == "." {
            name.to_string()
        } else {
            format!("{}/{}", d.path, name)
        };
        self.by_path
            .get(&p)
            .map(|r| self.node(r))
            .ok_or(Error::NotFound)
    }

    fn read_dir(&self, dir: &Node) -> Result<Vec<DirEntry>> {
        let d = self.rec(dir).filter(|r| matches!(r.content, Content::Dir));
        let d = d.ok_or(Error::NotDir)?;
        let kids = self.children.get(&d.path).map(Vec::as_slice).unwrap_or(&[]);
        Ok(kids
            .iter()
            .map(|p| DirEntry {
                name: p.rsplit('/').next().unwrap().as_bytes().to_vec(),
                node: self.node(&self.by_path[p]),
                cookie: 0,
            })
            .collect())
    }

    fn read_at(&self, n: &Node, buf: &mut [u8], off: u64) -> Result<usize> {
        let rec = self.rec(n).ok_or(Error::Io)?;
        // A read at or past the end is an empty read, not an error.
        let data: &[u8] = match &rec.content {
            Content::File(d) => d,
            _ => &[],
        };
        if off >= data.len() as u64 {
            return Ok(0);
        }
        let src = &data[off as usize..];
        let k = src.len().min(buf.len());
        buf[..k].copy_from_slice(&src[..k]);
        Ok(k)
    }

    fn readlink(&self, n: &Node) -> Result<Vec<u8>> {
        match self.rec(n).map(|r| &r.content) {
            Some(Content::Link(t)) => Ok(t.clone()),
            _ => Err(Error::Io),
        }
    }
}
