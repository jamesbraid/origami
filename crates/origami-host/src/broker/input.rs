use std::collections::HashMap;
use std::io::{self, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, TryLockError};
use std::time::{Duration, Instant};

use super::ring::{self, Reader, Ring, RingWriter};

/// Clears a half-typed line at the guest's prompt.
pub const CTRL_U: u8 = 0x15;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ConnId(pub u64);

#[derive(Clone, Debug)]
pub struct Config {
    pub ring_bytes: usize,
    /// How long one write to the engine's console may take. It also bounds
    /// how long another sender waits for the input mutex.
    pub write_timeout: Duration,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            ring_bytes: 4 << 20,
            write_timeout: Duration::from_secs(5),
        }
    }
}

/// Where console input goes: the engine's console chardev, or a test buffer.
pub trait InputSink: Send + 'static {
    /// Write all of `data`, failing with `TimedOut` if it takes longer than
    /// `timeout`.
    fn write_input(&mut self, data: &[u8], timeout: Duration) -> io::Result<()>;
}

impl InputSink for TcpStream {
    fn write_input(&mut self, data: &[u8], timeout: Duration) -> io::Result<()> {
        self.set_write_timeout(Some(timeout))?;
        self.write_all(data).and_then(|()| self.flush())
    }
}

#[derive(Debug)]
pub enum InputError {
    /// The write, or the wait for another sender's write, ran out of time.
    Stalled,
    Io(io::Error),
    /// Another connection holds the lease.
    LeaseHeld(ConnId),
    /// `release` from a connection that does not hold it.
    NotHolder,
}

impl std::fmt::Display for InputError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InputError::Stalled => write!(f, "console input timed out"),
            InputError::Io(e) => write!(f, "console input failed: {e}"),
            InputError::LeaseHeld(_) => write!(f, "another connection holds the console lease"),
            InputError::NotHolder => write!(f, "this connection does not hold the lease"),
        }
    }
}

impl std::error::Error for InputError {}

fn io_error(e: io::Error) -> InputError {
    match e.kind() {
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => InputError::Stalled,
        _ => InputError::Io(e),
    }
}

/// Told to a connection out of band, never inside the byte stream.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Notice {
    /// Input from this connection was dropped because `holder` has the lease.
    /// Sent once per lease, not once per keystroke.
    InputDropped { holder: ConnId },
    /// This connection's lease was ended by someone else.
    LeaseBroken { ctrl_u: bool },
}

pub trait Notifier: Send + Sync {
    fn notify(&self, notice: Notice);
}

impl<F: Fn(Notice) + Send + Sync> Notifier for F {
    fn notify(&self, notice: Notice) {
        self(notice)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SendOutcome {
    Sent,
    /// A different connection holds the lease; nothing was written.
    Dropped,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LeaseGrant {
    /// Ring offset at the grant: output before it is not the holder's.
    pub offset: u64,
    /// Total input bytes written to the sink before the grant. Nothing from
    /// another connection reaches the sink after this count.
    pub input_seq: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LeaseInfo {
    /// Ctrl-U was sent because the last line did not end in a carriage return.
    pub ctrl_u: bool,
}

struct InputState<S> {
    sink: S,
    holder: Option<ConnId>,
    partial: bool,
    written: u64,
}

struct Entry {
    notifier: Option<Arc<dyn Notifier>>,
    warned_epoch: AtomicU64,
}

pub struct Broker<S: InputSink> {
    ring: Arc<Ring>,
    live_from: u64,
    boot_offset: AtomicU64,
    lease_offset: AtomicU64,
    // Bumped on every grant, so a dropped-input notice fires once per lease.
    epoch: AtomicU64,
    next_id: AtomicU64,
    input: Mutex<InputState<S>>,
    conns: Mutex<HashMap<ConnId, Arc<Entry>>>,
    write_timeout: Duration,
}

impl<S: InputSink> Broker<S> {
    /// `tail` primes the ring with earlier output (the end of the serial
    /// log); everything after it is live. Returns the broker and the single
    /// writer end for the thread that reads the engine's console.
    pub fn new(config: Config, tail: &[u8], sink: S) -> (Arc<Broker<S>>, RingWriter) {
        let (mut writer, ring) = ring::new(config.ring_bytes);
        writer.append(tail);
        let live_from = ring.head();
        let broker = Arc::new(Broker {
            ring,
            live_from,
            boot_offset: AtomicU64::new(0),
            lease_offset: AtomicU64::new(0),
            epoch: AtomicU64::new(0),
            next_id: AtomicU64::new(1),
            input: Mutex::new(InputState {
                sink,
                holder: None,
                partial: false,
                written: 0,
            }),
            conns: Mutex::new(HashMap::new()),
            write_timeout: config.write_timeout,
        });
        (broker, writer)
    }

    pub fn ring(&self) -> &Arc<Ring> {
        &self.ring
    }

    pub fn live_from(&self) -> u64 {
        self.live_from
    }

    /// Record that the machine (re)started now, and return the ring offset
    /// where its output begins. Call it as the engine confirms the start.
    pub fn mark_boot(&self) -> u64 {
        let off = self.ring.head();
        self.boot_offset.store(off, Ordering::Release);
        off
    }

    pub fn boot_offset(&self) -> u64 {
        self.boot_offset.load(Ordering::Acquire)
    }

    pub fn holder(&self) -> Option<ConnId> {
        self.lock_input().ok().and_then(|g| g.holder)
    }

    /// A reader for an install: output from before the lease or the current
    /// boot, and primed history, cannot satisfy a step it waits on.
    pub fn install_reader(&self) -> Reader {
        let from = self
            .live_from
            .max(self.lease_offset.load(Ordering::Acquire))
            .max(self.boot_offset.load(Ordering::Acquire));
        self.ring.reader_at(from)
    }

    /// A viewer's reader starting at `offset` (0 reaches back to the primed
    /// history, if the ring still has it).
    pub fn reader_at(&self, offset: u64) -> Reader {
        self.ring.reader_at(offset)
    }

    pub fn reader_live(&self) -> Reader {
        self.ring.reader_at(self.ring.head())
    }

    /// Register a console connection. Dropping the returned handle is how
    /// its lease ends when the socket closes, whatever the reason.
    pub fn connect(self: &Arc<Self>, notifier: Option<Arc<dyn Notifier>>) -> Connection<S> {
        let id = ConnId(self.next_id.fetch_add(1, Ordering::Relaxed));
        let entry = Arc::new(Entry {
            notifier,
            warned_epoch: AtomicU64::new(0),
        });
        self.conns
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id, entry);
        Connection {
            broker: self.clone(),
            id,
        }
    }

    /// End someone else's lease, for a user whose install hung. Returns the
    /// old holder, who is told.
    pub fn break_lease(&self) -> Result<Option<ConnId>, InputError> {
        let mut g = self.lock_input()?;
        let Some(holder) = g.holder else {
            return Ok(None);
        };
        let ctrl_u = self.end_lease(&mut g);
        drop(g);
        self.notify(holder, Notice::LeaseBroken { ctrl_u });
        Ok(Some(holder))
    }

    // A std Mutex has no timed lock. Contention is rare (typing, a lease
    // grant), so spin briefly, then back off in short sleeps until the same
    // deadline a stuck write would hit.
    fn lock_input(&self) -> Result<MutexGuard<'_, InputState<S>>, InputError> {
        let deadline = Instant::now() + self.write_timeout;
        let mut spins = 0u32;
        loop {
            match self.input.try_lock() {
                Ok(g) => return Ok(g),
                Err(TryLockError::Poisoned(e)) => return Ok(e.into_inner()),
                Err(TryLockError::WouldBlock) => {}
            }
            if Instant::now() >= deadline {
                return Err(InputError::Stalled);
            }
            spins += 1;
            if spins < 200 {
                std::thread::yield_now();
            } else {
                std::thread::sleep(Duration::from_micros(50));
            }
        }
    }

    fn write(&self, g: &mut InputState<S>, data: &[u8]) -> Result<(), InputError> {
        match g.sink.write_input(data, self.write_timeout) {
            Ok(()) => {
                g.written += data.len() as u64;
                Ok(())
            }
            Err(e) => {
                // Part of it may have gone out; treat a held line as unfinished.
                g.partial = true;
                Err(io_error(e))
            }
        }
    }

    // Caller holds the mutex. Returns whether Ctrl-U was sent.
    fn end_lease(&self, g: &mut InputState<S>) -> bool {
        let ctrl_u = g.partial && self.write(g, &[CTRL_U]).is_ok();
        g.holder = None;
        g.partial = false;
        self.lease_offset.store(0, Ordering::Release);
        ctrl_u
    }

    fn notify(&self, id: ConnId, n: Notice) {
        let entry = self
            .conns
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&id)
            .cloned();
        if let Some(notifier) = entry.and_then(|e| e.notifier.clone()) {
            notifier.notify(n);
        }
    }
}

/// One console client's handle on the broker.
pub struct Connection<S: InputSink> {
    broker: Arc<Broker<S>>,
    id: ConnId,
}

impl<S: InputSink> Connection<S> {
    pub fn id(&self) -> ConnId {
        self.id
    }

    pub fn broker(&self) -> &Arc<Broker<S>> {
        &self.broker
    }

    /// Send input. While another connection holds the lease the bytes are
    /// dropped and this connection is told once. A write failure or timeout
    /// fails this call; the lease is untouched.
    pub fn send(&self, data: &[u8]) -> Result<SendOutcome, InputError> {
        if data.is_empty() {
            return Ok(SendOutcome::Sent);
        }
        let b = &*self.broker;
        let mut g = b.lock_input()?;
        match g.holder {
            Some(h) if h != self.id => {
                drop(g);
                self.warn(h);
                Ok(SendOutcome::Dropped)
            }
            holder => {
                b.write(&mut g, data)?;
                if holder.is_some() {
                    g.partial = data.last() != Some(&b'\r');
                }
                Ok(SendOutcome::Sent)
            }
        }
    }

    fn warn(&self, holder: ConnId) {
        let b = &*self.broker;
        let entry = b
            .conns
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&self.id)
            .cloned();
        let Some(entry) = entry else { return };
        let epoch = b.epoch.load(Ordering::Acquire);
        if entry.warned_epoch.swap(epoch, Ordering::AcqRel) != epoch {
            if let Some(n) = &entry.notifier {
                n.notify(Notice::InputDropped { holder });
            }
        }
    }

    /// Take the input lease. It is granted between whole writes: the mutex
    /// is held by any write in flight, and everything after the grant from
    /// other connections is dropped. Asking again while holding it is fine.
    pub fn lease(&self) -> Result<LeaseGrant, InputError> {
        let b = &*self.broker;
        let mut g = b.lock_input()?;
        match g.holder {
            Some(h) if h != self.id => return Err(InputError::LeaseHeld(h)),
            Some(_) => {}
            None => {
                g.holder = Some(self.id);
                g.partial = false;
                b.lease_offset.store(b.ring.head(), Ordering::Release);
                b.epoch.fetch_add(1, Ordering::AcqRel);
            }
        }
        Ok(LeaseGrant {
            offset: b.lease_offset.load(Ordering::Acquire),
            input_seq: g.written,
        })
    }

    /// Give the lease up. If the last line did not end in a carriage return,
    /// Ctrl-U is sent so the guest does not keep the fragment.
    pub fn release(&self) -> Result<LeaseInfo, InputError> {
        let b = &*self.broker;
        let mut g = b.lock_input()?;
        if g.holder != Some(self.id) {
            return Err(InputError::NotHolder);
        }
        Ok(LeaseInfo {
            ctrl_u: b.end_lease(&mut g),
        })
    }

    pub fn holds_lease(&self) -> bool {
        self.broker.holder() == Some(self.id)
    }
}

impl<S: InputSink> Drop for Connection<S> {
    fn drop(&mut self) {
        let b = &*self.broker;
        b.conns
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.id);
        // Blocking, not timed: the lease must not outlive its connection, and
        // a stuck write ahead of us is already bounded by the write timeout.
        let mut g = b.input.lock().unwrap_or_else(|e| e.into_inner());
        if g.holder == Some(self.id) {
            b.end_lease(&mut g);
        }
    }
}
