//! The host directory on Linux and macOS. Every path is resolved one
//! component at a time with `O_NOFOLLOW` from a descriptor for the share's
//! directory, so a symlink swapped in under a path the server already
//! checked is refused rather than followed out of the share.

use std::ffi::{CStr, CString};
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::raw::{c_int, c_uint};
use std::os::unix::fs::FileExt;
use std::path::Path;

use crate::fs::{Error, Space, Time};

/// The host has inode numbers a handle can carry.
pub(super) const INODES_USABLE: bool = true;

/// Characters, beyond '/', that a name may not hold.
pub(super) const BAD_NAME_CHARS: &[u8] = b"";

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

#[cfg(target_os = "linux")]
const DIR_SEARCH: c_int = libc::O_PATH;
#[cfg(not(target_os = "linux"))]
const DIR_SEARCH: c_int = libc::O_RDONLY;

pub(super) struct Host {
    root: OwnedFd,
    dir: std::path::PathBuf,
}

fn einval() -> io::Error {
    io::Error::from_raw_os_error(libc::EINVAL)
}

fn cstr(b: &[u8]) -> io::Result<CString> {
    CString::new(b).map_err(|_| einval())
}

fn cvt(r: c_int) -> io::Result<c_int> {
    if r < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(r)
    }
}

fn openat(dirfd: c_int, name: &CStr, flags: c_int, mode: c_uint) -> io::Result<OwnedFd> {
    let fd = cvt(unsafe { libc::openat(dirfd, name.as_ptr(), flags | libc::O_CLOEXEC, mode) })?;
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// Splits a share path into its elements. Only plain names are allowed:
/// the caller has checked, and this refuses again.
fn split(rel: &[u8]) -> io::Result<Vec<&[u8]>> {
    if rel == b"." {
        return Ok(Vec::new());
    }
    let comps: Vec<&[u8]> = rel.split(|&b| b == b'/').collect();
    if comps
        .iter()
        .any(|c| c.is_empty() || *c == b"." || *c == b"..")
    {
        return Err(einval());
    }
    Ok(comps)
}

fn ts(sec: i64, nsec: i64) -> Time {
    Time::new(sec, nsec as u32)
}

impl Host {
    pub fn open(dir: &Path) -> io::Result<Host> {
        let c = CString::new(std::os::unix::ffi::OsStrExt::as_bytes(dir.as_os_str()))
            .map_err(|_| einval())?;
        let root = openat(libc::AT_FDCWD, &c, libc::O_RDONLY | libc::O_DIRECTORY, 0)?;
        Ok(Host {
            root,
            dir: dir.to_path_buf(),
        })
    }

    /// The directory holding the last element of `rel`, as a descriptor,
    /// and that element's name. The root is its own directory, named ".".
    fn locate(&self, rel: &[u8]) -> io::Result<(OwnedFd, CString)> {
        let comps = split(rel)?;
        let mut cur = self.root.try_clone()?;
        let Some((last, dirs)) = comps.split_last() else {
            return Ok((cur, cstr(b".")?));
        };
        for d in dirs {
            cur = openat(
                cur.as_raw_fd(),
                &cstr(d)?,
                libc::O_DIRECTORY | libc::O_NOFOLLOW | DIR_SEARCH,
                0,
            )?;
        }
        Ok((cur, cstr(last)?))
    }

    /// Opens the file at `rel` without following a symlink at its end.
    fn open_file(&self, rel: &[u8], flags: c_int, mode: c_uint) -> io::Result<File> {
        let (dir, name) = self.locate(rel)?;
        let fd = openat(
            dir.as_raw_fd(),
            &name,
            flags | libc::O_NOFOLLOW | libc::O_NONBLOCK,
            mode,
        )?;
        Ok(File::from(fd))
    }

    /// Describes the file at `rel`, without following a symlink at its
    /// end.
    pub fn lstat(&self, rel: &[u8]) -> io::Result<HostStat> {
        let (dir, name) = self.locate(rel)?;
        lstat_at(dir.as_raw_fd(), &name)
    }

    /// The names in a directory, in the order the host gives them.
    pub fn list(&self, rel: &[u8]) -> io::Result<Vec<Vec<u8>>> {
        let comps = split(rel)?;
        let mut cur = self.root.try_clone()?;
        for d in &comps {
            cur = openat(
                cur.as_raw_fd(),
                &cstr(d)?,
                libc::O_DIRECTORY | libc::O_NOFOLLOW | DIR_SEARCH,
                0,
            )?;
        }
        // A fresh open for reading: a duplicate of the root descriptor
        // would share its position with every other listing.
        let cur = openat(
            cur.as_raw_fd(),
            &cstr(b".")?,
            libc::O_DIRECTORY | libc::O_RDONLY,
            0,
        )?;
        let dirp = unsafe { libc::fdopendir(std::os::fd::IntoRawFd::into_raw_fd(cur)) };
        if dirp.is_null() {
            return Err(io::Error::last_os_error());
        }
        let mut out = Vec::new();
        loop {
            // readdir returns null at the end and on error alike, and
            // only errno tells them apart.
            unsafe { *errno_location() = 0 };
            let ent = unsafe { libc::readdir(dirp) };
            if ent.is_null() {
                let e = io::Error::last_os_error();
                unsafe { libc::closedir(dirp) };
                return match e.raw_os_error() {
                    Some(0) | None => Ok(out),
                    _ => Err(e),
                };
            }
            let name = unsafe { CStr::from_ptr((*ent).d_name.as_ptr()) }.to_bytes();
            if name != b"." && name != b".." {
                out.push(name.to_vec());
            }
        }
    }

    pub fn read_at(&self, rel: &[u8], buf: &mut [u8], off: u64) -> io::Result<usize> {
        let f = self.open_file(rel, libc::O_RDONLY, 0)?;
        let mut n = 0;
        while n < buf.len() {
            match f.read_at(&mut buf[n..], off + n as u64) {
                Ok(0) => break,
                Ok(k) => n += k,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        Ok(n)
    }

    pub fn write_at(&self, rel: &[u8], data: &[u8], off: u64) -> io::Result<()> {
        let f = self.open_file(rel, libc::O_WRONLY, 0)?;
        f.write_all_at(data, off)
    }

    pub fn truncate(&self, rel: &[u8], size: u64) -> io::Result<()> {
        let f = self.open_file(rel, libc::O_WRONLY, 0)?;
        f.set_len(size)
    }

    /// Sets permission bits, setgid and sticky included, on the file
    /// itself, not on what a symlink there points to.
    pub fn chmod(&self, rel: &[u8], mode: u32) -> io::Result<()> {
        let (dir, name) = self.locate(rel)?;
        cvt(unsafe {
            libc::fchmodat(
                dir.as_raw_fd(),
                name.as_ptr(),
                mode as libc::mode_t,
                libc::AT_SYMLINK_NOFOLLOW,
            )
        })?;
        Ok(())
    }

    /// Sets the times that are Some and leaves the others.
    pub fn chtimes(&self, rel: &[u8], atime: Option<Time>, mtime: Option<Time>) -> io::Result<()> {
        let (dir, name) = self.locate(rel)?;
        let spec = |t: Option<Time>| match t {
            Some(t) => libc::timespec {
                tv_sec: t.sec as libc::time_t,
                tv_nsec: t.nsec as _,
            },
            None => libc::timespec {
                tv_sec: 0,
                tv_nsec: libc::UTIME_OMIT as _,
            },
        };
        let times = [spec(atime), spec(mtime)];
        cvt(unsafe {
            libc::utimensat(
                dir.as_raw_fd(),
                name.as_ptr(),
                times.as_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        })?;
        Ok(())
    }

    /// Makes a regular file, failing if it exists when `exclusive`.
    pub fn create(&self, rel: &[u8], exclusive: bool, perm: u32) -> io::Result<()> {
        let mut flags = libc::O_WRONLY | libc::O_CREAT;
        if exclusive {
            flags |= libc::O_EXCL;
        }
        self.open_file(rel, flags, perm as c_uint).map(drop)
    }

    pub fn mkdir(&self, rel: &[u8], perm: u32) -> io::Result<()> {
        let (dir, name) = self.locate(rel)?;
        cvt(unsafe { libc::mkdirat(dir.as_raw_fd(), name.as_ptr(), perm as libc::mode_t) })?;
        Ok(())
    }

    /// Stores `target` as given; nothing follows it.
    pub fn symlink(&self, target: &[u8], rel: &[u8]) -> io::Result<()> {
        let (dir, name) = self.locate(rel)?;
        let t = cstr(target)?;
        cvt(unsafe { libc::symlinkat(t.as_ptr(), dir.as_raw_fd(), name.as_ptr()) })?;
        Ok(())
    }

    pub fn readlink(&self, rel: &[u8]) -> io::Result<Vec<u8>> {
        let (dir, name) = self.locate(rel)?;
        let mut buf = vec![0u8; 4096];
        loop {
            let n = cvt(unsafe {
                libc::readlinkat(
                    dir.as_raw_fd(),
                    name.as_ptr(),
                    buf.as_mut_ptr() as *mut libc::c_char,
                    buf.len(),
                ) as c_int
            })? as usize;
            if n < buf.len() {
                buf.truncate(n);
                return Ok(buf);
            }
            buf.resize(buf.len() * 2, 0);
        }
    }

    /// Removes a file or symlink.
    pub fn unlink(&self, rel: &[u8]) -> io::Result<()> {
        let (dir, name) = self.locate(rel)?;
        cvt(unsafe { libc::unlinkat(dir.as_raw_fd(), name.as_ptr(), 0) })?;
        Ok(())
    }

    pub fn rmdir(&self, rel: &[u8]) -> io::Result<()> {
        let (dir, name) = self.locate(rel)?;
        cvt(unsafe { libc::unlinkat(dir.as_raw_fd(), name.as_ptr(), libc::AT_REMOVEDIR) })?;
        Ok(())
    }

    pub fn rename(&self, from: &[u8], to: &[u8]) -> io::Result<()> {
        let (fd, fname) = self.locate(from)?;
        let (td, tname) = self.locate(to)?;
        cvt(unsafe {
            libc::renameat(
                fd.as_raw_fd(),
                fname.as_ptr(),
                td.as_raw_fd(),
                tname.as_ptr(),
            )
        })?;
        Ok(())
    }

    /// The capacity of the filesystem holding the share.
    pub fn space(&self) -> io::Result<Space> {
        let c = cstr(std::os::unix::ffi::OsStrExt::as_bytes(self.dir.as_os_str()))?;
        let mut st: libc::statfs = unsafe { std::mem::zeroed() };
        cvt(unsafe { libc::statfs(c.as_ptr(), &mut st) })?;
        let bs = st.f_bsize as u64;
        Ok(Space {
            total: st.f_blocks as u64 * bs,
            free: st.f_bfree as u64 * bs,
            avail: st.f_bavail as u64 * bs,
            files: st.f_files as u64,
            free_files: st.f_ffree as u64,
            avail_files: st.f_ffree as u64,
        })
    }

    /// Whether `st` is the file a handle with `key` names.
    pub fn same_inode(st: &HostStat, dev: u64, ino: u64) -> bool {
        st.dev == dev && st.ino == ino
    }
}

#[cfg(target_os = "linux")]
fn errno_location() -> *mut c_int {
    unsafe { libc::__errno_location() }
}

#[cfg(target_os = "macos")]
fn errno_location() -> *mut c_int {
    unsafe { libc::__error() }
}

#[cfg(target_os = "linux")]
fn lstat_at(dirfd: c_int, name: &CStr) -> io::Result<HostStat> {
    let mut stx: libc::statx = unsafe { std::mem::zeroed() };
    let r = unsafe {
        libc::statx(
            dirfd,
            name.as_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
            libc::STATX_BASIC_STATS | libc::STATX_BTIME,
            &mut stx,
        )
    };
    if r < 0 {
        let e = io::Error::last_os_error();
        // A kernel or sandbox without statx: plain stat, no birth time.
        return match e.raw_os_error() {
            Some(libc::ENOSYS) | Some(libc::EPERM) => fstatat(dirfd, name),
            _ => Err(e),
        };
    }
    let generation = if stx.stx_mask & libc::STATX_BTIME != 0 {
        (stx.stx_btime.tv_sec as u64)
            .wrapping_mul(1_000_000_000)
            .wrapping_add(stx.stx_btime.tv_nsec as u64)
    } else {
        0
    };
    Ok(HostStat {
        mode: stx.stx_mode as u32,
        nlink: stx.stx_nlink,
        size: stx.stx_size,
        used: stx.stx_blocks * 512,
        atime: ts(stx.stx_atime.tv_sec, stx.stx_atime.tv_nsec as i64),
        mtime: ts(stx.stx_mtime.tv_sec, stx.stx_mtime.tv_nsec as i64),
        ctime: ts(stx.stx_ctime.tv_sec, stx.stx_ctime.tv_nsec as i64),
        dev: libc::makedev(stx.stx_dev_major, stx.stx_dev_minor) as u64,
        ino: stx.stx_ino,
        generation,
    })
}

#[cfg(not(target_os = "linux"))]
fn lstat_at(dirfd: c_int, name: &CStr) -> io::Result<HostStat> {
    fstatat(dirfd, name)
}

fn fstatat(dirfd: c_int, name: &CStr) -> io::Result<HostStat> {
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    cvt(unsafe { libc::fstatat(dirfd, name.as_ptr(), &mut st, libc::AT_SYMLINK_NOFOLLOW) })?;
    #[cfg(target_os = "macos")]
    let generation = (st.st_birthtime as u64)
        .wrapping_mul(1_000_000_000)
        .wrapping_add(st.st_birthtime_nsec as u64);
    #[cfg(not(target_os = "macos"))]
    let generation = 0;
    Ok(HostStat {
        mode: st.st_mode as u32,
        nlink: st.st_nlink as u32,
        size: st.st_size.max(0) as u64,
        used: st.st_blocks.max(0) as u64 * 512,
        atime: ts(st.st_atime as i64, st.st_atime_nsec as i64),
        mtime: ts(st.st_mtime as i64, st.st_mtime_nsec as i64),
        ctime: ts(st.st_ctime as i64, st.st_ctime_nsec as i64),
        dev: st.st_dev as u64,
        ino: st.st_ino as u64,
        generation,
    })
}

/// Maps a host error to the error NFS reports.
pub(super) fn map_err(e: &io::Error) -> Error {
    match e.raw_os_error() {
        Some(libc::EPERM) => Error::Perm,
        Some(libc::EACCES) => Error::Access,
        Some(libc::EEXIST) => Error::Exist,
        Some(libc::ENOENT) => Error::NotFound,
        Some(libc::ENOTDIR) => Error::NotDir,
        Some(libc::EISDIR) => Error::IsDir,
        Some(libc::EINVAL) => Error::Inval,
        Some(libc::ENOSPC) => Error::NoSpace,
        Some(libc::EDQUOT) => Error::Quota,
        Some(libc::ENAMETOOLONG) => Error::NameTooLong,
        Some(libc::ENOTEMPTY) => Error::NotEmpty,
        Some(libc::EFBIG) => Error::FBig,
        Some(libc::EXDEV) => Error::XDev,
        Some(libc::EROFS) => Error::Rofs,
        _ => Error::Io,
    }
}
