//! The host directory on Windows. std has nothing like `os.Root`, so each
//! path is checked a component at a time before it is used: an ancestor
//! that is a symlink or other reparse point is refused. Unlike the Unix
//! walk, this leaves a window between the check and the use.

use std::fs::{self, File, FileTimes, OpenOptions};
use std::io;
use std::os::windows::fs::{FileExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use crate::fs::{Error, Space, Time};

/// Windows reports no inode numbers through std, so a share keeps a path
/// table.
pub(super) const INODES_USABLE: bool = false;

/// Characters, beyond '/', that a name may not hold: '\' is a separator
/// here too, and ':' names an alternate data stream.
pub(super) const BAD_NAME_CHARS: &[u8] = b"\\:";

const ERROR_DIR_NOT_EMPTY: i32 = 145;
const ERROR_DISK_FULL: i32 = 112;
const ERROR_HANDLE_DISK_FULL: i32 = 39;
const ERROR_FILENAME_EXCED_RANGE: i32 = 206;
const ERROR_DIRECTORY: i32 = 267;
const ERROR_NOT_SAME_DEVICE: i32 = 17;
const ERROR_INVALID_NAME: i32 = 123;

const FILE_ATTRIBUTE_READONLY: u32 = 0x1;
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
const FILE_WRITE_ATTRIBUTES: u32 = 0x100;

/// What the host reports for a file.
#[derive(Clone, Copy, Debug)]
pub(super) struct HostStat {
    pub mode: u32,
    pub nlink: u32,
    pub size: u64,
    pub used: u64,
    pub atime: Time,
    pub mtime: Time,
    pub ctime: Time,
    pub dev: u64,
    pub ino: u64,
    pub generation: u64,
}

pub(super) struct Host {
    dir: PathBuf,
}

fn err(code: i32) -> io::Error {
    io::Error::from_raw_os_error(code)
}

fn t(st: io::Result<std::time::SystemTime>) -> Time {
    st.map(Time::from_system).unwrap_or_default()
}

impl Host {
    pub fn open(dir: &Path) -> io::Result<Host> {
        Ok(Host {
            dir: dir.to_path_buf(),
        })
    }

    /// The host path for `rel`, after checking that no ancestor is a
    /// symlink or junction.
    fn real(&self, rel: &[u8]) -> io::Result<PathBuf> {
        let mut p = self.dir.clone();
        if rel == b"." {
            return Ok(p);
        }
        let s = std::str::from_utf8(rel).map_err(|_| err(ERROR_INVALID_NAME))?;
        let comps: Vec<&str> = s.split('/').collect();
        if comps
            .iter()
            .any(|c| c.is_empty() || *c == "." || *c == ".." || c.contains(['\\', ':']))
        {
            return Err(err(ERROR_INVALID_NAME));
        }
        for (i, c) in comps.iter().enumerate() {
            if i > 0 {
                let md = fs::symlink_metadata(&p)?;
                if md.file_type().is_symlink()
                    || md.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
                {
                    return Err(err(ERROR_DIRECTORY));
                }
                if !md.is_dir() {
                    return Err(err(ERROR_DIRECTORY));
                }
            }
            p.push(c);
        }
        Ok(p)
    }

    /// Windows keeps no inode numbers, so shares there always use a path
    /// table.
    pub fn has_birth_time(&self) -> bool {
        false
    }

    pub fn lstat(&self, rel: &[u8]) -> io::Result<HostStat> {
        let md = fs::symlink_metadata(self.real(rel)?)?;
        let ro = md.file_attributes() & FILE_ATTRIBUTE_READONLY != 0;
        let ft = md.file_type();
        let mode = if ft.is_dir() {
            0o040000 | if ro { 0o555 } else { 0o777 }
        } else if ft.is_symlink() {
            0o120000 | 0o777
        } else {
            0o100000 | if ro { 0o444 } else { 0o666 }
        };
        let size = if ft.is_dir() { 0 } else { md.len() };
        let mtime = t(md.modified());
        Ok(HostStat {
            mode,
            nlink: 1,
            size,
            used: (size + 4095) & !4095,
            atime: t(md.accessed()),
            mtime,
            ctime: mtime,
            dev: 0,
            ino: 0,
            generation: 0,
        })
    }

    pub fn list(&self, rel: &[u8]) -> io::Result<Vec<Vec<u8>>> {
        let mut out = Vec::new();
        for e in fs::read_dir(self.real(rel)?)? {
            out.push(e?.file_name().to_string_lossy().into_owned().into_bytes());
        }
        Ok(out)
    }

    fn open_file(&self, rel: &[u8], write: bool) -> io::Result<File> {
        let p = self.real(rel)?;
        if fs::symlink_metadata(&p)?.file_type().is_symlink() {
            return Err(err(ERROR_DIRECTORY));
        }
        OpenOptions::new().read(!write).write(write).open(p)
    }

    pub fn read_at(&self, rel: &[u8], buf: &mut [u8], off: u64) -> io::Result<usize> {
        let f = self.open_file(rel, false)?;
        let mut n = 0;
        while n < buf.len() {
            match f.seek_read(&mut buf[n..], off + n as u64) {
                Ok(0) => break,
                Ok(k) => n += k,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        Ok(n)
    }

    pub fn write_at(&self, rel: &[u8], data: &[u8], off: u64) -> io::Result<()> {
        let f = self.open_file(rel, true)?;
        let mut n = 0;
        while n < data.len() {
            match f.seek_write(&data[n..], off + n as u64) {
                Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                Ok(k) => n += k,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    pub fn truncate(&self, rel: &[u8], size: u64) -> io::Result<()> {
        self.open_file(rel, true)?.set_len(size)
    }

    /// Windows keeps one permission bit: read-only.
    pub fn chmod(&self, rel: &[u8], mode: u32) -> io::Result<()> {
        let p = self.real(rel)?;
        let mut perm = fs::metadata(&p)?.permissions();
        perm.set_readonly(mode & 0o200 == 0);
        fs::set_permissions(&p, perm)
    }

    pub fn chtimes(&self, rel: &[u8], atime: Option<Time>, mtime: Option<Time>) -> io::Result<()> {
        let f = OpenOptions::new()
            .access_mode(FILE_WRITE_ATTRIBUTES)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(self.real(rel)?)?;
        let mut times = FileTimes::new();
        if let Some(a) = atime {
            times = times.set_accessed(a.to_system());
        }
        if let Some(m) = mtime {
            times = times.set_modified(m.to_system());
        }
        f.set_times(times)
    }

    pub fn create(&self, rel: &[u8], exclusive: bool, _perm: u32) -> io::Result<()> {
        let p = self.real(rel)?;
        let mut o = OpenOptions::new();
        o.write(true);
        if exclusive {
            o.create_new(true);
        } else {
            o.create(true);
        }
        o.open(p).map(drop)
    }

    pub fn mkdir(&self, rel: &[u8], _perm: u32) -> io::Result<()> {
        fs::create_dir(self.real(rel)?)
    }

    pub fn symlink(&self, target: &[u8], rel: &[u8]) -> io::Result<()> {
        let link = self.real(rel)?;
        let t = std::str::from_utf8(target).map_err(|_| err(ERROR_INVALID_NAME))?;
        let t = t.replace('/', "\\");
        let resolved = link.parent().unwrap_or(Path::new("")).join(&t);
        if resolved.is_dir() {
            std::os::windows::fs::symlink_dir(&t, link)
        } else {
            std::os::windows::fs::symlink_file(&t, link)
        }
    }

    pub fn readlink(&self, rel: &[u8]) -> io::Result<Vec<u8>> {
        let t = fs::read_link(self.real(rel)?)?;
        Ok(t.to_string_lossy().replace('\\', "/").into_bytes())
    }

    pub fn unlink(&self, rel: &[u8]) -> io::Result<()> {
        let p = self.real(rel)?;
        // A symlink to a directory is removed as a directory.
        match fs::remove_file(&p) {
            Err(e) if fs::symlink_metadata(&p).is_ok_and(|m| m.file_type().is_symlink()) => {
                fs::remove_dir(&p).map_err(|_| e)
            }
            r => r,
        }
    }

    pub fn rmdir(&self, rel: &[u8]) -> io::Result<()> {
        fs::remove_dir(self.real(rel)?)
    }

    pub fn rename(&self, from: &[u8], to: &[u8]) -> io::Result<()> {
        fs::rename(self.real(from)?, self.real(to)?)
    }

    pub fn space(&self) -> io::Result<Space> {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
        let wide: Vec<u16> = self.dir.as_os_str().encode_wide().chain([0]).collect();
        let (mut avail, mut total, mut free) = (0u64, 0u64, 0u64);
        let ok = unsafe { GetDiskFreeSpaceExW(wide.as_ptr(), &mut avail, &mut total, &mut free) };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Space {
            total,
            free,
            avail,
            ..Space::default()
        })
    }

    /// Windows handles never carry an inode.
    pub fn same_inode(_st: &HostStat, _dev: u64, _ino: u64) -> bool {
        false
    }
}

/// Maps a host error to the error NFS reports.
pub(super) fn map_err(e: &io::Error) -> Error {
    match e.raw_os_error() {
        Some(ERROR_DIR_NOT_EMPTY) => return Error::NotEmpty,
        Some(ERROR_DISK_FULL) | Some(ERROR_HANDLE_DISK_FULL) => return Error::NoSpace,
        Some(ERROR_FILENAME_EXCED_RANGE) => return Error::NameTooLong,
        Some(ERROR_DIRECTORY) => return Error::NotDir,
        Some(ERROR_NOT_SAME_DEVICE) => return Error::XDev,
        Some(ERROR_INVALID_NAME) => return Error::Inval,
        _ => {}
    }
    match e.kind() {
        io::ErrorKind::NotFound => Error::NotFound,
        io::ErrorKind::AlreadyExists => Error::Exist,
        io::ErrorKind::PermissionDenied => Error::Access,
        _ => Error::Io,
    }
}
