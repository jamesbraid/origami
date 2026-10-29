//! Exports a host directory for the NFS server to read and write, under
//! the rules a share follows:
//!
//! - Nothing outside the directory is ever reached. Every operation
//!   resolves its path from the share's directory without following
//!   symlinks, and a name from the client must be a single path element.
//!   A symlink is served as a symlink; the client resolves it, not the
//!   server.
//! - The guest sees every file owned by root, and every change lands owned
//!   by whoever runs the server. A chown succeeds and changes nothing. The
//!   setuid bit is never shown, never set, and is cleared by a write.
//! - A filehandle carries the share's id and the file's device, inode and
//!   generation, so it outlives a server restart and goes stale when its
//!   file is replaced. Where the host has no usable inode numbers
//!   (Windows), it carries an id from a table of paths kept in a file
//!   instead.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::fs::{
    fnv1a32, Attr, CreateHow, DirEntry, Error, FileSystem, Key, Node, Result, SetAttr, Space, Time,
};

#[cfg(unix)]
#[path = "host_unix.rs"]
mod host;
#[cfg(windows)]
#[path = "host_windows.rs"]
mod host;
mod table;

use host::{map_err, Host, HostStat, BAD_NAME_CHARS, INODES_USABLE};
use table::{under, PathTable};

/// Handle format bytes, so a handle from a share that changed how it names
/// files is stale rather than misread.
const FMT_INODE: u8 = 2;
const FMT_TABLE: u8 = 3;

/// A hook that sees every attribute set before the server does.
pub type AttrFilter = Box<dyn Fn(&mut Attr) + Send + Sync>;

/// Configures [`Share::open`].
#[derive(Default)]
pub struct Options {
    /// Keeps filehandles in a table of paths saved in this file instead of
    /// deriving them from inodes. Windows needs one; elsewhere setting it
    /// chooses one.
    pub path_table: Option<PathBuf>,

    /// Sees every attribute set before the server does. The transcript
    /// tests use it to pin the times and sizes a host chooses itself.
    pub attr_filter: Option<AttrFilter>,
}

/// One share: a host directory exported under a name, mounted as
/// `/name`.
pub struct Share {
    name: Vec<u8>,
    dir: PathBuf,
    host: Host,
    export_id: u32,
    filter: Option<AttrFilter>,
    /// Exactly one of these names files: the table when the share uses a
    /// path table, otherwise the cache, which remembers where each inode
    /// was last seen.
    table: Option<PathTable>,
    cache: Mutex<HashMap<Key, Vec<u8>>>,
}

fn join(dir: &[u8], name: &[u8]) -> Vec<u8> {
    if dir == b"." {
        return name.to_vec();
    }
    let mut p = dir.to_vec();
    p.push(b'/');
    p.extend_from_slice(name);
    p
}

/// The directory part of a share path, as Go's path.Dir.
fn dir_of(p: &[u8]) -> Vec<u8> {
    match p.iter().rposition(|&b| b == b'/') {
        Some(i) => p[..i].to_vec(),
        None => b".".to_vec(),
    }
}

/// Refuses a name that is not one ordinary directory entry. Joined onto a
/// path, ".." or a separator would reach another directory, possibly
/// outside the share.
fn check_name(name: &[u8]) -> Result<()> {
    if name.is_empty() || name == b"." || name == b".." {
        return Err(Error::Inval);
    }
    if name
        .iter()
        .any(|&b| b == b'/' || b == 0 || BAD_NAME_CHARS.contains(&b))
    {
        return Err(Error::Inval);
    }
    if name.len() > 255 {
        return Err(Error::NameTooLong);
    }
    Ok(())
}

/// A READDIR cookie: a hash of the entry's name, so it stays put however
/// the directory changes. Two names in one directory sharing a hash would
/// make a listing that pauses between them skip the second; with 31 bits
/// that is rare enough to accept.
pub(crate) fn cookie(name: &[u8]) -> u32 {
    (fnv1a32(name) & 0x7fff_ffff).max(1)
}

/// Converts Unix permission bits, less setuid, for chmod.
fn file_mode(m: u32) -> u32 {
    m & 0o3777
}

impl Share {
    /// Exports `dir` as the share `name`, mounted as "/name".
    pub fn open(name: &str, dir: &Path, opts: Options) -> io::Result<Share> {
        if name.is_empty() || name.contains(['/', '\\']) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "sharefs: share name must be one path element",
            ));
        }
        let abs = std::path::absolute(dir)?;
        if !std::fs::metadata(&abs)?.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{}: not a directory", abs.display()),
            ));
        }
        if opts.path_table.is_none() && !INODES_USABLE {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "sharefs: this host has no stable inode numbers, so a share needs a path table",
            ));
        }
        let host = Host::open(&abs)?;
        let table = match &opts.path_table {
            Some(p) => Some(PathTable::open(p)?),
            None => None,
        };
        Ok(Share {
            name: name.as_bytes().to_vec(),
            dir: abs,
            host,
            export_id: fnv1a32(name.as_bytes()),
            filter: opts.attr_filter,
            table,
            cache: Mutex::new(HashMap::new()),
        })
    }

    /// The host directory the share exports.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Stats the file at `rel`, without following a final symlink, and
    /// describes it.
    fn stat_node(&self, rel: &[u8]) -> Result<Node> {
        let st = self.host.lstat(rel).map_err(|e| map_err(&e))?;
        self.describe(rel, st)
    }

    fn describe(&self, rel: &[u8], st: HostStat) -> Result<Node> {
        let key = match &self.table {
            Some(t) => Key {
                ino: t.id_for(rel).map_err(|_| Error::Io)?,
                ..Key::default()
            },
            None => {
                let k = Key {
                    dev: st.dev,
                    ino: st.ino,
                    generation: st.generation,
                };
                self.cache.lock().unwrap().insert(k, rel.to_vec());
                k
            }
        };
        let mut attr = Attr {
            mode: st.mode & !0o4000,
            nlink: st.nlink,
            size: st.size,
            used: st.used,
            fileid: key.ino,
            atime: st.atime,
            mtime: st.mtime,
            ctime: st.ctime,
        };
        if st.mode & 0o170000 == 0o120000 {
            // Hosts differ: Linux gives every symlink 0777, macOS applies
            // the umask. The bits mean nothing for a link, so report
            // Linux's.
            attr.mode |= 0o777;
        }
        if let Some(f) = &self.filter {
            f(&mut attr);
        }
        Ok(Node {
            attr,
            path: rel.to_vec(),
            raw_mode: st.mode,
            key,
            describe: None,
            owner: 0,
        })
    }

    fn own_dir<'a>(&self, n: &'a Node) -> Result<&'a Node> {
        if n.is_dir() {
            Ok(n)
        } else {
            Err(Error::NotDir)
        }
    }

    /// Checks a name for a new entry in `dir` and returns its path.
    fn new_entry(&self, dir: &Node, name: &[u8]) -> Result<Vec<u8>> {
        let d = self.own_dir(dir)?;
        check_name(name)?;
        Ok(join(&d.path, name))
    }

    /// Finds the file `k` names by walking the share, for a handle the
    /// cache doesn't place: one issued before a restart, or for a file
    /// moved on the host.
    fn search(&self, k: Key) -> Result<Node> {
        if let Ok(n) = self.stat_node(b".") {
            if n.key == k {
                return Ok(n);
            }
        }
        let mut queue = std::collections::VecDeque::from([b".".to_vec()]);
        while let Some(dir) = queue.pop_front() {
            let Ok(names) = self.host.list(&dir) else {
                continue;
            };
            for name in names {
                let rel = join(&dir, &name);
                let Ok(st) = self.host.lstat(&rel) else {
                    continue;
                };
                if st.mode & 0o170000 == 0o040000 {
                    queue.push_back(rel.clone());
                }
                if !Host::same_inode(&st, k.dev, k.ino) {
                    continue;
                }
                if let Ok(n) = self.stat_node(&rel) {
                    if n.key == k {
                        return Ok(n);
                    }
                }
            }
        }
        Err(Error::Stale)
    }

    /// Drops what the share remembers about `rel` and everything under it.
    fn forget(&self, rel: &[u8]) {
        match &self.table {
            Some(t) => t.remove(rel),
            None => self.cache.lock().unwrap().retain(|_, p| !under(p, rel)),
        }
    }

    /// Records a rename, so handles under the old path keep working
    /// without a search.
    fn moved(&self, from: &[u8], to: &[u8]) {
        match &self.table {
            Some(t) => t.rename(from, to),
            None => {
                for p in self.cache.lock().unwrap().values_mut() {
                    if under(p, from) {
                        let mut np = to.to_vec();
                        np.extend_from_slice(&p[from.len()..]);
                        *p = np;
                    }
                }
            }
        }
    }

    /// Drops setuid, and setgid on a group-executable file, after a change
    /// to the file's contents, as a local write would.
    fn clear_setuid(&self, n: &Node) {
        let mut strip = 0o4000;
        if n.raw_mode & 0o010 != 0 {
            strip |= 0o2000;
        }
        if n.raw_mode & strip != 0 {
            let _ = self.host.chmod(&n.path, file_mode(n.raw_mode & !strip));
        }
    }

    /// Answers an exclusive create of a name that exists: the same request
    /// again if the file carries its verifier, else EXIST.
    fn retransmitted(&self, rel: &[u8], verf: [u8; 8]) -> Result<Node> {
        let n = self.stat_node(rel)?;
        let st = self.host.lstat(rel).map_err(|e| map_err(&e))?;
        let (at, mt) = verf_times(verf);
        if n.is_reg() && st.atime == at && st.mtime == mt {
            return Ok(n);
        }
        Err(Error::Exist)
    }

    /// The 32-byte handle for a node: format, three zero bytes, the share
    /// id, then device, inode and generation.
    fn make_handle(&self, n: &Node) -> Vec<u8> {
        let mut fh = vec![0u8; 32];
        fh[0] = if self.table.is_some() {
            FMT_TABLE
        } else {
            FMT_INODE
        };
        fh[4..8].copy_from_slice(&self.export_id.to_be_bytes());
        fh[8..16].copy_from_slice(&n.key.dev.to_be_bytes());
        fh[16..24].copy_from_slice(&n.key.ino.to_be_bytes());
        fh[24..32].copy_from_slice(&n.key.generation.to_be_bytes());
        fh
    }

    fn remove_entry(&self, dir: &Node, name: &[u8], want_dir: bool) -> Result<()> {
        let rel = self.new_entry(dir, name)?;
        let n = self.stat_node(&rel)?;
        if want_dir && !n.is_dir() {
            return Err(Error::NotDir);
        }
        if !want_dir && n.is_dir() {
            return Err(Error::IsDir);
        }
        if want_dir {
            self.host.rmdir(&rel)
        } else {
            self.host.unlink(&rel)
        }
        .map_err(|e| map_err(&e))?;
        self.forget(&rel);
        Ok(())
    }
}

/// Where an exclusive create keeps its verifier: in the new file's access
/// and modification seconds, until the client's SETATTR replaces them (RFC
/// 1813 section 3.3.8).
fn verf_times(verf: [u8; 8]) -> (Time, Time) {
    let w = |b: &[u8]| u32::from_be_bytes(b.try_into().unwrap()) as i64;
    (Time::new(w(&verf[..4]), 0), Time::new(w(&verf[4..]), 0))
}

impl FileSystem for Share {
    fn exports(&self) -> Vec<Vec<u8>> {
        let mut p = b"/".to_vec();
        p.extend_from_slice(&self.name);
        vec![p]
    }

    /// Resolves "/name", or a directory beneath it, to its node.
    fn mount_root(&self, p: &[u8]) -> Result<Node> {
        let start = p.iter().position(|&b| b != b'/').unwrap_or(p.len());
        let end = p.iter().rposition(|&b| b != b'/').map_or(start, |i| i + 1);
        let mut parts = p[start..end].split(|&b| b == b'/');
        if parts.next() != Some(&self.name[..]) {
            return Err(Error::NotFound);
        }
        let mut n = self.stat_node(b".")?;
        for part in parts {
            if part.is_empty() {
                continue;
            }
            if check_name(part).is_err() {
                return Err(Error::NotFound);
            }
            if !n.is_dir() {
                return Err(Error::NotDir);
            }
            n = self.stat_node(&join(&n.path, part))?;
        }
        Ok(n)
    }

    /// Unused: the server resolves this filesystem's own handles.
    fn node_by_id(&self, _id: u64) -> Result<Node> {
        Err(Error::Stale)
    }

    fn handle(&self, n: &Node) -> Vec<u8> {
        self.make_handle(n)
    }

    /// Finds the file a handle names. A handle is stale once its share id,
    /// inode or generation no longer matches a file in the share.
    fn node_by_handle(&self, fh: &[u8]) -> Result<Node> {
        let want = if self.table.is_some() {
            FMT_TABLE
        } else {
            FMT_INODE
        };
        if fh.len() != 32
            || fh[1] | fh[2] | fh[3] != 0
            || (fh[0] != FMT_INODE && fh[0] != FMT_TABLE)
        {
            return Err(Error::BadHandle);
        }
        if fh[0] != want || u32::from_be_bytes(fh[4..8].try_into().unwrap()) != self.export_id {
            return Err(Error::Stale);
        }
        let u = |a: usize| u64::from_be_bytes(fh[a..a + 8].try_into().unwrap());
        let k = Key {
            dev: u(8),
            ino: u(16),
            generation: u(24),
        };
        if let Some(t) = &self.table {
            let rel = t.path(k.ino).ok_or(Error::Stale)?;
            return match self.stat_node(&rel) {
                Err(Error::NotFound) => Err(Error::Stale),
                r => r,
            };
        }
        let cached = self.cache.lock().unwrap().get(&k).cloned();
        if let Some(rel) = cached {
            if let Ok(n) = self.stat_node(&rel) {
                if n.key == k {
                    return Ok(n);
                }
            }
        }
        self.search(k)
    }

    /// Finds `name` in `dir`. "." is `dir` itself and ".." its parent; the
    /// share's root is its own parent, as a mount point's is.
    fn lookup(&self, dir: &Node, name: &[u8]) -> Result<Node> {
        let d = self.own_dir(dir)?;
        match name {
            b"." => return self.stat_node(&d.path),
            b".." => return self.stat_node(&dir_of(&d.path)),
            _ => {}
        }
        if check_name(name).is_err() {
            return Err(Error::NotFound);
        }
        self.stat_node(&join(&d.path, name))
    }

    /// Lists `dir` in cookie order. "." and ".." are not listed.
    fn read_dir(&self, dir: &Node) -> Result<Vec<DirEntry>> {
        let d = self.own_dir(dir)?;
        let mut names = self.host.list(&d.path).map_err(|e| map_err(&e))?;
        // Stat in name order, not the directory's own, so a path table
        // numbers new entries the same way on every host.
        names.sort();
        let mut out = Vec::with_capacity(names.len());
        for name in names {
            let Ok(node) = self.stat_node(&join(&d.path, &name)) else {
                continue; // removed since the listing
            };
            let cookie = cookie(&name);
            out.push(DirEntry { name, node, cookie });
        }
        out.sort_by(|a, b| (a.cookie, &a.name).cmp(&(b.cookie, &b.name)));
        Ok(out)
    }

    fn read_at(&self, n: &Node, buf: &mut [u8], off: u64) -> Result<usize> {
        if !n.is_reg() {
            return Err(Error::Inval);
        }
        self.host
            .read_at(&n.path, buf, off)
            .map_err(|e| map_err(&e))
    }

    fn readlink(&self, n: &Node) -> Result<Vec<u8>> {
        if !n.is_link() {
            return Err(Error::Inval);
        }
        self.host.readlink(&n.path).map_err(|e| map_err(&e))
    }

    fn space(&self) -> Result<Space> {
        self.host.space().map_err(|e| map_err(&e))
    }

    fn writable(&self) -> bool {
        true
    }

    /// Applies a client's changes. A symlink's mode and times are left
    /// alone: the host can't set them without following the link.
    fn set_attr(&self, n: &Node, a: &SetAttr) -> Result<()> {
        if let Some(size) = a.size {
            if n.is_dir() {
                return Err(Error::IsDir);
            }
            if !n.is_reg() {
                return Err(Error::Inval);
            }
            self.host
                .truncate(&n.path, size.min(i64::MAX as u64))
                .map_err(|e| map_err(&e))?;
            self.clear_setuid(n);
        }
        if n.is_link() {
            return Ok(());
        }
        if let Some(m) = a.mode {
            self.host
                .chmod(&n.path, file_mode(m))
                .map_err(|e| map_err(&e))?;
        }
        if a.atime.is_some() || a.mtime.is_some() {
            self.host
                .chtimes(&n.path, a.atime, a.mtime)
                .map_err(|e| map_err(&e))?;
        }
        Ok(())
    }

    /// Writes to the host file before returning. It is not synced to disk:
    /// the guest runs on this host, so a crash that loses the host's cache
    /// loses the guest too.
    fn write_at(&self, n: &Node, data: &[u8], off: u64) -> Result<()> {
        if !n.is_reg() {
            return Err(Error::Inval);
        }
        self.host
            .write_at(&n.path, data, off)
            .map_err(|e| map_err(&e))?;
        self.clear_setuid(n);
        Ok(())
    }

    fn create(
        &self,
        dir: &Node,
        name: &[u8],
        how: CreateHow,
        a: &SetAttr,
        verf: [u8; 8],
    ) -> Result<Node> {
        let rel = self.new_entry(dir, name)?;
        let mut exclusive = true;
        if how == CreateHow::Unchecked {
            if let Ok(n) = self.stat_node(&rel) {
                if !n.is_reg() {
                    return Err(Error::Exist);
                }
                self.set_attr(&n, a)?;
                return Ok(n);
            }
            exclusive = false;
        }
        let perm = a.mode.unwrap_or(0o644);
        if let Err(e) = self.host.create(&rel, exclusive, perm & 0o777) {
            if how == CreateHow::Exclusive && e.kind() == io::ErrorKind::AlreadyExists {
                return self.retransmitted(&rel, verf);
            }
            return Err(map_err(&e));
        }
        let mut a = a.clone();
        if how == CreateHow::Exclusive {
            let (at, mt) = verf_times(verf);
            a = SetAttr {
                atime: Some(at),
                mtime: Some(mt),
                ..SetAttr::default()
            };
        }
        // Set the mode outright: the open applied the server's umask, and
        // the client has applied the guest's already.
        a.mode = Some(perm);
        let n = self.stat_node(&rel)?;
        self.set_attr(&n, &a)?;
        self.stat_node(&rel)
    }

    fn mkdir(&self, dir: &Node, name: &[u8], a: &SetAttr) -> Result<Node> {
        let rel = self.new_entry(dir, name)?;
        self.host.mkdir(&rel, 0o755).map_err(|e| map_err(&e))?;
        let n = self.stat_node(&rel)?;
        let mut a = a.clone();
        a.size = None;
        self.set_attr(&n, &a)?;
        self.stat_node(&rel)
    }

    /// Stores `target` as the client gave it. The server never follows it,
    /// so a target outside the share is only text.
    fn symlink(&self, dir: &Node, name: &[u8], target: &[u8], _a: &SetAttr) -> Result<Node> {
        let rel = self.new_entry(dir, name)?;
        if target.is_empty() || target.contains(&0) {
            return Err(Error::Inval);
        }
        self.host.symlink(target, &rel).map_err(|e| map_err(&e))?;
        self.stat_node(&rel)
    }

    fn remove(&self, dir: &Node, name: &[u8]) -> Result<()> {
        self.remove_entry(dir, name, false)
    }

    fn rmdir(&self, dir: &Node, name: &[u8]) -> Result<()> {
        self.remove_entry(dir, name, true)
    }

    fn rename(
        &self,
        from_dir: &Node,
        from_name: &[u8],
        to_dir: &Node,
        to_name: &[u8],
    ) -> Result<()> {
        let from = self.new_entry(from_dir, from_name)?;
        let to = self.new_entry(to_dir, to_name)?;
        self.host.rename(&from, &to).map_err(|e| map_err(&e))?;
        if from == to {
            return Ok(());
        }
        self.forget(&to);
        self.moved(&from, &to);
        Ok(())
    }
}

#[cfg(test)]
mod tests;
