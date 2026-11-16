//! The install driver's side of a runner's console: it takes the input
//! lease, so other viewers turn read-only, and reads output that begins at
//! the boot it waits for. One thread per direction, each over a bounded
//! buffer.

use origami_machine::console::{Console, Transport};
use origami_machine::control;
use origami_protocol::{BootOffsetData, Client, ClientError, ConsoleData, ErrorKind, Request};
use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// True once the work using the console should stop.
pub type Cancel = Arc<dyn Fn() -> bool + Send + Sync>;

type Failure = Box<dyn std::error::Error + Send + Sync>;

/// Starts the machine's runner paused, for [`InstallStart::Held`].
pub type Starter = Box<dyn FnOnce(&Path) -> Result<(), Failure> + Send>;

/// How an install reaches the guest.
pub enum InstallStart {
    /// The machine runs and sits at the installer's prompt: take the lease
    /// and read what the guest prints from now on.
    Attached,
    /// The machine runs: take the lease, then reset it, and read from the
    /// boot that follows.
    Reset,
    /// Start the machine held with the function, take the lease, then
    /// continue it and read from its boot.
    Held(Starter),
}

const CONNECT: Duration = Duration::from_secs(2);
const REPLY: Duration = Duration::from_secs(30);
const RUNNER_WAIT: Duration = Duration::from_secs(60);
const POLL: Duration = Duration::from_millis(100);
const CHUNK: usize = 4096;
/// Chunks buffered each way before the producing thread waits.
const BUFFER: usize = 64;
/// How long a full input buffer may stay full before the console is called stuck.
const STUCK: Duration = Duration::from_secs(30);

fn cancelled() -> io::Error {
    io::Error::other("the install was cancelled")
}

enum Piece {
    Data { offset: u64, bytes: Vec<u8> },
    End,
    Failed(String),
}

struct LeaseLink {
    pieces: Receiver<Piece>,
    keys: Option<SyncSender<Vec<u8>>>,
    failure: Arc<Mutex<Option<String>>>,
    closer: TcpStream,
    /// Output before this ring offset belongs to an earlier boot.
    gate: u64,
    ready: Vec<u8>,
    ended: bool,
    cancel: Cancel,
    /// Set when the console is dropped, so the writer thread stops.
    closing: Arc<AtomicBool>,
}

impl Transport for LeaseLink {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        loop {
            if (self.cancel)() {
                return Err(cancelled());
            }
            if !self.ready.is_empty() {
                let count = buffer.len().min(self.ready.len());
                buffer[..count].copy_from_slice(&self.ready[..count]);
                self.ready.drain(..count);
                return Ok(count);
            }
            if self.ended {
                return Ok(0);
            }
            match self.pieces.recv_timeout(POLL) {
                Ok(Piece::Data { offset, bytes }) => {
                    let skip = self.gate.saturating_sub(offset) as usize;
                    if skip < bytes.len() {
                        self.ready = bytes[skip..].to_vec();
                    }
                }
                Ok(Piece::End) | Err(RecvTimeoutError::Disconnected) => self.ended = true,
                Ok(Piece::Failed(text)) => return Err(io::Error::other(text)),
                Err(RecvTimeoutError::Timeout) => {
                    return Err(io::ErrorKind::TimedOut.into());
                }
            }
        }
    }

    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        let keys = self.keys.as_ref().expect("the sender lives until drop");
        for chunk in bytes.chunks(CHUNK) {
            let mut pending = chunk.to_vec();
            let waiting = Instant::now();
            loop {
                if (self.cancel)() {
                    return Err(cancelled());
                }
                if let Some(text) = self.failure.lock().unwrap().clone() {
                    return Err(io::Error::other(text));
                }
                match keys.try_send(pending) {
                    Ok(()) => break,
                    Err(TrySendError::Full(back)) => {
                        if waiting.elapsed() > STUCK {
                            return Err(io::Error::other("the console is not taking input"));
                        }
                        pending = back;
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(TrySendError::Disconnected(_)) => {
                        return Err(io::Error::other("the console closed"));
                    }
                }
            }
        }
        Ok(())
    }
}

impl Drop for LeaseLink {
    fn drop(&mut self) {
        // Closing the connection ends the lease; the broker clears a
        // half-typed line.
        self.closing.store(true, Ordering::Release);
        self.keys = None;
        let _ = self.closer.shutdown(Shutdown::Both);
    }
}

fn remote(error: ClientError) -> Failure {
    match error {
        ClientError::Remote {
            kind: ErrorKind::LeaseHeld,
            message,
        } => format!("another client holds the console: {message}; break its lease first").into(),
        other => other.to_string().into(),
    }
}

fn runner(dir: &Path) -> Result<Client, Failure> {
    let record = control::read(dir).map_err(|error| -> Failure {
        if error
            .downcast_ref::<io::Error>()
            .is_some_and(|error| error.kind() == io::ErrorKind::NotFound)
        {
            "install commands require a background run; start with origami run DIR --background"
                .into()
        } else {
            error
        }
    })?;
    if record.control_port == 0 {
        return Err("this machine's runner has no control socket; restart the machine".into());
    }
    let mut client = Client::connect(record.control_port, CONNECT).map_err(remote)?;
    client.ping().map_err(remote)?;
    Ok(client)
}

/// Connect an install's console to the machine in `dir` as `start` says.
pub fn connect(dir: &Path, start: InstallStart, cancel: Cancel) -> Result<Console, Failure> {
    let boot = match start {
        InstallStart::Attached => None,
        InstallStart::Reset => Some(Request::Reset {}),
        InstallStart::Held(starter) => {
            starter(dir)?;
            wait_for_runner(dir, &cancel)?;
            Some(Request::Cont {})
        }
    };
    let (data, stream) = runner(dir)?.console("", true, None).map_err(remote)?;
    let data: ConsoleData = serde_json::from_value(data)?;
    // The lease is ours before the guest runs, so nobody else's keystroke
    // reaches an installer that is about to start typing.
    let gate = match boot {
        None => data.start.max(data.live_from),
        Some(request) => {
            let reply = runner(dir)?
                .call_data_timeout(&request, REPLY)
                .map_err(remote)?;
            serde_json::from_value::<BootOffsetData>(reply)?.boot_offset
        }
    };
    let closer = stream.try_clone_writer()?;
    let sender = stream.try_clone_writer()?;
    let (piece_tx, pieces) = sync_channel(BUFFER);
    std::thread::Builder::new()
        .name("origami-install-read".into())
        .spawn(move || read_loop(stream, data.start, &piece_tx))?;
    let (keys, key_rx) = sync_channel::<Vec<u8>>(BUFFER);
    let failure = Arc::new(Mutex::new(None));
    let closing = Arc::new(AtomicBool::new(false));
    {
        let (failure, closing) = (failure.clone(), closing.clone());
        std::thread::Builder::new()
            .name("origami-install-write".into())
            .spawn(move || write_loop(sender, &key_rx, &failure, &closing))?;
    }
    Ok(Console::from_transport(Box::new(LeaseLink {
        pieces,
        keys: Some(keys),
        failure,
        closer,
        gate,
        ready: Vec::new(),
        ended: false,
        cancel,
        closing,
    })))
}

fn wait_for_runner(dir: &Path, cancel: &Cancel) -> Result<(), Failure> {
    let deadline = Instant::now() + RUNNER_WAIT;
    while Instant::now() < deadline {
        if cancel() {
            return Err(cancelled().into());
        }
        if control::read(dir).is_ok_and(|record| record.control_port != 0) {
            return Ok(());
        }
        std::thread::sleep(POLL);
    }
    Err("the held machine's runner did not come up".into())
}

fn read_loop(mut stream: impl Read, start: u64, out: &SyncSender<Piece>) {
    let mut offset = start;
    let mut buffer = [0u8; CHUNK];
    loop {
        let piece = match stream.read(&mut buffer) {
            Ok(0) => Piece::End,
            Ok(count) => {
                let bytes = buffer[..count].to_vec();
                let piece = Piece::Data { offset, bytes };
                offset += count as u64;
                piece
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => Piece::Failed(error.to_string()),
        };
        let last = !matches!(piece, Piece::Data { .. });
        if out.send(piece).is_err() || last {
            return;
        }
    }
}

fn write_loop(
    mut stream: TcpStream,
    keys: &Receiver<Vec<u8>>,
    failure: &Mutex<Option<String>>,
    closing: &AtomicBool,
) {
    let _ = stream.set_write_timeout(Some(Duration::from_secs(10)));
    while let Ok(bytes) = keys.recv() {
        if closing.load(Ordering::Acquire) {
            return;
        }
        if let Err(error) = Write::write_all(&mut stream, &bytes).and_then(|()| stream.flush()) {
            *failure.lock().unwrap() = Some(error.to_string());
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    fn read_all(gate: u64, pieces: Vec<Piece>) -> Vec<u8> {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let closer = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (tx, rx) = sync_channel(pieces.len().max(1));
        for piece in pieces {
            tx.send(piece).unwrap();
        }
        drop(tx);
        let mut link = LeaseLink {
            pieces: rx,
            keys: None,
            failure: Arc::default(),
            closer,
            gate,
            ready: Vec::new(),
            ended: false,
            cancel: Arc::new(|| false),
            closing: Arc::default(),
        };
        let mut out = Vec::new();
        let mut buffer = [0u8; 64];
        loop {
            match link.read(&mut buffer).unwrap() {
                0 => return out,
                count => out.extend_from_slice(&buffer[..count]),
            }
        }
    }

    fn data(offset: u64, text: &str) -> Piece {
        Piece::Data {
            offset,
            bytes: text.into(),
        }
    }

    #[test]
    fn output_before_the_boot_offset_never_reaches_the_installer() {
        let stale = vec![data(0, "Inst> "), data(6, "boot")];
        assert_eq!(read_all(6, stale), b"boot");
        assert_eq!(read_all(3, vec![data(0, "oldnew")]), b"new");
        assert_eq!(read_all(0, vec![data(0, "all")]), b"all");
    }
}
