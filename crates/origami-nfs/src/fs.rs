//! The filesystem interface the protocol code serves, and the types it
//! trades in.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// A failure an [`FileSystem`] reports. Each maps to an NFS status.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Error {
    NotFound,
    NotDir,
    Stale,
    Io,
    /// A handle this server could never have issued.
    BadHandle,
    Exist,
    IsDir,
    NotEmpty,
    Inval,
    Access,
    Perm,
    NoSpace,
    Quota,
    NameTooLong,
    FBig,
    XDev,
    Rofs,
    NotSupp,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Error::NotFound => "no such file",
            Error::NotDir => "not a directory",
            Error::Stale => "stale handle",
            Error::Io => "io error",
            Error::BadHandle => "not a handle this server issues",
            Error::Exist => "file exists",
            Error::IsDir => "is a directory",
            Error::NotEmpty => "directory not empty",
            Error::Inval => "invalid argument",
            Error::Access => "permission denied",
            Error::Perm => "operation not permitted",
            Error::NoSpace => "no space left",
            Error::Quota => "quota exceeded",
            Error::NameTooLong => "name too long",
            Error::FBig => "file too large",
            Error::XDev => "cross-device rename",
            Error::Rofs => "read-only filesystem",
            Error::NotSupp => "operation not supported",
        };
        f.write_str(s)
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

/// A point in time, as seconds since the Unix epoch and nanoseconds past
/// the second.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Time {
    pub sec: i64,
    pub nsec: u32,
}

impl Time {
    pub fn new(sec: i64, nsec: u32) -> Time {
        Time { sec, nsec }
    }

    /// Like Go's time.Unix: `nsec` may lie outside 0..1e9 and carries into
    /// the seconds.
    pub fn unix(sec: i64, nsec: i64) -> Time {
        Time {
            sec: sec + nsec.div_euclid(1_000_000_000),
            nsec: nsec.rem_euclid(1_000_000_000) as u32,
        }
    }

    pub fn now() -> Time {
        Time::from_system(SystemTime::now())
    }

    pub fn from_system(t: SystemTime) -> Time {
        match t.duration_since(UNIX_EPOCH) {
            Ok(d) => Time {
                sec: d.as_secs() as i64,
                nsec: d.subsec_nanos(),
            },
            Err(e) => {
                // Before the epoch: seconds floor, nanoseconds count up.
                let d = e.duration();
                let (s, n) = (d.as_secs() as i64, d.subsec_nanos());
                if n == 0 {
                    Time { sec: -s, nsec: 0 }
                } else {
                    Time {
                        sec: -s - 1,
                        nsec: 1_000_000_000 - n,
                    }
                }
            }
        }
    }

    pub fn to_system(self) -> SystemTime {
        if self.sec >= 0 {
            UNIX_EPOCH + Duration::new(self.sec as u64, self.nsec)
        } else {
            UNIX_EPOCH - Duration::new(self.sec.unsigned_abs(), 0) + Duration::new(0, self.nsec)
        }
    }
}

/// A node's attributes. `mode` is a Unix mode with type bits.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Attr {
    pub mode: u32,
    pub nlink: u32,
    pub size: u64,
    /// Bytes of storage in use.
    pub used: u64,
    pub fileid: u64,
    pub atime: Time,
    pub mtime: Time,
    pub ctime: Time,
}

/// What a handle carries to name a file. With a path table `ino` is the
/// table id and the others are zero.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Key {
    pub dev: u64,
    pub ino: u64,
    pub generation: u64,
}

/// One filesystem object as last seen. `path`, `raw_mode` and `key` belong
/// to the [`FileSystem`] that made the node.
#[derive(Clone, Debug)]
pub struct Node {
    pub attr: Attr,
    pub path: Vec<u8>,
    /// The host's mode with type bits, setuid included.
    pub raw_mode: u32,
    pub key: Key,
    /// Names the node in the log when it is a file the client opens.
    pub describe: Option<String>,
    /// Which of several filesystems made the node, for [`Shares`]. A
    /// filesystem that stands alone leaves it zero.
    ///
    /// [`Shares`]: crate::Shares
    pub owner: usize,
}

impl Node {
    /// The node's id, which for a filesystem without its own handles is
    /// also the filehandle.
    pub fn id(&self) -> u64 {
        self.attr.fileid
    }

    pub fn mode(&self) -> u32 {
        self.attr.mode
    }

    pub fn size(&self) -> i64 {
        self.attr.size.min(i64::MAX as u64) as i64
    }

    pub fn is_dir(&self) -> bool {
        self.raw_mode & 0o170000 == 0o040000
    }

    pub fn is_reg(&self) -> bool {
        self.raw_mode & 0o170000 == 0o100000
    }

    pub fn is_link(&self) -> bool {
        self.raw_mode & 0o170000 == 0o120000
    }
}

/// One directory entry. A nonzero `cookie` is the entry's READDIR cookie:
/// a value that stays put while the directory changes around it and fits
/// in 32 bits for NFSv2. Entries with cookies come sorted by them, and a
/// listing resumes after the last cookie the client saw. Entries without
/// one are numbered by position.
#[derive(Clone, Debug)]
pub struct DirEntry {
    pub name: Vec<u8>,
    pub node: Node,
    pub cookie: u32,
}

/// The attributes a client asks to change. A `None` field is left alone.
/// Owners are not here: every file is shown owned by root, and a chown
/// succeeds without changing anything.
#[derive(Clone, Debug, Default)]
pub struct SetAttr {
    pub mode: Option<u32>,
    pub size: Option<u64>,
    pub atime: Option<Time>,
    pub mtime: Option<Time>,
}

/// How CREATE treats an existing name (RFC 1813 section 3.3.8).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum CreateHow {
    /// Opens an existing file and applies the attributes.
    Unchecked,
    /// Fails with [`Error::Exist`] if the name exists.
    Guarded,
    /// Creates the file with a verifier recorded in it, and succeeds again
    /// for a retransmission carrying the same verifier.
    Exclusive,
}

/// Filesystem capacity, in bytes and in files.
#[derive(Copy, Clone, Debug, Default)]
pub struct Space {
    pub total: u64,
    pub free: u64,
    pub avail: u64,
    pub files: u64,
    pub free_files: u64,
    pub avail_files: u64,
}

/// The tree the server exports. Reads are required; a filesystem that
/// returns true from [`writable`](FileSystem::writable) implements the
/// changing methods too, and without it every change is refused with
/// read-only-filesystem.
pub trait FileSystem: Send + Sync {
    /// Resolves a mount path to its root node. A client mounts one
    /// export; the path chooses which.
    fn mount_root(&self, path: &[u8]) -> Result<Node>;
    /// Finds a node by id, for a filesystem that keeps the default
    /// handles, which carry the id.
    fn node_by_id(&self, id: u64) -> Result<Node>;
    fn lookup(&self, dir: &Node, name: &[u8]) -> Result<Node>;
    fn read_dir(&self, dir: &Node) -> Result<Vec<DirEntry>>;
    fn read_at(&self, n: &Node, buf: &mut [u8], off: u64) -> Result<usize>;
    fn readlink(&self, n: &Node) -> Result<Vec<u8>>;

    /// The paths a client may mount, for MOUNT's EXPORT.
    fn exports(&self) -> Vec<Vec<u8>> {
        vec![b"/".to_vec()]
    }

    /// Makes the 32-byte filehandle for a node. The default carries the
    /// node's id.
    fn handle(&self, n: &Node) -> Vec<u8> {
        encode_fh(n.id()).to_vec()
    }

    /// Finds the node a handle names: [`Error::BadHandle`] for bytes this
    /// filesystem could never have issued, [`Error::Stale`] for one whose
    /// file is gone.
    fn node_by_handle(&self, fh: &[u8]) -> Result<Node> {
        match decode_fh(fh) {
            Some(id) => self.node_by_id(id),
            None => Err(Error::BadHandle),
        }
    }

    /// Reports the capacity of the filesystem. Without it the server
    /// reports none, which suits read-only media.
    fn space(&self) -> Result<Space> {
        Ok(Space::default())
    }

    fn writable(&self) -> bool {
        false
    }

    fn set_attr(&self, _n: &Node, _a: &SetAttr) -> Result<()> {
        Err(Error::Rofs)
    }

    fn write_at(&self, _n: &Node, _data: &[u8], _off: u64) -> Result<()> {
        Err(Error::Rofs)
    }

    fn create(
        &self,
        _dir: &Node,
        _name: &[u8],
        _how: CreateHow,
        _a: &SetAttr,
        _verf: [u8; 8],
    ) -> Result<Node> {
        Err(Error::Rofs)
    }

    fn mkdir(&self, _dir: &Node, _name: &[u8], _a: &SetAttr) -> Result<Node> {
        Err(Error::Rofs)
    }

    fn symlink(&self, _dir: &Node, _name: &[u8], _target: &[u8], _a: &SetAttr) -> Result<Node> {
        Err(Error::Rofs)
    }

    fn remove(&self, _dir: &Node, _name: &[u8]) -> Result<()> {
        Err(Error::Rofs)
    }

    fn rmdir(&self, _dir: &Node, _name: &[u8]) -> Result<()> {
        Err(Error::Rofs)
    }

    fn rename(&self, _from_dir: &Node, _from: &[u8], _to_dir: &Node, _to: &[u8]) -> Result<()> {
        Err(Error::Rofs)
    }
}

pub(crate) const FH_SIZE: usize = 32;

/// Packs a node id into the 32-byte filehandle of a filesystem without
/// its own: a version byte, then the id, zero-padded.
pub fn encode_fh(id: u64) -> [u8; FH_SIZE] {
    let mut fh = [0u8; FH_SIZE];
    fh[0] = 1;
    fh[1..9].copy_from_slice(&id.to_be_bytes());
    fh
}

pub fn decode_fh(fh: &[u8]) -> Option<u64> {
    if fh.len() != FH_SIZE || fh[0] != 1 {
        return None;
    }
    Some(u64::from_be_bytes(fh[1..9].try_into().unwrap()))
}

/// FNV-1a, 32 bits. Share ids and READDIR cookies are hashes of names.
pub fn fnv1a32(data: &[u8]) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for &b in data {
        h ^= b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}
