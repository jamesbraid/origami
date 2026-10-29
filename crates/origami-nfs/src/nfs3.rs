//! NFS version 3 (RFC 1813).

use crate::fs::{Attr, CreateHow, DirEntry, Error, Node, Result, SetAttr, Time};
use crate::nfs2::{
    entry_cookie, ftype, readdir_start, status_for, NFSERR_NOTDIR, NFSERR_ROFS, NFS_OK, NF_DIR,
    NF_LNK, NF_REG,
};
use crate::rpc::{Server, MAX_RECORD};
use crate::xdr::{Reader, Writer};

// Status codes beyond those NFSv3 shares with version 2, which keep their
// numbers (RFC 1813 section 2.6).
const NFS3ERR_INVAL: u32 = 22;
const NFS3ERR_ISDIR: u32 = 21;
const NFS3ERR_NOT_SYNC: u32 = 10002;
const NFS3ERR_NOTSUPP: u32 = 10004;
const NFS3ERR_TOOSMALL: u32 = 10005;

/// rtmax and wtmax. 32 KiB plus headers still fits one UDP datagram.
const MAX_READ3: u32 = 32768;

// ACCESS bits (RFC 1813 section 3.3.4).
const ACCESS3_READ: u32 = 0x01;
const ACCESS3_LOOKUP: u32 = 0x02;
const ACCESS3_MODIFY: u32 = 0x04;
const ACCESS3_EXTEND: u32 = 0x08;
const ACCESS3_DELETE: u32 = 0x10;
const ACCESS3_EXECUTE: u32 = 0x20;

// FSINFO properties (RFC 1813 section 3.3.19).
const FSF3_SYMLINK: u32 = 0x02;
const FSF3_HOMOGENEOUS: u32 = 0x08;
const FSF3_CANSETTIME: u32 = 0x10;

const FILE_SYNC: u32 = 2; // stable_how

/// The size of a READDIR or READDIRPLUS reply around its entries: status,
/// dir_attributes, cookieverf, the list terminator and eof.
const READDIR_FIXED: i64 = 4 + 4 + 84 + 8 + 4 + 4;

/// Weak cache consistency data for one object (RFC 1813 section 2.6): its
/// attributes before a change, and afterwards those of whatever its
/// handle names then.
#[derive(Default)]
struct Wcc {
    fh: Option<Vec<u8>>,
    pre: Option<Attr>,
}

/// A reply that refuses a procedure without decoding its arguments. The
/// failure body is pre_op_attr and post_op_attr values with nothing to
/// report: `empty` counts them, two for each wcc_data, one for a lone
/// post_op_attr.
fn refuse3(status: u32, empty: usize) -> Vec<u8> {
    let mut w = Writer::new();
    w.u32(status);
    for _ in 0..empty {
        w.bool(false);
    }
    w.b
}

/// Reads an nfs_fh3, a variable-length opaque of at most 64 bytes.
fn read_fh3<'a>(r: &mut Reader<'a>) -> &'a [u8] {
    r.opaque(64)
}

/// Writes an NFSv3 fattr3 (84 bytes). The guest sees every file owned by
/// root.
fn write_fattr3(w: &mut Writer, n: &Node) {
    let a = &n.attr;
    w.u32(ftype(a.mode));
    w.u32(a.mode & 0o7777);
    w.u32(a.nlink);
    w.u32(0); // uid
    w.u32(0); // gid
    w.u64(a.size);
    w.u64(a.used);
    w.u32(0); // rdev specdata1
    w.u32(0); // rdev specdata2
    w.u64(1); // fsid
    w.u64(a.fileid);
    write_time3(w, a.atime);
    write_time3(w, a.mtime);
    write_time3(w, a.ctime);
}

fn write_time3(w: &mut Writer, t: Time) {
    w.u32(t.sec as u32);
    w.u32(t.nsec);
}

/// Writes a post_op_attr: the attributes when `n` is known.
fn write_post_op(w: &mut Writer, n: Option<&Node>) {
    match n {
        None => w.bool(false),
        Some(n) => {
            w.bool(true);
            write_fattr3(w, n);
        }
    }
}

/// Writes a pre_op_attr from attributes taken before a change.
fn write_pre_op(w: &mut Writer, a: Option<&Attr>) {
    match a {
        None => w.bool(false),
        Some(a) => {
            w.bool(true);
            w.u64(a.size);
            write_time3(w, a.mtime);
            write_time3(w, a.ctime);
        }
    }
}

/// Reads a set_atime or set_mtime.
fn read_set_time3(r: &mut Reader) -> Option<Time> {
    match r.u32() {
        0 => None,              // DONT_CHANGE
        1 => Some(Time::now()), // SET_TO_SERVER_TIME
        2 => {
            // SET_TO_CLIENT_TIME
            let sec = r.u32();
            let nsec = r.u32();
            Some(Time::unix(sec as i64, nsec as i64))
        }
        _ => {
            r.err = true;
            None
        }
    }
}

fn read_sattr3(r: &mut Reader) -> SetAttr {
    let mut a = SetAttr::default();
    if r.bool() {
        a.mode = Some(r.u32() & 0o7777);
    }
    if r.bool() {
        r.u32(); // uid: every file is root's
    }
    if r.bool() {
        r.u32(); // gid
    }
    if r.bool() {
        a.size = Some(r.u64());
    }
    a.atime = read_set_time3(r);
    a.mtime = read_set_time3(r);
    a
}

fn xdr_str_len(s: &[u8]) -> i64 {
    4 + ((s.len() + 3) & !3) as i64
}

/// The result of reading diropargs3.
struct DirOp3<'a> {
    dir: Result<Node>,
    name: &'a [u8],
    wcc: Wcc,
}

impl Server {
    pub(crate) fn nfs3_call(&self, proc_: u32, r: &mut Reader) -> Option<Vec<u8>> {
        let writable = self.fs.writable();
        Some(match proc_ {
            0 => Vec::new(),
            1 => self.nfs3_getattr(r),
            2 if writable => self.nfs3_setattr(r),
            3 => self.nfs3_lookup(r),
            4 => self.nfs3_access(r),
            5 => self.nfs3_readlink(r),
            6 => self.nfs3_read(r),
            7 if writable => self.nfs3_write(r),
            8 if writable => self.nfs3_create(r),
            9 if writable => self.nfs3_mkdir(r),
            10 if writable => self.nfs3_symlink(r),
            11 if writable => refuse3(NFS3ERR_NOTSUPP, 2), // MKNOD: no device nodes in a share
            12 if writable => self.nfs3_remove(r, false),
            13 if writable => self.nfs3_remove(r, true),
            14 if writable => self.nfs3_rename(r),
            15 if writable => refuse3(NFS3ERR_NOTSUPP, 3), // LINK: the export keeps one path per file
            21 if writable => self.nfs3_commit(r),
            2 | 7..=13 | 21 => refuse3(NFSERR_ROFS, 2),
            14 => refuse3(NFSERR_ROFS, 4),
            15 => refuse3(NFSERR_ROFS, 3),
            16 => self.nfs3_readdir(r),
            17 => self.nfs3_readdirplus(r),
            18 => self.nfs3_fsstat(r),
            19 => self.nfs3_fsinfo(r),
            20 => self.nfs3_pathconf(r),
            _ => return None,
        })
    }

    /// Resolves a v3 handle. A handle this server could never have issued
    /// is BADHANDLE; one it no longer knows is STALE.
    fn node3(&self, fh: &[u8]) -> std::result::Result<Node, u32> {
        self.resolve(fh).map_err(|e| status_for(Some(e)))
    }

    fn wcc_of(&self, fh: &[u8], n: Option<&Node>) -> Wcc {
        match n {
            None => Wcc::default(),
            Some(n) => Wcc {
                fh: Some(fh.to_vec()),
                pre: Some(n.attr),
            },
        }
    }

    fn write_wcc(&self, w: &mut Writer, c: &Wcc) {
        write_pre_op(w, c.pre.as_ref());
        match &c.fh {
            None => write_post_op(w, None),
            Some(fh) => {
                let n = self.resolve(fh).ok();
                write_post_op(w, n.as_ref());
            }
        }
    }

    /// Reads diropargs3 and resolves the directory, refusing a
    /// non-directory.
    fn dirop3<'a>(&self, r: &mut Reader<'a>) -> DirOp3<'a> {
        let fh = read_fh3(r);
        let name = r.str();
        if r.err {
            return DirOp3 {
                dir: Err(Error::Io),
                name,
                wcc: Wcc::default(),
            };
        }
        let dir = self.resolve(fh);
        let wcc = self.wcc_of(fh, dir.as_ref().ok());
        let dir = match dir {
            Ok(d) if ftype(d.mode()) != NF_DIR => Err(Error::NotDir),
            other => other,
        };
        DirOp3 { dir, name, wcc }
    }

    fn nfs3_getattr(&self, r: &mut Reader) -> Vec<u8> {
        let fh = read_fh3(r);
        let mut w = Writer::new();
        match self.node3(fh) {
            Ok(n) => {
                w.u32(NFS_OK);
                write_fattr3(&mut w, &n);
            }
            Err(st) => w.u32(st),
        }
        w.b
    }

    fn nfs3_lookup(&self, r: &mut Reader) -> Vec<u8> {
        let fh = read_fh3(r);
        let name = r.str();
        let mut w = Writer::new();
        let dir = match self.node3(fh) {
            Ok(d) => d,
            Err(st) => {
                w.u32(st);
                write_post_op(&mut w, None);
                return w.b;
            }
        };
        if ftype(dir.mode()) != NF_DIR {
            w.u32(NFSERR_NOTDIR);
            write_post_op(&mut w, Some(&dir));
            return w.b;
        }
        let child = match self.fs.lookup(&dir, name) {
            Ok(c) => c,
            Err(e) => {
                w.u32(status_for(Some(e)));
                write_post_op(&mut w, Some(&dir));
                return w.b;
            }
        };
        self.log_open(&child);
        w.u32(NFS_OK);
        w.opaque(&self.handle(&child));
        write_post_op(&mut w, Some(&child));
        write_post_op(&mut w, Some(&dir));
        w.b
    }

    /// Grants what root may do: read anything, search any directory,
    /// execute a file with an execute bit set, and on a writable export
    /// change any directory or file.
    fn nfs3_access(&self, r: &mut Reader) -> Vec<u8> {
        let fh = read_fh3(r);
        let want = r.u32();
        let mut w = Writer::new();
        let n = match self.node3(fh) {
            Ok(n) => n,
            Err(st) => {
                w.u32(st);
                write_post_op(&mut w, None);
                return w.b;
            }
        };
        let mut allow = ACCESS3_READ;
        let writable = self.fs.writable();
        match ftype(n.mode()) {
            NF_DIR => {
                allow |= ACCESS3_LOOKUP;
                if writable {
                    allow |= ACCESS3_MODIFY | ACCESS3_EXTEND | ACCESS3_DELETE;
                }
            }
            NF_REG => {
                if n.mode() & 0o111 != 0 {
                    allow |= ACCESS3_EXECUTE;
                }
                if writable {
                    allow |= ACCESS3_MODIFY | ACCESS3_EXTEND;
                }
            }
            _ => {}
        }
        w.u32(NFS_OK);
        write_post_op(&mut w, Some(&n));
        w.u32(want & allow);
        w.b
    }

    fn nfs3_readlink(&self, r: &mut Reader) -> Vec<u8> {
        let fh = read_fh3(r);
        let mut w = Writer::new();
        let n = match self.node3(fh) {
            Ok(n) => n,
            Err(st) => {
                w.u32(st);
                write_post_op(&mut w, None);
                return w.b;
            }
        };
        if ftype(n.mode()) != NF_LNK {
            w.u32(NFS3ERR_INVAL);
            write_post_op(&mut w, Some(&n));
            return w.b;
        }
        match self.fs.readlink(&n) {
            Ok(target) => {
                w.u32(NFS_OK);
                write_post_op(&mut w, Some(&n));
                w.opaque(&target);
            }
            Err(e) => {
                w.u32(status_for(Some(e)));
                write_post_op(&mut w, Some(&n));
            }
        }
        w.b
    }

    fn nfs3_read(&self, r: &mut Reader) -> Vec<u8> {
        let fh = read_fh3(r);
        let offset = r.u64();
        let count = r.u32();
        let mut w = Writer::new();
        let n = match self.node3(fh) {
            Ok(n) => n,
            Err(st) => {
                w.u32(st);
                write_post_op(&mut w, None);
                return w.b;
            }
        };
        let st = match ftype(n.mode()) {
            NF_DIR => NFS3ERR_ISDIR,
            NF_REG => NFS_OK,
            _ => NFS3ERR_INVAL,
        };
        if st != NFS_OK {
            w.u32(st);
            write_post_op(&mut w, Some(&n));
            return w.b;
        }
        let size = n.size().max(0) as u64;
        let mut data = Vec::new();
        if offset < size {
            data = vec![0u8; count.min(MAX_READ3) as usize];
            match self.fs.read_at(&n, &mut data, offset) {
                Ok(got) => data.truncate(got),
                Err(e) => {
                    w.u32(status_for(Some(e)));
                    write_post_op(&mut w, Some(&n));
                    return w.b;
                }
            }
        }
        w.u32(NFS_OK);
        write_post_op(&mut w, Some(&n));
        w.u32(data.len() as u32);
        w.bool(offset.wrapping_add(data.len() as u64) >= size);
        w.opaque(&data);
        w.b
    }

    /// Resolves a READDIR or READDIRPLUS directory and its entries. On
    /// failure it writes the failure reply into `w` and returns None.
    fn readdir_start3(&self, w: &mut Writer, fh: &[u8]) -> Option<(Node, Vec<DirEntry>)> {
        let dir = match self.node3(fh) {
            Ok(d) => d,
            Err(st) => {
                w.u32(st);
                write_post_op(w, None);
                return None;
            }
        };
        if ftype(dir.mode()) != NF_DIR {
            w.u32(NFSERR_NOTDIR);
            write_post_op(w, Some(&dir));
            return None;
        }
        match self.fs.read_dir(&dir) {
            Ok(ents) => Some((dir, ents)),
            Err(e) => {
                w.u32(status_for(Some(e)));
                write_post_op(w, Some(&dir));
                None
            }
        }
    }

    fn nfs3_readdir(&self, r: &mut Reader) -> Vec<u8> {
        let fh = read_fh3(r);
        let cookie = r.u64();
        r.bytes(8); // cookieverf
        let count = r.u32() as i64;
        let mut w = Writer::new();
        let Some((dir, ents)) = self.readdir_start3(&mut w, fh) else {
            return w.b;
        };
        let mut body = Writer::new();
        let mut budget = count - READDIR_FIXED;
        let ok = readdir_entries(
            &mut body,
            &ents,
            cookie,
            |e| {
                let n = 4 + 8 + xdr_str_len(&e.name) + 8;
                if n > budget {
                    return false;
                }
                budget -= n;
                true
            },
            |body, e, c| {
                body.u64(e.node.attr.fileid);
                body.opaque(&e.name);
                body.u64(c);
            },
        );
        if !ok {
            w.u32(NFS3ERR_TOOSMALL);
            write_post_op(&mut w, Some(&dir));
            return w.b;
        }
        w.u32(NFS_OK);
        write_post_op(&mut w, Some(&dir));
        // The cookie verifier is always zero and an incoming one is not
        // checked (RFC 1813 allows either): a cookie is an entry's
        // position or its own stable value, and neither is invalidated
        // by the directory changing.
        w.fixed(&[0; 8]);
        w.fixed(&body.b);
        w.b
    }

    fn nfs3_readdirplus(&self, r: &mut Reader) -> Vec<u8> {
        let fh = read_fh3(r);
        let cookie = r.u64();
        r.bytes(8); // cookieverf
        let dircount = r.u32() as i64;
        let maxcount = r.u32() as i64;
        let mut w = Writer::new();
        let Some((dir, ents)) = self.readdir_start3(&mut w, fh) else {
            return w.b;
        };
        let mut body = Writer::new();
        // dircount bounds the entries' fileid, name and cookie alone;
        // maxcount bounds the whole reply.
        let (mut dir_budget, mut max_budget) = (dircount, maxcount - READDIR_FIXED);
        let ok = readdir_entries(
            &mut body,
            &ents,
            cookie,
            |e| {
                let d = 8 + xdr_str_len(&e.name) + 8;
                let m = 4 + d + 4 + 84 + 4 + 4 + crate::fs::FH_SIZE as i64;
                if d > dir_budget || m > max_budget {
                    return false;
                }
                dir_budget -= d;
                max_budget -= m;
                true
            },
            |body, e, c| {
                body.u64(e.node.attr.fileid);
                body.opaque(&e.name);
                body.u64(c);
                write_post_op(body, Some(&e.node));
                body.bool(true); // post_op_fh3
                body.opaque(&self.handle(&e.node));
            },
        );
        if !ok {
            w.u32(NFS3ERR_TOOSMALL);
            write_post_op(&mut w, Some(&dir));
            return w.b;
        }
        w.u32(NFS_OK);
        write_post_op(&mut w, Some(&dir));
        w.fixed(&[0; 8]);
        w.fixed(&body.b);
        w.b
    }

    /// Reports the filesystem's capacity, or none for read-only media.
    fn nfs3_fsstat(&self, r: &mut Reader) -> Vec<u8> {
        let fh = read_fh3(r);
        let mut w = Writer::new();
        let n = self.node3(fh);
        let (st, sp) = match &n {
            Ok(_) => match self.fs.space() {
                Ok(sp) => (NFS_OK, sp),
                Err(e) => (status_for(Some(e)), Default::default()),
            },
            Err(st) => (*st, Default::default()),
        };
        w.u32(st);
        write_post_op(&mut w, n.as_ref().ok());
        if st != NFS_OK {
            return w.b;
        }
        for v in [
            sp.total,
            sp.free,
            sp.avail,
            sp.files,
            sp.free_files,
            sp.avail_files,
        ] {
            w.u64(v);
        }
        w.u32(0); // invarsec
        w.b
    }

    fn nfs3_fsinfo(&self, r: &mut Reader) -> Vec<u8> {
        let fh = read_fh3(r);
        let mut w = Writer::new();
        let n = self.node3(fh);
        let st = n.as_ref().err().copied().unwrap_or(NFS_OK);
        w.u32(st);
        write_post_op(&mut w, n.as_ref().ok());
        if st != NFS_OK {
            return w.b;
        }
        w.u32(MAX_READ3); // rtmax
        w.u32(MAX_READ3); // rtpref
        w.u32(4096); // rtmult
        w.u32(MAX_READ3); // wtmax
        w.u32(MAX_READ3); // wtpref
        w.u32(4096); // wtmult
        w.u32(8192); // dtpref
        w.u64(i64::MAX as u64);
        // A writable export is a host directory, which keeps times to the
        // nanosecond; read-only media keep whole seconds.
        if self.fs.writable() {
            w.u32(0); // time_delta
            w.u32(1);
            w.u32(FSF3_SYMLINK | FSF3_HOMOGENEOUS | FSF3_CANSETTIME);
            return w.b;
        }
        w.u32(1); // time_delta
        w.u32(0);
        w.u32(FSF3_SYMLINK | FSF3_HOMOGENEOUS);
        w.b
    }

    fn nfs3_pathconf(&self, r: &mut Reader) -> Vec<u8> {
        let fh = read_fh3(r);
        let mut w = Writer::new();
        let n = self.node3(fh);
        let st = n.as_ref().err().copied().unwrap_or(NFS_OK);
        w.u32(st);
        write_post_op(&mut w, n.as_ref().ok());
        if st != NFS_OK {
            return w.b;
        }
        w.u32(1); // linkmax: no hard links
        w.u32(255); // name_max
        w.bool(true); // no_trunc
        w.bool(true); // chown_restricted
        w.bool(false); // case_insensitive
        w.bool(true); // case_preserving
        w.b
    }

    fn nfs3_setattr(&self, r: &mut Reader) -> Vec<u8> {
        let fh = read_fh3(r);
        let a = read_sattr3(r);
        let mut guard = None;
        if r.bool() {
            let sec = r.u32();
            let nsec = r.u32();
            guard = Some(Time::unix(sec as i64, nsec as i64));
        }
        if r.err {
            return Vec::new();
        }
        let n = self.resolve(fh);
        let c = self.wcc_of(fh, n.as_ref().ok());
        let st = match &n {
            Err(e) => status_for(Some(*e)),
            Ok(n) => match guard {
                Some(g) if Some(g) != c.pre.map(|p| p.ctime) => NFS3ERR_NOT_SYNC,
                _ => status_for(self.fs.set_attr(n, &a).err()),
            },
        };
        let mut w = Writer::new();
        w.u32(st);
        self.write_wcc(&mut w, &c);
        w.b
    }

    fn nfs3_write(&self, r: &mut Reader) -> Vec<u8> {
        let fh = read_fh3(r);
        let offset = r.u64();
        let count = r.u32();
        r.u32(); // stable: every write is FILE_SYNC
        let data = r.opaque(MAX_RECORD);
        if r.err {
            return Vec::new();
        }
        let data = &data[..(count as usize).min(data.len())];
        let n = self.resolve(fh);
        let c = self.wcc_of(fh, n.as_ref().ok());
        let err = match &n {
            Ok(n) => self.write_to(n, data, offset).err(),
            Err(e) => Some(*e),
        };
        let mut w = Writer::new();
        w.u32(status_for(err));
        self.write_wcc(&mut w, &c);
        if err.is_some() {
            return w.b;
        }
        w.u32(data.len() as u32);
        w.u32(FILE_SYNC);
        w.fixed(&self.verifier);
        w.b
    }

    /// Has nothing to flush, since every WRITE was FILE_SYNC; it returns
    /// the verifier so a client can tell the server hasn't restarted.
    fn nfs3_commit(&self, r: &mut Reader) -> Vec<u8> {
        let fh = read_fh3(r);
        r.u64(); // offset
        r.u32(); // count
        if r.err {
            return Vec::new();
        }
        let n = self.resolve(fh);
        let mut w = Writer::new();
        w.u32(status_for(n.as_ref().err().copied()));
        write_pre_op(&mut w, None);
        write_post_op(&mut w, n.as_ref().ok());
        if n.is_ok() {
            w.fixed(&self.verifier);
        }
        w.b
    }

    /// Writes the success reply of CREATE, MKDIR and SYMLINK: the node's
    /// handle and attributes, then the directory's wcc. A node made but
    /// then not found again still succeeds, without the optional parts.
    fn new_node(&self, w: &mut Writer, n: &Node, dir: &Wcc) {
        w.u32(NFS_OK);
        let fh = self.handle(n);
        match self.resolve(&fh) {
            Err(_) => {
                w.bool(false); // post_op_fh3
                write_post_op(w, None);
            }
            Ok(n) => {
                w.bool(true);
                w.opaque(&fh);
                write_post_op(w, Some(&n));
            }
        }
        self.write_wcc(w, dir);
    }

    /// Finishes CREATE, MKDIR or SYMLINK.
    fn made(&self, res: Result<Node>, dir: &Wcc) -> Vec<u8> {
        let mut w = Writer::new();
        match res {
            Err(e) => {
                w.u32(status_for(Some(e)));
                self.write_wcc(&mut w, dir);
            }
            Ok(n) => self.new_node(&mut w, &n, dir),
        }
        w.b
    }

    fn nfs3_create(&self, r: &mut Reader) -> Vec<u8> {
        let op = self.dirop3(r);
        let how = r.u32();
        let mut a = SetAttr::default();
        let mut verf = [0u8; 8];
        let how = match how {
            0 => CreateHow::Unchecked,
            1 => CreateHow::Guarded,
            2 => CreateHow::Exclusive,
            _ => {
                r.err = true;
                CreateHow::Unchecked
            }
        };
        if !r.err {
            match how {
                CreateHow::Unchecked | CreateHow::Guarded => a = read_sattr3(r),
                CreateHow::Exclusive => {
                    let b = r.bytes(8);
                    verf[..b.len()].copy_from_slice(b);
                }
            }
        }
        if r.err {
            return Vec::new();
        }
        let res = op
            .dir
            .and_then(|d| self.fs.create(&d, op.name, how, &a, verf));
        self.made(res, &op.wcc)
    }

    fn nfs3_mkdir(&self, r: &mut Reader) -> Vec<u8> {
        let op = self.dirop3(r);
        let a = read_sattr3(r);
        if r.err {
            return Vec::new();
        }
        let res = op.dir.and_then(|d| self.fs.mkdir(&d, op.name, &a));
        self.made(res, &op.wcc)
    }

    fn nfs3_symlink(&self, r: &mut Reader) -> Vec<u8> {
        let op = self.dirop3(r);
        let a = read_sattr3(r);
        let target = r.str();
        if r.err {
            return Vec::new();
        }
        let res = op
            .dir
            .and_then(|d| self.fs.symlink(&d, op.name, target, &a));
        self.made(res, &op.wcc)
    }

    fn nfs3_remove(&self, r: &mut Reader, rmdir: bool) -> Vec<u8> {
        let op = self.dirop3(r);
        if r.err {
            return Vec::new();
        }
        let res = op.dir.and_then(|d| {
            if rmdir {
                self.fs.rmdir(&d, op.name)
            } else {
                self.fs.remove(&d, op.name)
            }
        });
        let mut w = Writer::new();
        w.u32(status_for(res.err()));
        self.write_wcc(&mut w, &op.wcc);
        w.b
    }

    fn nfs3_rename(&self, r: &mut Reader) -> Vec<u8> {
        let from = self.dirop3(r);
        let to = self.dirop3(r);
        if r.err {
            return Vec::new();
        }
        let res = match (&from.dir, &to.dir) {
            (Err(e), _) | (_, Err(e)) => Err(*e),
            (Ok(f), Ok(t)) => self.fs.rename(f, from.name, t, to.name),
        };
        let mut w = Writer::new();
        w.u32(status_for(res.err()));
        self.write_wcc(&mut w, &from.wcc);
        self.write_wcc(&mut w, &to.wcc);
        w.b
    }
}

/// Writes entries from `cookie` on while they fit, then the terminator
/// and eof. `fits` charges an entry against the budgets the caller
/// tracks; `emit` writes it. Returns false, having written nothing, when
/// not even the first remaining entry fits.
fn readdir_entries(
    w: &mut Writer,
    ents: &[DirEntry],
    cookie: u64,
    mut fits: impl FnMut(&DirEntry) -> bool,
    mut emit: impl FnMut(&mut Writer, &DirEntry, u64),
) -> bool {
    let start = readdir_start(ents, cookie);
    let mut i = start;
    while i < ents.len() {
        if !fits(&ents[i]) {
            if i == start {
                return false;
            }
            break;
        }
        w.bool(true);
        emit(w, &ents[i], entry_cookie(ents, i));
        i += 1;
    }
    w.bool(false);
    w.bool(i >= ents.len());
    true
}
