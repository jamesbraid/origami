//! A read-only file-system trait, modelled on Go's `io/fs`, and a resolver
//! that follows symlinks across several file systems.
//!
//! The block layer below is `positioned_io::ReadAt`.

use std::io;
use std::sync::Arc;

pub use positioned_io::ReadAt;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileType {
    File,
    Dir,
    Symlink,
    Other,
}

#[derive(Clone, Debug)]
pub struct Stat {
    pub file_type: FileType,
    pub size: u64,
}

#[derive(Clone, Debug)]
pub struct DirEntry {
    pub name: String,
    pub file_type: FileType,
}

/// A shared block device.
pub type Dev = Arc<dyn ReadAt + Send + Sync>;

/// A read-only file system. Paths are slash-separated, relative to the root,
/// with `""` or `"."` for the root. An implementation never follows a
/// symlink, not even in the middle of a path: it fails such a path, and the
/// caller resolves links with [`ReadFs::read_link`] (see [`Volumes`]).
pub trait ReadFs: Send + Sync {
    fn stat(&self, path: &str) -> io::Result<Stat>;
    fn open(&self, path: &str) -> io::Result<Box<dyn ReadAt + '_>>;
    fn read_dir(&self, path: &str) -> io::Result<Vec<DirEntry>>;
    fn read_link(&self, path: &str) -> io::Result<String>;
}

pub fn not_found(path: &str) -> io::Error {
    io::Error::new(io::ErrorKind::NotFound, format!("{path}: not found"))
}

pub fn invalid(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.into())
}

/// A byte range of a device, as a device of its own.
#[derive(Clone)]
pub struct Part {
    pub dev: Dev,
    pub start: u64,
    pub len: u64,
}

impl ReadAt for Part {
    fn read_at(&self, pos: u64, buf: &mut [u8]) -> io::Result<usize> {
        if pos >= self.len {
            return Ok(0);
        }
        let n = buf.len().min((self.len - pos) as usize);
        self.dev.read_at(self.start + pos, &mut buf[..n])
    }
}

/// Reads up to `limit` bytes of a file.
pub fn read_limited(f: &dyn ReadAt, size: u64, limit: u64) -> io::Result<Vec<u8>> {
    let mut buf = vec![0; size.min(limit) as usize];
    f.read_exact_at(0, &mut buf)?;
    Ok(buf)
}

const MAX_LINKS: usize = 40;
/// Components a single lookup may process, links included.
const MAX_STEPS: usize = 4096;

/// File systems mounted at absolute paths, with symlink-following lookups.
#[derive(Default)]
pub struct Volumes {
    mounts: Vec<(String, Box<dyn ReadFs>)>,
}

impl Volumes {
    pub fn new() -> Self {
        Self::default()
    }

    /// Mounts `fs` at the absolute path `at`.
    pub fn mount(&mut self, at: &str, fs: Box<dyn ReadFs>) {
        self.mounts.push((normalize(at), fs));
    }

    pub fn is_empty(&self) -> bool {
        self.mounts.is_empty()
    }

    fn locate(&self, abs: &str) -> Option<(&dyn ReadFs, String)> {
        let (at, fs) = self
            .mounts
            .iter()
            .filter(|(at, _)| abs == at || at == "/" || abs.starts_with(&format!("{at}/")))
            .max_by_key(|(at, _)| at.len())?;
        let rel = abs[at.len()..].trim_start_matches('/');
        Some((
            fs.as_ref(),
            if rel.is_empty() {
                ".".into()
            } else {
                rel.into()
            },
        ))
    }

    fn lstat(&self, abs: &str) -> io::Result<Stat> {
        let (fs, rel) = self.locate(abs).ok_or_else(|| not_found(abs))?;
        fs.stat(&rel)
    }

    /// The absolute path with every symlink followed, final one included.
    pub fn resolve(&self, path: &str) -> io::Result<String> {
        let mut todo: Vec<String> = components(path).into_iter().rev().collect();
        let mut cur: Vec<String> = vec![];
        let mut links = 0;
        let mut steps = 0;
        while let Some(c) = todo.pop() {
            steps += 1;
            if steps > MAX_STEPS {
                return Err(invalid(format!("{path}: path too long")));
            }
            match c.as_str() {
                "." => continue,
                ".." => {
                    cur.pop();
                    continue;
                }
                _ => {}
            }
            cur.push(c);
            let abs = format!("/{}", cur.join("/"));
            if self.lstat(&abs)?.file_type != FileType::Symlink {
                continue;
            }
            links += 1;
            if links > MAX_LINKS {
                return Err(invalid(format!("{path}: too many symlinks")));
            }
            let (fs, rel) = self.locate(&abs).ok_or_else(|| not_found(&abs))?;
            let target = fs.read_link(&rel)?;
            cur.pop();
            if target.starts_with('/') {
                cur.clear();
            }
            todo.extend(components(&target).into_iter().rev());
        }
        Ok(format!("/{}", cur.join("/")))
    }

    pub fn stat(&self, path: &str) -> io::Result<Stat> {
        self.lstat(&self.resolve(path)?)
    }

    pub fn read_dir(&self, path: &str) -> io::Result<Vec<DirEntry>> {
        let abs = self.resolve(path)?;
        let (fs, rel) = self.locate(&abs).ok_or_else(|| not_found(&abs))?;
        fs.read_dir(&rel)
    }

    /// Reads up to `limit` bytes of the file at `path`.
    pub fn read_file(&self, path: &str, limit: u64) -> io::Result<Vec<u8>> {
        let abs = self.resolve(path)?;
        let (fs, rel) = self.locate(&abs).ok_or_else(|| not_found(&abs))?;
        let st = fs.stat(&rel)?;
        if st.file_type != FileType::File {
            return Err(invalid(format!("{abs}: not a regular file")));
        }
        read_limited(fs.open(&rel)?.as_ref(), st.size, limit)
    }
}

fn components(p: &str) -> Vec<String> {
    p.split('/')
        .filter(|c| !c.is_empty())
        .map(String::from)
        .collect()
}

fn normalize(p: &str) -> String {
    format!("/{}", components(p).join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// A tree of name -> (type, content or link target).
    struct Mem(BTreeMap<&'static str, (FileType, &'static str)>);

    impl ReadFs for Mem {
        fn stat(&self, p: &str) -> io::Result<Stat> {
            if p == "." {
                return Ok(Stat {
                    file_type: FileType::Dir,
                    size: 0,
                });
            }
            let (t, c) = self.0.get(p).ok_or_else(|| not_found(p))?;
            Ok(Stat {
                file_type: *t,
                size: c.len() as u64,
            })
        }
        fn open(&self, p: &str) -> io::Result<Box<dyn ReadAt + '_>> {
            let (_, c) = self.0.get(p).ok_or_else(|| not_found(p))?;
            Ok(Box::new(c.as_bytes()))
        }
        fn read_dir(&self, _: &str) -> io::Result<Vec<DirEntry>> {
            Ok(vec![])
        }
        fn read_link(&self, p: &str) -> io::Result<String> {
            Ok(self.0.get(p).ok_or_else(|| not_found(p))?.1.into())
        }
    }

    fn vols() -> Volumes {
        use FileType::*;
        let root = Mem(BTreeMap::from([
            ("usr", (Dir, "")),
            ("var", (Symlink, "usr/var")),
            ("etc", (Dir, "")),
            ("etc/rel", (Symlink, "../usr/var/hist")),
        ]));
        let usr = Mem(BTreeMap::from([
            ("var", (Dir, "")),
            ("var/hist", (File, "hello")),
        ]));
        let mut v = Volumes::new();
        v.mount("/", Box::new(root));
        v.mount("/usr", Box::new(usr));
        v
    }

    #[test]
    fn follows_links_across_mounts() {
        let v = vols();
        assert_eq!(v.resolve("/var/hist").unwrap(), "/usr/var/hist");
        assert_eq!(v.read_file("/var/hist", 100).unwrap(), b"hello");
        assert_eq!(v.read_file("/etc/rel", 3).unwrap(), b"hel");
    }

    #[test]
    fn hostile_paths_end() {
        let mut v = Volumes::new();
        v.mount("/", Box::new(Mem(BTreeMap::new())));
        assert!(v.resolve(&"a/".repeat(100_000)).is_err());
        // a link that expands to many components each time it is followed
        let mut v = Volumes::new();
        v.mount(
            "/",
            Box::new(Mem(BTreeMap::from([(
                "l",
                (FileType::Symlink, "l/l/l/l/l/l/l/l/l/l"),
            )]))),
        );
        assert!(v.resolve("/l").is_err());
    }

    #[test]
    fn link_loops_end() {
        let mut v = Volumes::new();
        v.mount(
            "/",
            Box::new(Mem(BTreeMap::from([("a", (FileType::Symlink, "a"))]))),
        );
        assert!(v.resolve("/a").is_err());
    }
}
