//! The socket thread, and the handle the emulator's threads use.

use crate::engine::*;
use crate::lines::Waker;
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

struct Inner {
    engine: Mutex<Engine>,
    waker: Arc<Waker>,
    stop: AtomicBool,
}

/// A cloneable handle to a running producer, for the host's threads.
#[derive(Clone)]
pub struct Handle(Arc<Inner>);

impl Handle {
    /// Run `f` on the producer's state under its lock, then wake the socket
    /// thread to send whatever `f` queued. Do not call this from a `Host` method.
    pub fn with<R>(&self, f: impl FnOnce(&mut Engine) -> R) -> R {
        let r = f(&mut self.0.engine.lock().unwrap());
        self.0.waker.wake();
        r
    }
}

pub struct Producer {
    handle: Handle,
    join: Option<JoinHandle<()>>,
    path: Option<PathBuf>,
}

impl Producer {
    pub fn waker() -> io::Result<Arc<Waker>> {
        Waker::new()
    }

    /// Serve on an already-listening socket (the launcher's inherited descriptor).
    pub fn start(
        cfg: Config,
        listener: UnixListener,
        host: Box<dyn Host>,
        waker: Arc<Waker>,
    ) -> io::Result<Producer> {
        if cfg!(target_endian = "big") {
            return Err(io::Error::other("big-endian hosts are not supported"));
        }
        listener.set_nonblocking(true)?;
        let engine = Engine::new(cfg, host, waker.clone())?;
        let inner = Arc::new(Inner {
            engine: Mutex::new(engine),
            waker,
            stop: AtomicBool::new(false),
        });
        let i2 = inner.clone();
        let join = std::thread::Builder::new()
            .name("display-producer".into())
            .spawn(move || serve(i2, listener))?;
        Ok(Producer {
            handle: Handle(inner),
            join: Some(join),
            path: None,
        })
    }

    /// Bind `path` inside a directory this user owns that no one else can
    /// write, creating it 0700 if absent, and refuse a path that answers.
    pub fn start_at(
        cfg: Config,
        path: &Path,
        host: Box<dyn Host>,
        waker: Arc<Waker>,
    ) -> io::Result<Producer> {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let dir = path
            .parent()
            .ok_or_else(|| io::Error::other("socket path has no directory"))?;
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
        if std::os::unix::net::UnixStream::connect(path).is_ok() {
            return Err(io::Error::other("socket path is in use"));
        }
        let _ = std::fs::remove_file(path);
        let listener = UnixListener::bind(path)?;
        let mut p = Producer::start(cfg, listener, host, waker)?;
        p.path = Some(path.to_path_buf());
        Ok(p)
    }

    pub fn handle(&self) -> Handle {
        self.handle.clone()
    }

    pub fn with<R>(&self, f: impl FnOnce(&mut Engine) -> R) -> R {
        self.handle.with(f)
    }

    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        self.handle.0.stop.store(true, Ordering::SeqCst);
        self.handle.0.waker.wake();
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
        if let Some(p) = self.path.take() {
            let _ = std::fs::remove_file(p);
        }
    }
}

impl Drop for Producer {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn serve(inner: Arc<Inner>, listener: UnixListener) {
    loop {
        let mut fds = vec![
            libc::pollfd {
                fd: listener.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: inner.waker.0.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        let ids: Vec<u64> = {
            let e = inner.engine.lock().unwrap();
            let mut ids = vec![];
            for v in &e.viewers {
                let mut ev = libc::POLLIN;
                if !v.out.is_empty() && !v.hold_writes {
                    ev |= libc::POLLOUT;
                }
                fds.push(libc::pollfd {
                    fd: v.sock.as_raw_fd(),
                    events: ev,
                    revents: 0,
                });
                ids.push(v.serial());
            }
            ids
        };
        // SAFETY: a valid pollfd array of the stated length.
        let r = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, -1) };
        if r < 0 && io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
            return;
        }
        if inner.stop.load(Ordering::SeqCst) {
            return;
        }
        let mut e = inner.engine.lock().unwrap();
        if fds[1].revents != 0 {
            e.waker.drain();
        }
        if fds[0].revents & libc::POLLIN != 0 {
            while let Ok((s, _)) = listener.accept() {
                e.accept(s);
            }
        }
        for (k, serial) in ids.iter().enumerate() {
            if fds[2 + k].revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0 {
                if let Some(vi) = e.viewers.iter().position(|v| v.serial() == *serial) {
                    e.on_readable(vi);
                }
            }
        }
        e.bottom_half();
        loop {
            for vi in 0..e.viewers.len() {
                e.flush(vi);
            }
            let Some(vi) = e.viewers.iter().position(|v| v.gone) else {
                break;
            };
            e.drop_viewer(vi);
            e.bottom_half();
        }
    }
}
