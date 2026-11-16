use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use super::input::{Broker, Connection, InputSink};
use super::ring::{Reader, RingWriter};

/// How often an idle output pump looks for a stop request.
const POLL: Duration = Duration::from_millis(100);

/// Feed the ring from the engine's console until it ends, then close the
/// ring so readers see end of output.
pub fn run_producer<R: Read>(mut writer: RingWriter, mut src: R) -> io::Result<()> {
    let mut buf = vec![0u8; 16 * 1024];
    let r = loop {
        match src.read(&mut buf) {
            Ok(0) => break Ok(()),
            Ok(n) => writer.append(&buf[..n]),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => break Err(e),
        }
    };
    writer.close();
    r
}

/// Serve one raw console client: ring output to `to_client` from `reader`,
/// bytes from `from_client` into the broker as this connection's input. Two
/// threads, one per direction, so neither blocks the other. Returns when
/// either side ends; `shutdown` must unblock a blocked `from_client` read.
/// Input the broker drops or fails to write is lost, as on a real line.
pub fn pump<S, R, W>(
    conn: Connection<S>,
    mut reader: Reader,
    mut from_client: R,
    mut to_client: W,
    shutdown: impl Fn() + Sync,
) where
    S: InputSink,
    R: Read,
    W: Write + Send,
{
    let stop = AtomicBool::new(false);
    thread::scope(|scope| {
        scope.spawn(|| {
            let mut buf = vec![0u8; 16 * 1024];
            while !stop.load(Ordering::Acquire) {
                let o = reader.read_wait(&mut buf, Some(POLL));
                if o.n > 0
                    && to_client
                        .write_all(&buf[..o.n])
                        .and_then(|()| to_client.flush())
                        .is_err()
                {
                    break;
                }
                if o.eof {
                    break;
                }
            }
            stop.store(true, Ordering::Release);
            shutdown();
        });
        let mut buf = vec![0u8; 4096];
        loop {
            match from_client.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    let _ = conn.send(&buf[..n]);
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => break,
            }
            if stop.load(Ordering::Acquire) {
                break;
            }
        }
        stop.store(true, Ordering::Release);
        // Release the lease first: a client that vanished must not hold the
        // console while the output thread winds down.
        drop(conn);
    });
}

/// The legacy raw TCP port: whoever connects sees live output and types
/// into the console, under the same lease rules as the control socket.
pub struct RawPort {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    live: Arc<Mutex<HashMap<u64, TcpStream>>>,
    accept: Option<thread::JoinHandle<()>>,
}

impl RawPort {
    pub fn bind<S: InputSink>(
        addr: impl ToSocketAddrs,
        broker: Arc<Broker<S>>,
    ) -> io::Result<RawPort> {
        Self::bind_with(addr, broker, || {})
    }

    /// Like `bind`, calling `attach` for each client after its reader is
    /// placed at the live tail and before it is served: the runner connects
    /// to the engine's console there, so the first client misses nothing.
    pub fn bind_with<S: InputSink>(
        addr: impl ToSocketAddrs,
        broker: Arc<Broker<S>>,
        attach: impl Fn() + Send + Sync + 'static,
    ) -> io::Result<RawPort> {
        let attach = Arc::new(attach);
        let listener = TcpListener::bind(addr)?;
        let addr = listener.local_addr()?;
        let stop = Arc::new(AtomicBool::new(false));
        let live: Arc<Mutex<HashMap<u64, TcpStream>>> = Arc::default();
        let (stop2, live2) = (stop.clone(), live.clone());
        let accept = thread::Builder::new()
            .name("console-accept".into())
            .spawn(move || {
                let next = AtomicU64::new(0);
                for stream in listener.incoming() {
                    if stop2.load(Ordering::Acquire) {
                        break;
                    }
                    let Ok(stream) = stream else { continue };
                    let (Ok(out), Ok(closer), Ok(track)) =
                        (stream.try_clone(), stream.try_clone(), stream.try_clone())
                    else {
                        continue;
                    };
                    let _ = stream.set_nodelay(true);
                    let id = next.fetch_add(1, Ordering::Relaxed);
                    live2
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .insert(id, track);
                    let broker = broker.clone();
                    let live3 = live2.clone();
                    let attach = attach.clone();
                    let _ = thread::Builder::new()
                        .name("console-conn".into())
                        .spawn(move || {
                            let conn = broker.connect(None);
                            let reader = broker.reader_live();
                            attach();
                            pump(conn, reader, stream, out, || {
                                let _ = closer.shutdown(Shutdown::Both);
                            });
                            live3.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
                        });
                }
            })?;
        Ok(RawPort {
            addr,
            stop,
            live,
            accept: Some(accept),
        })
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn shutdown(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = TcpStream::connect_timeout(&self.addr, Duration::from_secs(1));
        for s in self.live.lock().unwrap_or_else(|e| e.into_inner()).values() {
            let _ = s.shutdown(Shutdown::Both);
        }
        if let Some(h) = self.accept.take() {
            let _ = h.join();
        }
    }
}

impl Drop for RawPort {
    fn drop(&mut self) {
        self.shutdown();
    }
}
