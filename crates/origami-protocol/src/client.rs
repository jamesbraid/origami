use std::collections::VecDeque;
use std::io;
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::time::Duration;

use serde_json::Value;

use crate::frame::{write_line, FrameError, LineReader, RawStream};
use crate::message::{ErrorKind, Event, PingData, Reply, Request, Verb};

#[derive(Debug)]
pub enum ClientError {
    Io(io::Error),
    /// No reply in time. The connection is unusable afterwards, because the
    /// late reply would be taken for the next call's.
    Timeout,
    Closed,
    Frame(FrameError),
    Protocol(String),
    /// The server's `ping` did not list the verb.
    NotOffered(Verb),
    /// The server answered `ok: false`.
    Remote {
        kind: ErrorKind,
        message: String,
    },
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClientError::Io(e) => write!(f, "{e}"),
            ClientError::Timeout => write!(f, "no reply from the runner in time"),
            ClientError::Closed => write!(f, "the runner closed the connection"),
            ClientError::Frame(e) => write!(f, "{e}"),
            ClientError::Protocol(s) => write!(f, "protocol error: {s}"),
            ClientError::NotOffered(v) => write!(f, "the runner does not offer {}", v.name()),
            ClientError::Remote { message, .. } => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for ClientError {}

fn io_err(e: io::Error) -> ClientError {
    match e.kind() {
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => ClientError::Timeout,
        _ => ClientError::Io(e),
    }
}

pub struct Client {
    reader: LineReader<TcpStream>,
    writer: TcpStream,
    events: VecDeque<Event>,
    offered: Option<Vec<Verb>>,
    default_timeout: Duration,
    poisoned: bool,
}

impl Client {
    pub fn connect(port: u16, timeout: Duration) -> Result<Client, ClientError> {
        Self::connect_addr(SocketAddr::from((Ipv4Addr::LOCALHOST, port)), timeout)
    }

    pub fn connect_addr(addr: SocketAddr, timeout: Duration) -> Result<Client, ClientError> {
        let s = TcpStream::connect_timeout(&addr, timeout).map_err(io_err)?;
        let _ = s.set_nodelay(true);
        s.set_write_timeout(Some(timeout))
            .map_err(ClientError::Io)?;
        let writer = s.try_clone().map_err(ClientError::Io)?;
        Ok(Client {
            reader: LineReader::new(s),
            writer,
            events: VecDeque::new(),
            offered: None,
            default_timeout: timeout,
            poisoned: false,
        })
    }

    /// Ask what the server offers; later calls to other verbs are refused
    /// locally if it did not list them.
    pub fn ping(&mut self) -> Result<PingData, ClientError> {
        let v = self.call_data(&Request::Ping {})?;
        let data: PingData = serde_json::from_value(v)
            .map_err(|e| ClientError::Protocol(format!("bad ping: {e}")))?;
        self.offered = Some(
            data.verbs
                .iter()
                .filter_map(|n| Verb::from_name(n))
                .collect(),
        );
        Ok(data)
    }

    pub fn offers(&self, verb: Verb) -> bool {
        self.offered.as_ref().is_none_or(|o| o.contains(&verb))
    }

    /// Wait up to `timeout` for the next pushed event. `Ok(None)` is a
    /// timeout; the connection stays usable. Meant for a connection kept
    /// only to watch events, not one that also makes calls.
    pub fn next_event(&mut self, timeout: Duration) -> Result<Option<Event>, ClientError> {
        if let Some(ev) = self.events.pop_front() {
            return Ok(Some(ev));
        }
        if self.poisoned {
            return Err(ClientError::Closed);
        }
        // On the stream that is read: Windows keeps a cloned socket's timeouts apart.
        self.reader
            .get_ref()
            .set_read_timeout(Some(timeout.max(Duration::from_millis(1))))
            .map_err(ClientError::Io)?;
        loop {
            let line = match self.reader.read_line() {
                Ok(Some(l)) => l,
                Ok(None) => {
                    self.poisoned = true;
                    return Err(ClientError::Closed);
                }
                Err(FrameError::Io(e)) => match io_err(e) {
                    ClientError::Timeout => return Ok(None),
                    other => return Err(other),
                },
                Err(e) => return Err(ClientError::Frame(e)),
            };
            if let Ok(ev) = serde_json::from_slice::<Event>(&line) {
                if ev != Event::Unknown {
                    return Ok(Some(ev));
                }
            }
        }
    }

    /// Events pushed since the last call, oldest first.
    pub fn take_events(&mut self) -> Vec<Event> {
        self.events.drain(..).collect()
    }

    pub fn call(&mut self, req: &Request) -> Result<Reply, ClientError> {
        self.call_timeout(req, self.default_timeout)
    }

    /// For verbs that legitimately take long, like `wait-serial` and `stop`.
    pub fn call_timeout(&mut self, req: &Request, timeout: Duration) -> Result<Reply, ClientError> {
        if self.poisoned {
            return Err(ClientError::Closed);
        }
        if !self.offers(req.verb()) {
            return Err(ClientError::NotOffered(req.verb()));
        }
        let r = self.exchange(req, timeout);
        if matches!(
            r,
            Err(ClientError::Timeout | ClientError::Io(_) | ClientError::Closed)
        ) {
            self.poisoned = true;
        }
        r
    }

    fn exchange(&mut self, req: &Request, timeout: Duration) -> Result<Reply, ClientError> {
        write_line(&mut self.writer, &req.to_line()).map_err(io_err)?;
        // On the stream that is read: Windows keeps a cloned socket's timeouts apart.
        self.reader
            .get_ref()
            .set_read_timeout(Some(timeout.max(Duration::from_millis(1))))
            .map_err(ClientError::Io)?;
        loop {
            let line = match self.reader.read_line() {
                Ok(Some(l)) => l,
                Ok(None) => return Err(ClientError::Closed),
                Err(FrameError::Io(e)) => return Err(io_err(e)),
                Err(e) => return Err(ClientError::Frame(e)),
            };
            let v: Value = serde_json::from_slice(&line)
                .map_err(|e| ClientError::Protocol(format!("reply is not JSON: {e}")))?;
            if v.get("event").is_some() {
                // An event this client does not know is not an error.
                if let Ok(ev) = serde_json::from_value::<Event>(v) {
                    if ev != Event::Unknown {
                        self.events.push_back(ev);
                    }
                }
                continue;
            }
            return serde_json::from_value(v)
                .map_err(|e| ClientError::Protocol(format!("bad reply: {e}")));
        }
    }

    /// The reply's data, or the server's error as [`ClientError::Remote`].
    pub fn call_data(&mut self, req: &Request) -> Result<Value, ClientError> {
        self.call_data_timeout(req, self.default_timeout)
    }

    pub fn call_data_timeout(
        &mut self,
        req: &Request,
        timeout: Duration,
    ) -> Result<Value, ClientError> {
        let r = self.call_timeout(req, timeout)?;
        into_data(r)
    }

    /// Send a `console` request. On success the connection is raw bytes from
    /// here on and the reply's data comes back with the stream.
    pub fn console(
        mut self,
        line: &str,
        lease: bool,
        from: Option<u64>,
    ) -> Result<(Value, RawStream), ClientError> {
        let req = Request::Console {
            line: line.to_owned(),
            lease,
            from,
        };
        let data = self.call_data(&req)?;
        let (stream, pending) = self.reader.into_parts();
        // The timeout set for the reply must not apply to the byte stream.
        stream.set_read_timeout(None).map_err(ClientError::Io)?;
        Ok((data, RawStream::new(stream, pending)))
    }
}

fn into_data(r: Reply) -> Result<Value, ClientError> {
    if r.ok {
        Ok(r.data.unwrap_or(Value::Null))
    } else {
        Err(ClientError::Remote {
            kind: r.kind.unwrap_or(ErrorKind::Other),
            message: r.error.unwrap_or_else(|| "request failed".into()),
        })
    }
}
