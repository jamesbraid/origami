//! An expect-style client for a background machine's serial console.
//!
//! `origami run --background` serves the console on a loopback TCP port named
//! in `state/run.json`. Only one client may hold it at a time.

use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::path::Path;
use std::time::{Duration, Instant};

use regex::Regex;

use super::{pause, Result};

/// How much unmatched console text to keep. A match can only reach back this
/// far, and an install prints megabytes.
const KEEP: usize = 256 * 1024;

pub struct Console {
    stream: TcpStream,
    /// Console text since the last match, carriage returns removed.
    pub buffer: String,
    log: Option<File>,
}

impl Console {
    /// Connect to the machine's console, waiting up to a minute for `run`
    /// to publish its port. Everything read is appended to `log`.
    pub fn connect(machine: &Path, log: Option<&Path>) -> Result<Console> {
        let end = Instant::now() + Duration::from_secs(60);
        loop {
            match Self::try_connect(machine, log) {
                Ok(con) => return Ok(con),
                Err(e) if Instant::now() > end => {
                    return Err(format!("no console for {}: {e}", machine.display()).into());
                }
                Err(_) => pause(Duration::from_secs(1)),
            }
        }
    }

    fn try_connect(machine: &Path, log: Option<&Path>) -> Result<Console> {
        let record: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(machine.join("state/run.json"))?)?;
        let port = record["console_port"]
            .as_u64()
            .ok_or("run.json has no console_port")?;
        let stream = TcpStream::connect(("127.0.0.1", port as u16))?;
        stream.set_read_timeout(Some(Duration::from_millis(500)))?;
        let log = match log {
            Some(path) => Some(OpenOptions::new().create(true).append(true).open(path)?),
            None => None,
        };
        Ok(Console {
            stream,
            buffer: String::new(),
            log,
        })
    }

    pub fn close(&mut self) {
        let _ = self.stream.shutdown(Shutdown::Both);
    }

    /// Read what is available, for up to the socket's half-second timeout.
    fn pump(&mut self) -> Result<()> {
        let mut data = [0u8; 65536];
        match self.stream.read(&mut data) {
            Ok(0) => Err("console closed".into()),
            Ok(n) => {
                if let Some(log) = &mut self.log {
                    log.write_all(&data[..n])?;
                    log.flush()?;
                }
                // Latin-1: every byte is one char, so a multibyte sequence
                // split across reads cannot corrupt the buffer.
                self.buffer.extend(
                    data[..n]
                        .iter()
                        .filter(|&&b| b != b'\r')
                        .map(|&b| b as char),
                );
                if self.buffer.len() > KEEP * 2 {
                    let mut cut = self.buffer.len() - KEEP;
                    while !self.buffer.is_char_boundary(cut) {
                        cut += 1;
                    }
                    self.buffer.drain(..cut);
                }
                Ok(())
            }
            // A timeout is `WouldBlock` on Unix and `TimedOut` on Windows;
            // a signal during the read is not an error either.
            Err(e)
                if matches!(
                    e.kind(),
                    ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::Interrupted
                ) =>
            {
                Ok(())
            }
            Err(e) => Err(e.into()),
        }
    }

    pub fn send(&mut self, text: &str) -> Result<()> {
        self.stream.write_all(text.as_bytes())?;
        Ok(())
    }

    pub fn line(&mut self, text: &str) -> Result<()> {
        self.send(&format!("{text}\r"))
    }

    /// Wait for a regex. Returns the text before the match and the whole
    /// match, and drops everything up to the end of the match.
    pub fn expect(&mut self, pattern: &str, timeout: Duration) -> Result<(String, String)> {
        let rx = Regex::new(pattern)?;
        let end = Instant::now() + timeout;
        loop {
            if let Some(m) = rx.find(&self.buffer) {
                let before = self.buffer[..m.start()].to_string();
                let matched = m.as_str().to_string();
                self.buffer.drain(..m.end());
                return Ok((before, matched));
            }
            if Instant::now() > end {
                let tail: String = self
                    .buffer
                    .chars()
                    .rev()
                    .take(600)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect();
                return Err(format!("timed out waiting for {pattern:?}; tail: {tail:?}").into());
            }
            self.pump()?;
        }
    }

    /// Run a shell command and return its exit status and output, with the
    /// echoed command line still at the front.
    pub fn run(&mut self, command: &str, timeout: Duration) -> Result<(i32, String)> {
        self.buffer.clear();
        let marker = format!(
            "__done_{}__",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_millis() % 100_000)
        );
        self.line(&format!("{command}; echo {marker}$?"))?;
        // The echoed command line carries `$?`, not digits, so only the
        // real marker matches.
        let (before, matched) = self.expect(&format!(r"{marker}\d+\n"), timeout)?;
        let status = matched[marker.len()..].trim().parse()?;
        Ok((status, before))
    }
}

/// Log in as root on the serial console and leave a Bourne shell, so `$?`
/// means what the tests expect.
pub fn login(con: &mut Console, timeout: Duration) -> Result<()> {
    con.line("")?;
    con.expect(r"login: ", timeout)?;
    con.line("root")?;
    con.expect(r"TERM = \(vt100\) ", Duration::from_secs(120))?;
    con.line("")?;
    con.expect(r"# $", Duration::from_secs(120))?;
    con.line("exec /bin/sh")?;
    pause(Duration::from_secs(2));
    Ok(())
}

impl Console {
    /// Keep reading for `d`, so late output reaches the log.
    pub fn drain(&mut self, d: Duration) -> Result<()> {
        let end = Instant::now() + d;
        while Instant::now() < end {
            self.pump()?;
        }
        Ok(())
    }
}
