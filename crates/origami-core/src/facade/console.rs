//! A machine's primary console: a byte ring with sequence offsets, so any
//! number of readers can follow it, and the write half.

use super::error::{CoreError, ErrorKind};
use super::machine::ConsoleChunk;
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};

struct Ring {
    data: VecDeque<u8>,
    /// The offset of `data[0]`.
    start: u64,
    connected: bool,
    down: Option<String>,
    generation: u64,
    /// Offset where output printed after the connection begins.
    live_from: u64,
}

pub(crate) struct Console {
    cap: usize,
    ring: Mutex<Ring>,
    writer: Mutex<Option<Box<dyn Write + Send>>>,
}

impl Console {
    pub(crate) fn new(cap: usize) -> Arc<Console> {
        Arc::new(Console {
            cap,
            ring: Mutex::new(Ring {
                data: VecDeque::new(),
                start: 0,
                connected: false,
                down: Some("the machine has not started".into()),
                generation: 0,
                live_from: 0,
            }),
            writer: Mutex::new(None),
        })
    }

    /// A fresh start: the offsets begin again at zero.
    pub(crate) fn reset(&self) {
        {
            let mut ring = self.ring.lock().unwrap();
            ring.data.clear();
            ring.start = 0;
            ring.generation += 1;
            ring.live_from = 0;
            ring.connected = false;
            ring.down = Some("the console is not open".into());
        }
        *self.writer.lock().unwrap() = None;
    }

    /// `history` is how many bytes the reader will first replay.
    pub(crate) fn connect(&self, writer: Box<dyn Write + Send>, history: u64) {
        *self.writer.lock().unwrap() = Some(writer);
        let mut ring = self.ring.lock().unwrap();
        ring.live_from = history;
        ring.connected = true;
        ring.down = None;
    }

    pub(crate) fn down(&self, reason: String) {
        {
            let mut ring = self.ring.lock().unwrap();
            ring.connected = false;
            ring.down = Some(reason);
        }
        *self.writer.lock().unwrap() = None;
    }

    /// Read until the line ends, calling `news` after each chunk.
    pub(crate) fn pump(&self, mut reader: Box<dyn Read + Send>, news: impl Fn()) {
        let mut buf = [0u8; 4096];
        let reason = loop {
            match reader.read(&mut buf) {
                Ok(0) => break "the console closed".to_string(),
                Ok(n) => {
                    self.append(&buf[..n]);
                    news();
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(error) => break error.to_string(),
            }
        };
        self.down(reason);
    }

    fn append(&self, bytes: &[u8]) {
        let mut ring = self.ring.lock().unwrap();
        ring.data.extend(bytes);
        let over = ring.data.len().saturating_sub(self.cap);
        if over > 0 {
            ring.data.drain(..over);
            ring.start += over as u64;
        }
    }

    pub(crate) fn read(&self, from: u64, max: usize) -> ConsoleChunk {
        let ring = self.ring.lock().unwrap();
        let end = ring.start + ring.data.len() as u64;
        let first = from.clamp(ring.start, end);
        let dropped = first - from.min(first);
        let count = ((end - first) as usize).min(max);
        let skip = (first - ring.start) as usize;
        let bytes = ring.data.iter().skip(skip).take(count).copied().collect();
        ConsoleChunk {
            bytes,
            next: first + count as u64,
            dropped,
            generation: ring.generation,
            live_from: ring.live_from,
        }
    }

    pub(crate) fn send(&self, bytes: &[u8]) -> Result<(), CoreError> {
        let mut guard = self.writer.lock().unwrap();
        let Some(writer) = guard.as_mut() else {
            drop(guard);
            let why = self.ring.lock().unwrap().down.clone();
            return Err(CoreError::new(
                ErrorKind::NotRunning,
                why.unwrap_or_else(|| "the console is not connected".into()),
            ));
        };
        writer
            .write_all(bytes)
            .and_then(|()| writer.flush())
            .map_err(CoreError::from)
    }

    pub(crate) fn status(&self) -> (bool, Option<String>, u64) {
        let ring = self.ring.lock().unwrap();
        (
            ring.connected,
            ring.down.clone(),
            ring.start + ring.data.len() as u64,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_output_begins_after_the_replayed_history() {
        let console = Console::new(1024);
        // A terminal that opens first sees no live output yet.
        assert_eq!(console.read(0, 16).live_from, 0);
        console.connect(Box::new(std::io::sink()), 5);
        console.append(b"\x1b[c old\x1b[c");
        let chunk = console.read(0, 64);
        assert_eq!(chunk.live_from, 5);
        console.reset();
        assert_eq!(console.read(0, 16).live_from, 0);
    }
}
