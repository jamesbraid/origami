//! The sockets QEMU's stream netdevs connect to: loopback TCP everywhere,
//! and Unix sockets where the platform has them, which cost the kernel
//! less per frame (QEMU writes each frame with its own `sendmsg`).

use std::io::{self, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
#[cfg(unix)]
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};

/// Where a listener is, as QEMU is told to connect to it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Endpoint {
    Tcp(SocketAddr),
    Unix(PathBuf),
}

/// Where to listen.
#[derive(Clone, Debug)]
pub enum Bind {
    Tcp(SocketAddr),
    /// A socket file to create. It is removed when the listener goes.
    Unix(PathBuf),
}

pub enum Listener {
    Tcp(TcpListener),
    #[cfg(unix)]
    Unix(UnixListener, PathBuf),
}

impl Listener {
    pub fn bind(bind: &Bind) -> io::Result<Listener> {
        match bind {
            Bind::Tcp(addr) => Ok(Listener::Tcp(TcpListener::bind(addr)?)),
            #[cfg(unix)]
            Bind::Unix(path) => Ok(Listener::Unix(UnixListener::bind(path)?, path.clone())),
            #[cfg(not(unix))]
            Bind::Unix(_) => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Unix sockets are not available here",
            )),
        }
    }

    pub fn endpoint(&self) -> io::Result<Endpoint> {
        match self {
            Listener::Tcp(l) => Ok(Endpoint::Tcp(l.local_addr()?)),
            #[cfg(unix)]
            Listener::Unix(_, path) => Ok(Endpoint::Unix(path.clone())),
        }
    }

    pub fn accept(&self) -> io::Result<Conn> {
        match self {
            Listener::Tcp(l) => {
                let (s, _) = l.accept()?;
                // Frames are sent as soon as they are whole.
                let _ = s.set_nodelay(true);
                Ok(Conn::Tcp(s))
            }
            #[cfg(unix)]
            Listener::Unix(l, _) => Ok(Conn::Unix(l.accept()?.0)),
        }
    }
}

impl Endpoint {
    /// Connects and drops at once, to wake a thread blocked in `accept`.
    pub fn poke(&self) {
        match self {
            Endpoint::Tcp(addr) => {
                let _ = TcpStream::connect(addr);
            }
            #[cfg(unix)]
            Endpoint::Unix(path) => {
                let _ = UnixStream::connect(path);
            }
            #[cfg(not(unix))]
            Endpoint::Unix(_) => {}
        }
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Listener::Unix(_, path) = self {
            let _ = std::fs::remove_file(path);
        }
    }
}

pub enum Conn {
    Tcp(TcpStream),
    #[cfg(unix)]
    Unix(UnixStream),
}

impl Conn {
    pub fn try_clone(&self) -> io::Result<Conn> {
        match self {
            Conn::Tcp(s) => Ok(Conn::Tcp(s.try_clone()?)),
            #[cfg(unix)]
            Conn::Unix(s) => Ok(Conn::Unix(s.try_clone()?)),
        }
    }

    pub fn shutdown(&self, how: Shutdown) -> io::Result<()> {
        match self {
            Conn::Tcp(s) => s.shutdown(how),
            #[cfg(unix)]
            Conn::Unix(s) => s.shutdown(how),
        }
    }
}

impl Read for Conn {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Conn::Tcp(s) => s.read(buf),
            #[cfg(unix)]
            Conn::Unix(s) => s.read(buf),
        }
    }
}

impl Write for Conn {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Conn::Tcp(s) => s.write(buf),
            #[cfg(unix)]
            Conn::Unix(s) => s.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// A directory for the switch's Unix sockets, private to this user. None
/// where there are no Unix sockets or no short enough path for one
/// (`sun_path` holds about 100 bytes).
#[cfg(unix)]
pub fn socket_dir() -> Option<PathBuf> {
    use std::os::unix::fs::DirBuilderExt;
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|p| p.is_dir())
        .unwrap_or_else(std::env::temp_dir);
    let dir = base.join(format!("origami-sw-{}-{}", std::process::id(), next_id()));
    if dir.as_os_str().len() > 80 {
        return None;
    }
    std::fs::DirBuilder::new().mode(0o700).create(&dir).ok()?;
    Some(dir)
}

#[cfg(not(unix))]
pub fn socket_dir() -> Option<PathBuf> {
    None
}

#[cfg(unix)]
fn next_id() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    N.fetch_add(1, Ordering::Relaxed)
}

/// Removes a directory made by [`socket_dir`], and anything left in it.
pub fn remove_dir(dir: &Path) {
    let _ = std::fs::remove_dir_all(dir);
}
