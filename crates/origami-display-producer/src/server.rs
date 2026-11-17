//! The socket thread, and the handle the emulator's threads use.

use crate::engine::*;
use crate::lines::Waker;
use crate::plat;
use origami_display::v4::sys::{AsSock, UnixListener, UnixStream};
use std::io;
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
        let dir = path
            .parent()
            .ok_or_else(|| io::Error::other("socket path has no directory"))?;
        plat::private_dir(dir)?;
        if UnixStream::connect(path).is_ok() {
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
        let mut socks = vec![(listener.sock(), false), (inner.waker.0.sock(), false)];
        let ids: Vec<u64> = {
            let e = inner.engine.lock().unwrap();
            let mut ids = vec![];
            for v in &e.viewers {
                socks.push((v.sock.sock(), !v.out.is_empty() && !v.hold_writes));
                ids.push(v.serial());
            }
            ids
        };
        let Ok(ready) = plat::poll(&socks) else {
            return;
        };
        if inner.stop.load(Ordering::SeqCst) {
            return;
        }
        let mut e = inner.engine.lock().unwrap();
        if ready[1].0 {
            e.waker.drain();
        }
        if ready[0].0 {
            while let Ok((s, _)) = listener.accept() {
                e.accept(s);
            }
        }
        for (k, serial) in ids.iter().enumerate() {
            if ready[2 + k].0 {
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
