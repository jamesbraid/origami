//! NFS version 2 (RFC 1094).

use crate::fs::{Attr, CreateHow, Error, Node, Result, SetAttr, Time, FH_SIZE};
use crate::rpc::{LogLevel, Server, MAX_RECORD};
use crate::xdr::{Reader, Writer};

// Status codes (RFC 1094 section 2.3.1). They are errno values, and NFSv3
// keeps the same numbers for the ones it shares.
pub(crate) const NFS_OK: u32 = 0;
pub(crate) const NFSERR_PERM: u32 = 1;
pub(crate) const NFSERR_NOENT: u32 = 2;
pub(crate) const NFSERR_IO: u32 = 5;
pub(crate) const NFSERR_NOTDIR: u32 = 20;
pub(crate) const NFSERR_ROFS: u32 = 30;
pub(crate) const NFSERR_STALE: u32 = 70;

// File types.
pub(crate) const NF_NON: u32 = 0;
pub(crate) const NF_REG: u32 = 1;
pub(crate) const NF_DIR: u32 = 2;
pub(crate) const NF_LNK: u32 = 5;

/// Bounds a single READ reply so it fits a UDP datagram.
const MAX_READ: u32 = 8192;

/// Maps an error to a status. Most are errno values both versions share;
/// BADHANDLE and NOTSUPP exist only in NFSv3, and [`status2_for`]
/// substitutes for them. Also the MOUNT status of either version.
pub(crate) fn status_for(err: Option<Error>) -> u32 {
    let Some(e) = err else {
        return NFS_OK;
    };
    match e {
        Error::Perm => NFSERR_PERM,
        Error::NotFound => NFSERR_NOENT,
        Error::Io => NFSERR_IO,
        Error::Access => 13,
        Error::Exist => 17,
        Error::XDev => 18,
        Error::NotDir => NFSERR_NOTDIR,
        Error::IsDir => 21,
        Error::Inval => 22,
        Error::FBig => 27,
        Error::NoSpace => 28,
        Error::Rofs => NFSERR_ROFS,
        Error::NameTooLong => 63,
        Error::NotEmpty => 66,
        Error::Quota => 69,
        Error::Stale => NFSERR_STALE,
        Error::BadHandle => 10001,
        Error::NotSupp => 10004,
    }
}

/// [`status_for`] for NFSv2, which has no BADHANDLE: a handle this server
/// never issued is as stale as one it forgot.
pub(crate) fn status2_for(err: Option<Error>) -> u32 {
    match err {
        Some(Error::BadHandle) => NFSERR_STALE,
        Some(Error::NotSupp) => NFSERR_IO,
        e => status_for(e),
    }
}

pub(crate) fn ftype(mode: u32) -> u32 {
    match mode & 0o170000 {
        0o040000 => NF_DIR,
        0o120000 => NF_LNK,
        0o100000 => NF_REG,
        _ => NF_NON,
    }
}

/// Fits a size into an NFSv2 32-bit field.
fn clamp32(v: u64) -> u32 {
    v.min(u32::MAX as u64) as u32
}

impl Server {
    /// Turns a filehandle into a node, through the filesystem's own
    /// handles.
    pub(crate) fn resolve(&self, fh: &[u8]) -> Result<Node> {
        self.fs.node_by_handle(fh)
    }

    pub(crate) fn handle(&self, n: &Node) -> Vec<u8> {
        self.fs.handle(n)
    }

    pub(crate) fn nfs2_call(&self, proc_: u32, r: &mut Reader) -> Option<Vec<u8>> {
        let writable = self.fs.writable();
        Some(match proc_ {
            0 => Vec::new(),
            1 => self.attrstat(r.bytes(FH_SIZE), None),
            2 if writable => self.nfs_setattr(r),
            3 => Vec::new(), // ROOT, obsolete
            4 => self.nfs_lookup(r),
            5 => self.nfs_readlink(r),
            6 => self.nfs_read(r),
            7 => Vec::new(), // WRITECACHE, obsolete
            8 if writable => self.nfs_write(r),
            9 if writable => self.nfs_create(r, false),
            10 if writable => self.nfs_remove(r, false),
            11 if writable => self.nfs_rename(r),
            12 if writable => status_only(NFSERR_PERM), // LINK: PERM is what link(2) returns without hard links
            13 if writable => self.nfs_symlink(r),
            14 if writable => self.nfs_create(r, true),
            15 if writable => self.nfs_remove(r, true),
            2 | 8..=15 => status_only(NFSERR_ROFS),
            16 => self.nfs_readdir(r),
            17 => self.nfs_statfs(r),
            _ => return None,
        })
    }

    /// Resolves a filehandle to a node, or gives the status to emit.
    fn node(&self, fh: &[u8]) -> std::result::Result<Node, u32> {
        self.resolve(fh).map_err(|e| status2_for(Some(e)))
    }

    /// The reply most v2 procedures give: a status, then on success the
    /// attributes of the node `fh` names, read afresh.
    fn attrstat(&self, fh: &[u8], err: Option<Error>) -> Vec<u8> {
        let mut w = Writer::new();
        if err.is_some() {
            w.u32(status2_for(err));
            return w.b;
        }
        match self.node(fh) {
            Ok(n) => {
                w.u32(NFS_OK);
                write_fattr(&mut w, &n);
            }
            Err(st) => w.u32(st),
        }
        w.b
    }

    fn nfs_lookup(&self, r: &mut Reader) -> Vec<u8> {
        let fh = r.bytes(FH_SIZE);
        let name = r.str();
        let mut w = Writer::new();
        let dir = match self.node(fh) {
            Ok(d) => d,
            Err(st) => {
                w.u32(st);
                return w.b;
            }
        };
        let child = match self.fs.lookup(&dir, name) {
            Ok(c) => c,
            Err(e) => {
                w.u32(status2_for(Some(e)));
                return w.b;
            }
        };
        self.log_open(&child);
        w.u32(NFS_OK);
        w.fixed(&self.handle(&child));
        write_fattr(&mut w, &child);
        w.b
    }

    /// Logs a regular file LOOKUP resolved to: the NFS equivalent of an
    /// open, since a client LOOKUPs a file once and READs the handle it
    /// gets back. A directory component on the way doesn't log.
    pub(crate) fn log_open(&self, n: &Node) {
        if ftype(n.mode()) == NF_REG {
            if let Some(d) = &n.describe {
                self.log(LogLevel::Info, || format!("nfs: opened {d}"));
            }
        }
    }

    fn nfs_readlink(&self, r: &mut Reader) -> Vec<u8> {
        let fh = r.bytes(FH_SIZE);
        let mut w = Writer::new();
        let n = match self.node(fh) {
            Ok(n) => n,
            Err(st) => {
                w.u32(st);
                return w.b;
            }
        };
        match self.fs.readlink(&n) {
            Ok(t) => {
                w.u32(NFS_OK);
                w.opaque(&t);
            }
            Err(e) => w.u32(status2_for(Some(e))),
        }
        w.b
    }

    fn nfs_read(&self, r: &mut Reader) -> Vec<u8> {
        let fh = r.bytes(FH_SIZE);
        let offset = r.u32();
        let count = r.u32();
        r.u32(); // totalcount, ignored per spec
        let mut w = Writer::new();
        let n = match self.node(fh) {
            Ok(n) => n,
            Err(st) => {
                w.u32(st);
                return w.b;
            }
        };
        let mut buf = vec![0u8; count.min(MAX_READ) as usize];
        match self.fs.read_at(&n, &mut buf, offset as u64) {
            Ok(got) => {
                w.u32(NFS_OK);
                write_fattr(&mut w, &n);
                w.opaque(&buf[..got]);
            }
            Err(e) => w.u32(status2_for(Some(e))),
        }
        w.b
    }

    fn nfs_setattr(&self, r: &mut Reader) -> Vec<u8> {
        let fh = r.bytes(FH_SIZE);
        let a = read_sattr2(r);
        if r.err {
            return Vec::new();
        }
        let err = match self.resolve(fh) {
            Ok(n) => self.fs.set_attr(&n, &a).err(),
            Err(e) => Some(e),
        };
        self.attrstat(fh, err)
    }

    fn nfs_write(&self, r: &mut Reader) -> Vec<u8> {
        let fh = r.bytes(FH_SIZE);
        r.u32(); // beginoffset, unused (RFC 1094 section 2.2.9)
        let offset = r.u32();
        r.u32(); // totalcount, unused
        let data = r.opaque(MAX_RECORD);
        if r.err {
            return Vec::new();
        }
        let err = match self.resolve(fh) {
            Ok(n) => self.write_to(&n, data, offset as u64).err(),
            Err(e) => Some(e),
        };
        self.attrstat(fh, err)
    }

    /// Checks `n` is a regular file and writes to it.
    pub(crate) fn write_to(&self, n: &Node, data: &[u8], offset: u64) -> Result<()> {
        match ftype(n.mode()) {
            NF_REG => {}
            NF_DIR => return Err(Error::IsDir),
            _ => return Err(Error::Inval),
        }
        if offset > i64::MAX as u64 - data.len() as u64 {
            return Err(Error::FBig);
        }
        self.fs.write_at(n, data, offset)
    }

    /// Reads diropargs and resolves the directory, refusing a
    /// non-directory.
    fn dirop<'a>(&self, r: &mut Reader<'a>) -> (Result<Node>, &'a [u8]) {
        let fh = r.bytes(FH_SIZE);
        let name = r.str();
        if r.err {
            return (Err(Error::Io), name);
        }
        let dir = match self.resolve(fh) {
            Ok(d) if ftype(d.mode()) != NF_DIR => Err(Error::NotDir),
            other => other,
        };
        (dir, name)
    }

    /// Answers CREATE and, with `mkdir` set, MKDIR: both reply with the
    /// new node's handle and attributes. A v2 CREATE of a name that exists
    /// opens it and applies the attributes, so a size of zero truncates it.
    fn nfs_create(&self, r: &mut Reader, mkdir: bool) -> Vec<u8> {
        let (dir, name) = self.dirop(r);
        let a = read_sattr2(r);
        if r.err {
            return Vec::new();
        }
        let made = dir.and_then(|d| {
            if mkdir {
                self.fs.mkdir(&d, name, &a)
            } else {
                self.fs.create(&d, name, CreateHow::Unchecked, &a, [0; 8])
            }
        });
        let mut w = Writer::new();
        let n = match made {
            Ok(n) => n,
            Err(e) => {
                w.u32(status2_for(Some(e)));
                return w.b;
            }
        };
        let fh = self.handle(&n);
        let n = match self.resolve(&fh) {
            Ok(n) => n,
            Err(e) => {
                w.u32(status2_for(Some(e)));
                return w.b;
            }
        };
        w.u32(NFS_OK);
        w.fixed(&fh);
        write_fattr(&mut w, &n);
        w.b
    }

    fn nfs_remove(&self, r: &mut Reader, rmdir: bool) -> Vec<u8> {
        let (dir, name) = self.dirop(r);
        if r.err {
            return Vec::new();
        }
        let res = dir.and_then(|d| {
            if rmdir {
                self.fs.rmdir(&d, name)
            } else {
                self.fs.remove(&d, name)
            }
        });
        status_only(status2_for(res.err()))
    }

    fn nfs_rename(&self, r: &mut Reader) -> Vec<u8> {
        let (from, from_name) = self.dirop(r);
        let (to, to_name) = self.dirop(r);
        if r.err {
            return Vec::new();
        }
        let res = from.and_then(|f| to.and_then(|t| self.fs.rename(&f, from_name, &t, to_name)));
        status_only(status2_for(res.err()))
    }

    fn nfs_symlink(&self, r: &mut Reader) -> Vec<u8> {
        let (dir, name) = self.dirop(r);
        let target = r.str();
        let a = read_sattr2(r);
        if r.err {
            return Vec::new();
        }
        let res = dir.and_then(|d| self.fs.symlink(&d, name, target, &a));
        status_only(status2_for(res.err()))
    }

    fn nfs_readdir(&self, r: &mut Reader) -> Vec<u8> {
        let fh = r.bytes(FH_SIZE);
        let cookie = r.u32();
        let count = r.u32();
        let mut w = Writer::new();
        let dir = match self.node(fh) {
            Ok(d) => d,
            Err(st) => {
                w.u32(st);
                return w.b;
            }
        };
        let ents = match self.fs.read_dir(&dir) {
            Ok(e) => e,
            Err(e) => {
                w.u32(status2_for(Some(e)));
                return w.b;
            }
        };
        w.u32(NFS_OK);
        let mut budget = count as i64;
        let start = readdir_start(&ents, cookie as u64);
        let mut i = start;
        while i < ents.len() {
            let e = &ents[i];
            let entry_size = (4 + 4 + 4 + ((e.name.len() + 3) & !3) + 4) as i64;
            if budget - entry_size < 0 && i > start {
                break;
            }
            budget -= entry_size;
            w.u32(1); // value follows
            w.u32(e.node.attr.fileid as u32);
            w.opaque(&e.name);
            w.u32(entry_cookie(&ents, i) as u32);
            i += 1;
        }
        w.u32(0); // no more entries in this reply
        w.bool(i >= ents.len());
        w.b
    }

    fn nfs_statfs(&self, r: &mut Reader) -> Vec<u8> {
        let fh = r.bytes(FH_SIZE);
        let mut w = Writer::new();
        if let Err(st) = self.node(fh) {
            w.u32(st);
            return w.b;
        }
        let sp = match self.fs.space() {
            Ok(s) => s,
            Err(e) => {
                w.u32(status2_for(Some(e)));
                return w.b;
            }
        };
        // Without capacity to report, the export is read-only media:
        // 512-byte blocks, none free. With it, 4 KiB blocks reach 16 TiB
        // in 32 bits.
        let bsize: u64 = if sp.total != 0 { 4096 } else { 512 };
        w.u32(NFS_OK);
        w.u32(MAX_READ); // tsize
        w.u32(bsize as u32);
        w.u32(clamp32(sp.total / bsize));
        w.u32(clamp32(sp.free / bsize));
        w.u32(clamp32(sp.avail / bsize));
        w.b
    }
}

/// A reply that is only a status, as every refusal of a procedure whose
/// arguments aren't decoded.
pub(crate) fn status_only(st: u32) -> Vec<u8> {
    let mut w = Writer::new();
    w.u32(st);
    w.b
}

/// Writes an NFSv2 fattr (68 bytes). The guest sees every file owned by
/// root. Blocks are 512 bytes, matching EFS.
fn write_fattr(w: &mut Writer, n: &Node) {
    let a: &Attr = &n.attr;
    w.u32(ftype(a.mode));
    w.u32(a.mode);
    w.u32(a.nlink);
    w.u32(0); // uid
    w.u32(0); // gid
    w.u32(clamp32(a.size));
    w.u32(512); // blocksize
    w.u32(0); // rdev
    w.u32(clamp32(a.used / 512));
    w.u32(1); // fsid
    w.u32(a.fileid as u32);
    for t in [a.atime, a.mtime, a.ctime] {
        w.u32(t.sec as u32);
        w.u32(t.nsec / 1000);
    }
}

/// Reads an NFSv2 timeval from a sattr. All ones leaves the time alone; a
/// microsecond count of one million asks for the server's clock, as Sun
/// clients send for utimes(path, NULL).
fn read_time2(r: &mut Reader) -> Option<Time> {
    let sec = r.u32();
    let usec = r.u32();
    if sec == u32::MAX {
        None
    } else if usec == 1_000_000 {
        Some(Time::now())
    } else {
        Some(Time::unix(sec as i64, usec as i64 * 1000))
    }
}

/// Reads an NFSv2 sattr, where all ones means "don't set".
fn read_sattr2(r: &mut Reader) -> SetAttr {
    let mut a = SetAttr::default();
    let m = r.u32();
    if m != u32::MAX {
        a.mode = Some(m & 0o7777);
    }
    r.u32(); // uid
    r.u32(); // gid
    let sz = r.u32();
    if sz != u32::MAX {
        a.size = Some(sz as u64);
    }
    a.atime = read_time2(r);
    a.mtime = read_time2(r);
    a
}

/// The index of the first entry after `cookie`. Entries with their own
/// cookies resume after the last one the client saw, so a listing
/// survives entries removed before it; others are numbered by position.
pub(crate) fn readdir_start(ents: &[crate::fs::DirEntry], cookie: u64) -> usize {
    if !ents.is_empty() && ents[0].cookie != 0 {
        return ents
            .iter()
            .position(|e| e.cookie as u64 > cookie)
            .unwrap_or(ents.len());
    }
    cookie.min(ents.len() as u64) as usize
}

pub(crate) fn entry_cookie(ents: &[crate::fs::DirEntry], i: usize) -> u64 {
    match ents[i].cookie {
        0 => i as u64 + 1,
        c => c as u64,
    }
}
