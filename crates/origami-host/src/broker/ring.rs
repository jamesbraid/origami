//! The output ring: one producer, any number of readers, no lock on the
//! append or read path.
//!
//! Offsets are absolute byte counts since the ring was made, so a reader's
//! position stays meaningful as the buffer wraps. The producer publishes in
//! two steps, like a seqlock: `reserved` moves first (the new end of the
//! write about to happen), the bytes follow, then `head` moves. A reader
//! copies up to `head`, then checks `reserved` did not pass its start by more
//! than the capacity; if it did, the copy may be torn and is discarded.
//! Readers wait on a condition variable, but the producer touches its mutex
//! only when a reader is registered as waiting, and only for the instant of
//! a notify.

use std::sync::atomic::{fence, AtomicBool, AtomicU64, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

pub struct Ring {
    buf: Box<[AtomicU8]>,
    cap: u64,
    reserved: AtomicU64,
    head: AtomicU64,
    closed: AtomicBool,
    waiters: AtomicUsize,
    gate: Mutex<()>,
    cv: Condvar,
}

/// The single producer's handle. Not `Clone`, so there is only one.
pub struct RingWriter {
    ring: Arc<Ring>,
}

pub struct Reader {
    ring: Arc<Ring>,
    offset: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReadOut {
    /// Bytes copied into the caller's buffer.
    pub n: usize,
    /// Bytes skipped because the reader fell more than a ring behind.
    pub dropped: u64,
    /// The producer finished and everything has been read.
    pub eof: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaitResult {
    /// The pattern ended at this absolute offset.
    Found(u64),
    Timeout,
    Eof,
}

pub fn new(capacity: usize) -> (RingWriter, Arc<Ring>) {
    let cap = capacity.max(1);
    let ring = Arc::new(Ring {
        buf: (0..cap).map(|_| AtomicU8::new(0)).collect(),
        cap: cap as u64,
        reserved: AtomicU64::new(0),
        head: AtomicU64::new(0),
        closed: AtomicBool::new(false),
        waiters: AtomicUsize::new(0),
        gate: Mutex::new(()),
        cv: Condvar::new(),
    });
    (RingWriter { ring: ring.clone() }, ring)
}

impl Ring {
    pub fn capacity(&self) -> usize {
        self.cap as usize
    }

    /// Offset one past the last byte appended.
    pub fn head(&self) -> u64 {
        self.head.load(Ordering::Acquire)
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    /// A reader at `offset`, clamped to the end if it is in the future.
    pub fn reader_at(self: &Arc<Self>, offset: u64) -> Reader {
        Reader {
            ring: self.clone(),
            offset: offset.min(self.head()),
        }
    }

    fn wake(&self) {
        // Pairs with the fence in `wait`: either the waiter sees the new head
        // under the gate, or we see it registered and notify after it sleeps.
        fence(Ordering::SeqCst);
        if self.waiters.load(Ordering::Relaxed) > 0 {
            drop(self.gate.lock().unwrap_or_else(|e| e.into_inner()));
            self.cv.notify_all();
        }
    }

    /// Block until the head passes `offset`, the ring closes or the timeout
    /// expires.
    fn wait(&self, offset: u64, timeout: Option<Duration>) {
        let deadline = timeout.map(|t| Instant::now() + t);
        self.waiters.fetch_add(1, Ordering::SeqCst);
        fence(Ordering::SeqCst);
        let mut g = self.gate.lock().unwrap_or_else(|e| e.into_inner());
        while self.head.load(Ordering::Acquire) <= offset && !self.closed.load(Ordering::Acquire) {
            match deadline {
                None => g = self.cv.wait(g).unwrap_or_else(|e| e.into_inner()),
                Some(d) => {
                    let now = Instant::now();
                    if now >= d {
                        break;
                    }
                    g = self
                        .cv
                        .wait_timeout(g, d - now)
                        .unwrap_or_else(|e| e.into_inner())
                        .0;
                }
            }
        }
        drop(g);
        self.waiters.fetch_sub(1, Ordering::SeqCst);
    }
}

impl RingWriter {
    pub fn ring(&self) -> &Arc<Ring> {
        &self.ring
    }

    pub fn append(&mut self, mut data: &[u8]) {
        if data.is_empty() {
            return;
        }
        let r = &*self.ring;
        let head = r.head.load(Ordering::Relaxed);
        let new_head = head + data.len() as u64;
        // Only the newest `cap` bytes can survive; skip the rest unwritten.
        let mut at = head;
        if data.len() as u64 > r.cap {
            let skip = data.len() - r.cap as usize;
            data = &data[skip..];
            at += skip as u64;
        }
        r.reserved.store(new_head, Ordering::Relaxed);
        fence(Ordering::Release);
        let start = (at % r.cap) as usize;
        let first = data.len().min(r.cap as usize - start);
        for (slot, b) in r.buf[start..start + first].iter().zip(data) {
            slot.store(*b, Ordering::Relaxed);
        }
        for (slot, b) in r.buf[..data.len() - first].iter().zip(&data[first..]) {
            slot.store(*b, Ordering::Relaxed);
        }
        r.head.store(new_head, Ordering::Release);
        r.wake();
    }

    /// No more output: readers drain what is left, then see `eof`.
    pub fn close(&mut self) {
        self.ring.closed.store(true, Ordering::Release);
        self.ring.wake();
    }
}

impl Drop for RingWriter {
    fn drop(&mut self) {
        self.close();
    }
}

impl Reader {
    pub fn offset(&self) -> u64 {
        self.offset
    }

    /// Copy what is available now, without waiting.
    pub fn read(&mut self, out: &mut [u8]) -> ReadOut {
        let r = &*self.ring;
        let mut dropped = 0;
        loop {
            let head = r.head.load(Ordering::Acquire);
            let reserved = r.reserved.load(Ordering::Relaxed);
            let floor = reserved.saturating_sub(r.cap);
            if self.offset < floor {
                // Land with a quarter ring of headroom, or a fast producer
                // would overwrite the copy as we make it.
                let target = floor.max(head.saturating_sub(r.cap - r.cap / 4)).min(head);
                dropped += target - self.offset;
                self.offset = target;
            }
            if self.offset >= head {
                let eof = r.closed.load(Ordering::Acquire)
                    && self.offset >= r.head.load(Ordering::Acquire);
                return ReadOut { n: 0, dropped, eof };
            }
            let n = ((head - self.offset).min(out.len() as u64)) as usize;
            let start = (self.offset % r.cap) as usize;
            let first = n.min(r.cap as usize - start);
            for (o, slot) in out[..first].iter_mut().zip(&r.buf[start..start + first]) {
                *o = slot.load(Ordering::Relaxed);
            }
            for (o, slot) in out[first..n].iter_mut().zip(&r.buf[..n - first]) {
                *o = slot.load(Ordering::Relaxed);
            }
            fence(Ordering::Acquire);
            if r.reserved.load(Ordering::Relaxed).saturating_sub(r.cap) > self.offset {
                continue;
            }
            self.offset += n as u64;
            return ReadOut {
                n,
                dropped,
                eof: false,
            };
        }
    }

    /// Like `read`, but wait up to `timeout` (forever if `None`) for output.
    /// Returns with `n == 0` only on timeout or end of output; any drops seen
    /// meanwhile are reported then.
    pub fn read_wait(&mut self, out: &mut [u8], timeout: Option<Duration>) -> ReadOut {
        let deadline = timeout.map(|t| Instant::now() + t);
        let mut dropped = 0;
        loop {
            let mut o = self.read(out);
            dropped += o.dropped;
            if o.n > 0 || o.eof {
                o.dropped = dropped;
                return o;
            }
            let left = match deadline {
                None => None,
                Some(d) => match d.checked_duration_since(Instant::now()) {
                    Some(l) if !l.is_zero() => Some(l),
                    _ => {
                        return ReadOut {
                            n: 0,
                            dropped,
                            eof: false,
                        }
                    }
                },
            };
            self.ring.wait(self.offset, left);
        }
    }

    /// Read until `pattern` has appeared, leaving the reader just past it, or
    /// the time is up. Bytes dropped along the way break a match spanning them.
    pub fn wait_for(&mut self, pattern: &[u8], timeout: Duration) -> WaitResult {
        let deadline = Instant::now() + timeout;
        let keep = pattern.len().saturating_sub(1);
        let mut window: Vec<u8> = Vec::new();
        let mut chunk = vec![0u8; 8192];
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let o = self.read_wait(&mut chunk, Some(left));
            if o.dropped > 0 {
                window.clear();
            }
            if o.n > 0 {
                window.extend_from_slice(&chunk[..o.n]);
                if let Some(i) = find(&window, pattern) {
                    // Leave the reader just past the match, so the next step
                    // still sees what the guest printed after it.
                    let end = self.offset - (window.len() - (i + pattern.len())) as u64;
                    self.offset = end;
                    return WaitResult::Found(end);
                }
                if window.len() > keep {
                    window.drain(..window.len() - keep);
                }
            } else if o.eof {
                return WaitResult::Eof;
            } else if Instant::now() >= deadline {
                return WaitResult::Timeout;
            }
        }
    }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    hay.windows(needle.len()).position(|w| w == needle)
}
