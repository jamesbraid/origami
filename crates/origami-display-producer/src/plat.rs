//! What differs by operating system on the producer's side: who the peer
//! is, how a region's handle reaches it, the wake-up channel and a random id.

use origami_display::v4::sys::{Passed, RawPassed, UnixStream};
use std::io;

/// A region's handle as the producer holds it, with the access a viewer gets.
#[derive(Clone)]
pub struct Grant {
    pub handle: std::sync::Arc<Passed>,
    #[cfg_attr(unix, allow(dead_code))]
    pub writable: bool,
}

#[cfg(unix)]
mod imp {
    use super::*;
    use std::os::fd::{AsRawFd, FromRawFd};

    pub struct Peer;

    impl Peer {
        /// The peer if its user is ours or listed.
        pub fn of(sock: &UnixStream, allow: &[u32]) -> Option<Peer> {
            let uid = peer_uid(sock)?;
            // SAFETY: geteuid has no preconditions.
            (uid == unsafe { libc::geteuid() } || allow.contains(&uid)).then_some(Peer)
        }

        pub fn pid(&self) -> u32 {
            0
        }
    }

    #[cfg(target_os = "linux")]
    fn peer_uid(sock: &UnixStream) -> Option<u32> {
        let mut cred = libc::ucred {
            pid: 0,
            uid: 0,
            gid: 0,
        };
        let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
        // SAFETY: SO_PEERCRED fills a ucred of the stated size.
        let r = unsafe {
            libc::getsockopt(
                sock.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                (&mut cred as *mut libc::ucred).cast(),
                &mut len,
            )
        };
        (r == 0).then_some(cred.uid)
    }

    #[cfg(not(target_os = "linux"))]
    fn peer_uid(sock: &UnixStream) -> Option<u32> {
        let (mut uid, mut gid) = (0, 0);
        // SAFETY: getpeereid writes two ids.
        (unsafe { libc::getpeereid(sock.as_raw_fd(), &mut uid, &mut gid) } == 0).then_some(uid)
    }

    /// Before a message's first byte: the descriptor rides it.
    pub fn stage(
        _peer: &Peer,
        grant: &Grant,
        _msg: &mut [u8],
        _staged: &mut Option<u64>,
    ) -> io::Result<Option<RawPassed>> {
        Ok(Some(grant.handle.as_raw_fd()))
    }

    pub fn unstage(_peer: &Peer, _value: u64) {}

    pub fn linger(_sock: &UnixStream) {}

    pub fn random_u64() -> u64 {
        let mut b = [0u8; 8];
        #[cfg(target_os = "linux")]
        // SAFETY: getrandom fills our buffer.
        let ok = unsafe { libc::getrandom(b.as_mut_ptr().cast(), 8, 0) } == 8;
        #[cfg(not(target_os = "linux"))]
        // SAFETY: getentropy fills our buffer.
        let ok = unsafe { libc::getentropy(b.as_mut_ptr().cast(), 8) } == 0;
        if ok {
            u64::from_le_bytes(b) | 1
        } else {
            origami_display::v4::sys::now_ns() | 1
        }
    }

    /// Shared memory of `size` bytes: a memfd, sealed by [`seal`] once mapped.
    #[cfg(target_os = "linux")]
    pub fn create_section(size: u64) -> io::Result<Passed> {
        // SAFETY: memfd_create with a NUL-terminated name.
        let raw = unsafe {
            libc::memfd_create(
                c"origami-display".as_ptr(),
                libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
            )
        };
        if raw < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: raw is a fresh descriptor.
        let fd = unsafe { Passed::from_raw_fd(raw) };
        // SAFETY: ftruncate on our own memfd.
        if unsafe { libc::ftruncate(fd.as_raw_fd(), size as libc::off_t) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(fd)
    }

    /// Shared memory of `size` bytes: a POSIX shm object unlinked at once, so
    /// only descriptors reach it. macOS has no memfd and no seals.
    #[cfg(not(target_os = "linux"))]
    pub fn create_section(size: u64) -> io::Result<Passed> {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        // macOS limits shm names to 31 bytes.
        let name = std::ffi::CString::new(format!(
            "/od.{:x}.{:x}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ))
        .unwrap();
        // SAFETY: shm_open with a NUL-terminated name.
        let raw = unsafe {
            libc::shm_open(
                name.as_ptr(),
                libc::O_RDWR | libc::O_CREAT | libc::O_EXCL,
                0o600,
            )
        };
        if raw < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: raw is a fresh descriptor.
        let fd = unsafe { Passed::from_raw_fd(raw) };
        // SAFETY: the name is ours; the object lives on through `fd`. macOS
        // shm_open takes no O_CLOEXEC, so mark the descriptor here.
        unsafe {
            libc::shm_unlink(name.as_ptr());
            libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC);
        }
        // SAFETY: ftruncate on our own shm object (macOS allows it once).
        if unsafe { libc::ftruncate(fd.as_raw_fd(), size as libc::off_t) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(fd)
    }

    /// Seal `fd` once the producer has mapped it, so the viewer's descriptor
    /// cannot resize it (nor map it writable unless `viewer_writable`).
    #[cfg(target_os = "linux")]
    pub fn seal(fd: &Passed, viewer_writable: bool) -> io::Result<()> {
        use origami_display::v4::regions::{
            seals, SEAL_FUTURE_WRITE, SEAL_GROW, SEAL_SEAL, SEAL_SHRINK,
        };
        let mut want = SEAL_SHRINK | SEAL_GROW;
        if !viewer_writable {
            want |= SEAL_FUTURE_WRITE;
        }
        for s in [want, SEAL_SEAL] {
            // SAFETY: F_ADD_SEALS on our memfd.
            if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_ADD_SEALS, s) } != 0 {
                return Err(io::Error::last_os_error());
            }
        }
        let got = seals(fd)?;
        if got & (want | SEAL_SEAL) != (want | SEAL_SEAL) {
            return Err(io::Error::other("memfd refused a seal"));
        }
        Ok(())
    }

    /// Shared memory has no seals off Linux; the viewer skips the check too.
    /// Without them a connected peer (our uid or an allowed one) can map a read-only region writable.
    #[cfg(not(target_os = "linux"))]
    pub fn seal(_fd: &Passed, _viewer_writable: bool) -> io::Result<()> {
        Ok(())
    }

    /// The wake-up channel: an eventfd. Write end and read end are one descriptor.
    #[cfg(target_os = "linux")]
    pub struct Wake(std::os::fd::OwnedFd);

    #[cfg(target_os = "linux")]
    impl Wake {
        pub fn new() -> io::Result<Wake> {
            // SAFETY: eventfd returns a new descriptor we own.
            let fd = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: fd is valid and owned here.
            Ok(Wake(unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) }))
        }
        pub fn wake(&self) {
            let one: u64 = 1;
            // SAFETY: writing 8 bytes to an eventfd; a full counter is harmless.
            unsafe { libc::write(self.0.as_raw_fd(), (&one as *const u64).cast(), 8) };
        }
        pub fn drain(&self) {
            let mut v: u64 = 0;
            // SAFETY: reading 8 bytes from a non-blocking eventfd.
            unsafe { libc::read(self.0.as_raw_fd(), (&mut v as *mut u64).cast(), 8) };
        }
        pub fn sock(&self) -> origami_display::v4::sys::RawSock {
            self.0.as_raw_fd()
        }
    }

    /// The wake-up channel off Linux: a connected socket pair, as on Windows.
    #[cfg(not(target_os = "linux"))]
    pub struct Wake {
        rx: UnixStream,
        tx: UnixStream,
    }

    #[cfg(not(target_os = "linux"))]
    impl Wake {
        pub fn new() -> io::Result<Wake> {
            let (rx, tx) = UnixStream::pair()?;
            rx.set_nonblocking(true)?;
            tx.set_nonblocking(true)?;
            Ok(Wake { rx, tx })
        }
        pub fn wake(&self) {
            use std::io::Write;
            // A full buffer already means a wake-up is pending.
            let _ = (&self.tx).write(&[1]);
        }
        pub fn drain(&self) {
            use std::io::Read;
            let mut b = [0u8; 64];
            while matches!((&self.rx).read(&mut b), Ok(n) if n > 0) {}
        }
        pub fn sock(&self) -> origami_display::v4::sys::RawSock {
            self.rx.as_raw_fd()
        }
    }

    /// Wait for readiness on `socks` (read, plus write where flagged) and the
    /// listener. Returns one readiness flag per entry: `(readable, writable)`.
    /// Interrupted waits return all-false.
    pub fn poll(
        socks: &[(origami_display::v4::sys::RawSock, bool)],
    ) -> io::Result<Vec<(bool, bool)>> {
        let mut fds: Vec<libc::pollfd> = socks
            .iter()
            .map(|&(fd, write)| libc::pollfd {
                fd,
                events: libc::POLLIN | if write { libc::POLLOUT } else { 0 },
                revents: 0,
            })
            .collect();
        // SAFETY: a valid pollfd array of the stated length.
        let r = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, -1) };
        if r < 0 {
            let e = io::Error::last_os_error();
            return if e.kind() == io::ErrorKind::Interrupted {
                Ok(vec![(false, false); socks.len()])
            } else {
                Err(e)
            };
        }
        Ok(fds
            .iter()
            .map(|p| {
                (
                    p.revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0,
                    p.revents & libc::POLLOUT != 0,
                )
            })
            .collect())
    }

    /// Mode and ownership of the socket directory, then create it if absent.
    pub fn private_dir(dir: &std::path::Path) -> io::Result<()> {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if !dir.exists() {
            std::fs::create_dir_all(dir)?;
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
        let md = std::fs::metadata(dir)?;
        // SAFETY: geteuid has no preconditions.
        if md.uid() != unsafe { libc::geteuid() } || md.mode() & 0o022 != 0 {
            return Err(io::Error::other(
                "socket directory must be ours and not group or world writable",
            ));
        }
        Ok(())
    }
}

#[cfg(windows)]
mod imp {
    use super::*;
    use origami_display::v4::sys::AsSock;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
    use std::sync::atomic::{AtomicBool, Ordering};
    use windows_sys::Win32::Foundation::{
        DuplicateHandle, GetLastError, DUPLICATE_CLOSE_SOURCE, HANDLE, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Networking::WinSock as ws;
    use windows_sys::Win32::Security::{
        EqualSid, GetTokenInformation, TokenUser, TOKEN_QUERY, TOKEN_USER,
    };
    use windows_sys::Win32::System::Memory::{
        CreateFileMappingW, FILE_MAP_EXECUTE, FILE_MAP_READ, FILE_MAP_WRITE, PAGE_READWRITE,
        SECTION_QUERY,
    };
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_DUP_HANDLE,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };

    const READ_CONTROL: u32 = 0x2_0000;

    /// The process on the other end of a connection, opened for handle duplication.
    pub struct Peer {
        process: OwnedHandle,
        pid: u32,
    }

    fn user_sid(token: HANDLE) -> Option<Vec<u8>> {
        let mut need = 0u32;
        // SAFETY: a size query with no buffer.
        unsafe { GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut need) };
        if need == 0 {
            return None;
        }
        let mut buf = vec![0u8; need as usize];
        // SAFETY: `buf` is `need` bytes.
        let ok = unsafe {
            GetTokenInformation(token, TokenUser, buf.as_mut_ptr().cast(), need, &mut need)
        };
        (ok != 0).then_some(buf)
    }

    fn same_user(process: HANDLE) -> bool {
        let token_of = |p: HANDLE| -> Option<OwnedHandle> {
            let mut t: HANDLE = std::ptr::null_mut();
            // SAFETY: `t` receives a token handle we own.
            let ok = unsafe { OpenProcessToken(p, TOKEN_QUERY, &mut t) };
            // SAFETY: a fresh handle on success.
            (ok != 0).then(|| unsafe { OwnedHandle::from_raw_handle(t as RawHandle) })
        };
        // SAFETY: the pseudo handle of this process.
        let (Some(mine), Some(theirs)) =
            (token_of(unsafe { GetCurrentProcess() }), token_of(process))
        else {
            return false;
        };
        let (Some(a), Some(b)) = (
            user_sid(mine.as_raw_handle() as HANDLE),
            user_sid(theirs.as_raw_handle() as HANDLE),
        ) else {
            return false;
        };
        // SAFETY: both buffers start with a TOKEN_USER whose SID pointer stays inside them.
        unsafe {
            let ua = (a.as_ptr() as *const TOKEN_USER).read_unaligned();
            let ub = (b.as_ptr() as *const TOKEN_USER).read_unaligned();
            EqualSid(ua.User.Sid, ub.User.Sid) != 0
        }
    }

    impl Peer {
        /// The peer if its token's user is ours (`allow` lists user ids, a
        /// POSIX notion with no meaning here).
        pub fn of(sock: &UnixStream, _allow: &[u32]) -> Option<Peer> {
            let mut pid = 0u32;
            let mut got = 0u32;
            // SAFETY: SIO_AF_UNIX_GETPEERPID writes a u32.
            let r = unsafe {
                ws::WSAIoctl(
                    sock.sock() as ws::SOCKET,
                    ws::SIO_AF_UNIX_GETPEERPID,
                    std::ptr::null(),
                    0,
                    (&mut pid as *mut u32).cast(),
                    4,
                    &mut got,
                    std::ptr::null_mut(),
                    None,
                )
            };
            if r != 0 || pid == 0 {
                return None;
            }
            // SAFETY: plain open of a process by id.
            let p = unsafe {
                OpenProcess(
                    PROCESS_DUP_HANDLE | PROCESS_QUERY_LIMITED_INFORMATION,
                    0,
                    pid,
                )
            };
            if p.is_null() {
                return None;
            }
            // SAFETY: a fresh process handle.
            let process = unsafe { OwnedHandle::from_raw_handle(p as RawHandle) };
            same_user(process.as_raw_handle() as HANDLE).then_some(Peer { process, pid })
        }

        pub fn pid(&self) -> u32 {
            self.pid
        }
    }

    fn duplicate(peer: &Peer, grant: &Grant) -> io::Result<u64> {
        // Direct3D opens a section as a heap only with execute and READ_CONTROL
        // as well (measured on WARP), so a writable grant carries them.
        let access = SECTION_QUERY
            | FILE_MAP_READ
            | if grant.writable {
                FILE_MAP_WRITE | FILE_MAP_EXECUTE | READ_CONTROL
            } else {
                0
            };
        let mut out: HANDLE = std::ptr::null_mut();
        // SAFETY: duplicating our section into the peer with the access the
        // contract names, not the source's full access.
        let ok = unsafe {
            DuplicateHandle(
                GetCurrentProcess(),
                grant.handle.as_raw_handle() as HANDLE,
                peer.process.as_raw_handle() as HANDLE,
                &mut out,
                access,
                0,
                0,
            )
        };
        if ok == 0 {
            Err(io::Error::from_raw_os_error(
                unsafe { GetLastError() } as i32
            ))
        } else {
            Ok(out as usize as u64)
        }
    }

    /// Before a message's first byte: duplicate its handle into the peer (once
    /// per message, kept for retries) and write the value into the tail.
    pub fn stage(
        peer: &Peer,
        grant: &Grant,
        msg: &mut [u8],
        staged: &mut Option<u64>,
    ) -> io::Result<Option<RawPassed>> {
        let value = match *staged {
            Some(v) => v,
            None => {
                let v = duplicate(peer, grant)?;
                *staged = Some(v);
                v
            }
        };
        let at = msg.len() - 8;
        msg[at..].copy_from_slice(&value.to_le_bytes());
        Ok(None)
    }

    /// A duplicate whose message never started: close it in the peer, which
    /// cannot have seen the value.
    pub fn unstage(peer: &Peer, value: u64) {
        // SAFETY: closes the peer's handle by duplicating it onto nothing.
        unsafe {
            DuplicateHandle(
                peer.process.as_raw_handle() as HANDLE,
                value as usize as HANDLE,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                0,
                0,
                DUPLICATE_CLOSE_SOURCE,
            )
        };
    }

    /// Closing with unread input resets the socket and loses our final ERROR:
    /// stop writing and drain the peer's input briefly first.
    pub fn linger(sock: &UnixStream) {
        use std::io::Read;
        let _ = sock.shutdown(std::net::Shutdown::Write);
        let end = std::time::Instant::now() + std::time::Duration::from_millis(50);
        let mut b = [0u8; 4096];
        while std::time::Instant::now() < end {
            match (&*sock).read(&mut b) {
                Ok(0) => break,
                Ok(_) => {}
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    std::thread::sleep(std::time::Duration::from_millis(2))
                }
                Err(_) => break,
            }
        }
    }

    pub fn random_u64() -> u64 {
        use std::hash::{BuildHasher, Hasher};
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u64(origami_display::v4::sys::now_ns());
        h.finish() | 1
    }

    /// A pagefile-backed section of `size` bytes. Sections cannot be resized.
    pub fn create_section(size: u64) -> io::Result<Passed> {
        // SAFETY: an anonymous, unnamed section with default security.
        let h = unsafe {
            CreateFileMappingW(
                INVALID_HANDLE_VALUE,
                std::ptr::null(),
                PAGE_READWRITE,
                (size >> 32) as u32,
                size as u32,
                std::ptr::null(),
            )
        };
        if h.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: a fresh section handle.
        Ok(unsafe { OwnedHandle::from_raw_handle(h as RawHandle) })
    }

    /// Access to the viewer's copy is set when the handle is duplicated.
    pub fn seal(_h: &Passed, _viewer_writable: bool) -> io::Result<()> {
        Ok(())
    }

    /// The wake-up channel: a connected socket pair, since `WSAPoll` waits on
    /// sockets only.
    pub struct Wake {
        rx: UnixStream,
        tx: UnixStream,
        pending: AtomicBool,
    }

    impl Wake {
        pub fn new() -> io::Result<Wake> {
            let (rx, tx) = UnixStream::pair()?;
            rx.set_nonblocking(true)?;
            tx.set_nonblocking(true)?;
            Ok(Wake {
                rx,
                tx,
                pending: AtomicBool::new(false),
            })
        }
        pub fn wake(&self) {
            use std::io::Write;
            if !self.pending.swap(true, Ordering::AcqRel) {
                let _ = (&self.tx).write(&[1]);
            }
        }
        pub fn drain(&self) {
            use std::io::Read;
            let mut b = [0u8; 16];
            self.pending.store(false, Ordering::Release);
            while matches!((&self.rx).read(&mut b), Ok(n) if n > 0) {}
        }
        pub fn sock(&self) -> origami_display::v4::sys::RawSock {
            self.rx.sock()
        }
    }

    pub fn poll(
        socks: &[(origami_display::v4::sys::RawSock, bool)],
    ) -> io::Result<Vec<(bool, bool)>> {
        let mut fds: Vec<ws::WSAPOLLFD> = socks
            .iter()
            .map(|&(s, write)| ws::WSAPOLLFD {
                fd: s as ws::SOCKET,
                events: ws::POLLRDNORM | if write { ws::POLLWRNORM } else { 0 },
                revents: 0,
            })
            .collect();
        // SAFETY: a valid WSAPOLLFD array of the stated length.
        let r = unsafe { ws::WSAPoll(fds.as_mut_ptr(), fds.len() as u32, -1) };
        if r < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(fds
            .iter()
            .map(|p| {
                (
                    p.revents & (ws::POLLRDNORM | ws::POLLHUP | ws::POLLERR) != 0,
                    p.revents & ws::POLLWRNORM != 0,
                )
            })
            .collect())
    }

    /// Create the directory if absent. The DACL is inherited, so callers must
    /// place it under the per-user `%LOCALAPPDATA%` (narrower than the design's check).
    pub fn private_dir(dir: &std::path::Path) -> io::Result<()> {
        std::fs::create_dir_all(dir)
    }
}

pub use imp::*;
