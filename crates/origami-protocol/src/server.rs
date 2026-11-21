use std::collections::HashMap;
use std::io::{self, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::Duration;

use crate::frame::{write_line, FrameError, LineReader, RawStream};
use crate::message::{ErrorKind, Event, PingData, Reply, Request, Verb, PROTOCOL};

const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
const DRAIN: Duration = Duration::from_secs(2);

/// What a handler decided for one request.
pub enum Outcome {
    Reply(Reply),
    /// Send the reply, then hand the connection to [`Handler::raw`]. Only
    /// meaningful for `console`; an error reply should use `Reply`.
    Raw(Reply),
}

pub trait Handler: Send + Sync + 'static {
    /// The engine name `ping` reports.
    fn engine(&self) -> String;

    /// The verbs this server offers. Others are refused before `handle`.
    fn verbs(&self) -> Vec<Verb> {
        Verb::ALL.to_vec()
    }

    /// `ping` is answered by the server and never reaches here.
    fn handle(&self, conn: &Conn, req: Request) -> Outcome;

    /// Runs on the connection's thread after an [`Outcome::Raw`] reply, with
    /// the stream in raw mode; the connection ends when it returns.
    fn raw(&self, conn: &Conn, stream: RawStream) {
        let _ = (conn, stream);
    }

    /// The connection is gone, whatever the reason. Called exactly once,
    /// after `raw` if there was one.
    fn closed(&self, conn: &Conn) {
        let _ = conn;
    }
}

struct Out {
    stream: Mutex<TcpStream>,
    raw: AtomicBool,
}

/// Pushes event lines to one connection from any thread.
#[derive(Clone)]
pub struct EventSender(Arc<Out>);

impl EventSender {
    /// `Ok(false)` if the connection is already in raw mode, where a line
    /// would corrupt the byte stream.
    pub fn send(&self, event: &Event) -> io::Result<bool> {
        let mut s = self.0.stream.lock().unwrap();
        if self.0.raw.load(Ordering::Acquire) {
            return Ok(false);
        }
        write_line(&mut *s, &event.to_line())?;
        Ok(true)
    }
}

pub struct Conn {
    pub id: u64,
    pub peer: SocketAddr,
    out: Arc<Out>,
}

impl Conn {
    pub fn events(&self) -> EventSender {
        EventSender(self.out.clone())
    }
}

struct Shared {
    handler: Arc<dyn Handler>,
    next_id: AtomicU64,
    stop: AtomicBool,
    live: Mutex<HashMap<u64, TcpStream>>,
    /// Requests being handled or replied to; shutdown lets them finish.
    busy: (Mutex<usize>, Condvar),
}

pub struct Server {
    addr: SocketAddr,
    shared: Arc<Shared>,
    accept: Option<thread::JoinHandle<()>>,
}

impl Server {
    pub fn bind(
        addr: impl std::net::ToSocketAddrs,
        handler: Arc<dyn Handler>,
    ) -> io::Result<Server> {
        let listener = TcpListener::bind(addr)?;
        let addr = listener.local_addr()?;
        let shared = Arc::new(Shared {
            handler,
            next_id: AtomicU64::new(1),
            stop: AtomicBool::new(false),
            live: Mutex::new(HashMap::new()),
            busy: (Mutex::new(0), Condvar::new()),
        });
        let sh = shared.clone();
        let accept = thread::Builder::new()
            .name("control-accept".into())
            .spawn(move || accept_loop(listener, sh))?;
        Ok(Server {
            addr,
            shared,
            accept: Some(accept),
        })
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    /// Stop accepting and close every connection, raw ones included.
    pub fn shutdown(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        // Wake the blocked accept.
        let _ = TcpStream::connect_timeout(&self.addr, Duration::from_secs(1));
        // A request that made the machine exit is still writing its reply.
        let (count, idle) = &self.shared.busy;
        let _ = idle.wait_timeout_while(count.lock().unwrap(), DRAIN, |n| *n > 0);
        for s in self.shared.live.lock().unwrap().values() {
            let _ = s.shutdown(Shutdown::Both);
        }
        if let Some(h) = self.accept.take() {
            let _ = h.join();
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn accept_loop(listener: TcpListener, sh: Arc<Shared>) {
    for stream in listener.incoming() {
        if sh.stop.load(Ordering::Acquire) {
            break;
        }
        let Ok(stream) = stream else { continue };
        let sh = sh.clone();
        let _ = thread::Builder::new()
            .name("control-conn".into())
            .spawn(move || serve(stream, sh));
    }
}

fn serve(stream: TcpStream, sh: Arc<Shared>) {
    let _ = stream.set_nodelay(true);
    let _ = stream.set_write_timeout(Some(WRITE_TIMEOUT));
    let (Ok(peer), Ok(out_stream), Ok(live)) =
        (stream.peer_addr(), stream.try_clone(), stream.try_clone())
    else {
        return;
    };
    let id = sh.next_id.fetch_add(1, Ordering::Relaxed);
    sh.live.lock().unwrap().insert(id, live);
    let conn = Conn {
        id,
        peer,
        out: Arc::new(Out {
            stream: Mutex::new(out_stream),
            raw: AtomicBool::new(false),
        }),
    };
    run(&conn, stream, &sh);
    sh.handler.closed(&conn);
    sh.live.lock().unwrap().remove(&id);
}

struct Busy<'a>(&'a (Mutex<usize>, Condvar));

impl<'a> Busy<'a> {
    fn enter(b: &'a (Mutex<usize>, Condvar)) -> Self {
        *b.0.lock().unwrap() += 1;
        Busy(b)
    }
}

impl Drop for Busy<'_> {
    fn drop(&mut self) {
        *self.0 .0.lock().unwrap() -= 1;
        self.0 .1.notify_all();
    }
}

fn reply(conn: &Conn, r: &Reply) -> io::Result<()> {
    let mut s = conn.out.stream.lock().unwrap();
    write_line(&mut *s, &r.to_line())
}

fn run(conn: &Conn, stream: TcpStream, sh: &Shared) {
    let handler = &sh.handler;
    let offered = handler.verbs();
    let mut reader = LineReader::new(stream);
    loop {
        let line = match reader.read_line() {
            Ok(Some(l)) => l,
            Ok(None) | Err(FrameError::Io(_)) => return,
            Err(FrameError::TooLong) => {
                // The rest of the line is still in flight; no way to resync.
                let _ = reply(
                    conn,
                    &Reply::err(ErrorKind::TooLarge, "request line too long"),
                );
                return;
            }
        };
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let req = match Request::parse(&line) {
            Ok(r) => r,
            Err(e) => {
                if reply(conn, &e).is_err() {
                    return;
                }
                continue;
            }
        };
        let verb = req.verb();
        let busy = Busy::enter(&sh.busy);
        let outcome = if verb == Verb::Ping {
            Outcome::Reply(Reply::data(PingData {
                protocol: PROTOCOL,
                engine: handler.engine(),
                verbs: offered.iter().map(|v| v.name().to_owned()).collect(),
            }))
        } else if !offered.contains(&verb) {
            Outcome::Reply(Reply::err(
                ErrorKind::Unsupported,
                format!("{} is not offered by this server", verb.name()),
            ))
        } else {
            handler.handle(conn, req)
        };
        match outcome {
            Outcome::Reply(r) => {
                if reply(conn, &r).is_err() {
                    return;
                }
            }
            Outcome::Raw(r) => {
                {
                    // Flip under the writer lock so no event line lands
                    // after the reply and before the raw bytes.
                    let mut s = conn.out.stream.lock().unwrap();
                    if write_line(&mut *s, &r.to_line()).is_err() {
                        return;
                    }
                    conn.out.raw.store(true, Ordering::Release);
                }
                drop(busy);
                let (stream, pending) = reader.into_parts();
                handler.raw(conn, RawStream::new(stream, pending));
                let _ = conn.out.stream.lock().unwrap().flush();
                return;
            }
        }
    }
}
