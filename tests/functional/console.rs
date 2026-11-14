//! An expect-style layer over the product's console client.
//!
//! `origami run --background` serves the guest's serial console on a loopback
//! port, which `origami::console::Console` connects to. This adds what a test
//! needs on top: a log of every byte, a buffer to match regular expressions
//! against, and a shell command helper.

use std::fs::File;
use std::io::Write;
use std::path::Path;
use std::time::{Duration, Instant};

use regex::Regex;

pub struct Console {
    inner: origami::console::Console,
    buffer: String,
    log: File,
    markers: u32,
}

impl Console {
    pub fn connect(machine: &Path, log: &Path) -> Result<Self, String> {
        let inner =
            origami::console::Console::connect(machine).map_err(|e| format!("console: {e}"))?;
        let log = File::options()
            .create(true)
            .append(true)
            .open(log)
            .map_err(|e| format!("console log: {e}"))?;
        Ok(Self {
            inner,
            buffer: String::new(),
            log,
            markers: 0,
        })
    }

    /// Read what the guest has sent, waiting at most a second.
    fn pump(&mut self) -> Result<(), String> {
        let mut chunk = [0u8; 65536];
        match self
            .inner
            .read_some(&mut chunk)
            .map_err(|e| format!("console: {e}"))?
        {
            Some(0) => Err("console closed".into()),
            Some(count) => {
                let _ = self.log.write_all(&chunk[..count]);
                let _ = self.log.flush();
                // IRIX writes bytes, not UTF-8; Latin-1 keeps every one.
                self.buffer.extend(
                    chunk[..count]
                        .iter()
                        .filter(|b| **b != b'\r')
                        .map(|b| *b as char),
                );
                Ok(())
            }
            None => Ok(()),
        }
    }

    pub fn send(&mut self, text: &str) -> Result<(), String> {
        self.inner.send(text).map_err(|e| format!("console: {e}"))
    }

    pub fn line(&mut self, text: &str) -> Result<(), String> {
        self.send(&format!("{text}\r"))
    }

    /// Wait for `pattern`, drop the buffer up to its end and return the text
    /// before it with the match's capture groups.
    pub fn expect(
        &mut self,
        pattern: &str,
        timeout: Duration,
    ) -> Result<(String, Vec<String>), String> {
        let rx = Regex::new(pattern).map_err(|e| e.to_string())?;
        let end = Instant::now() + timeout;
        loop {
            if let Some(found) = rx.captures(&self.buffer) {
                let whole = found.get(0).unwrap();
                let (start, stop) = (whole.start(), whole.end());
                let groups = found
                    .iter()
                    .skip(1)
                    .map(|g| g.map_or_else(String::new, |g| g.as_str().to_string()))
                    .collect();
                let before = self.buffer[..start].to_string();
                self.buffer.drain(..stop);
                return Ok((before, groups));
            }
            if Instant::now() > end {
                let skip = self.buffer.chars().count().saturating_sub(600);
                let tail: String = self.buffer.chars().skip(skip).collect();
                return Err(format!("waiting for {pattern:?}; tail: {tail:?}"));
            }
            self.pump()?;
        }
    }

    /// Type a shell command and return its exit status and output, without
    /// the echoed command line.
    pub fn run(&mut self, command: &str, timeout: Duration) -> Result<(i32, String), String> {
        self.buffer.clear();
        self.markers += 1;
        let marker = format!("__done_{}__", self.markers);
        self.line(&format!("{command}; echo {marker}$?"))?;
        let (before, found) = self.expect(&format!(r"{marker}(\d+)\n"), timeout)?;
        let status = found[0].parse().map_err(|_| "no exit status")?;
        // The shell echoes the command first; its next prompt can arrive ahead
        // of the output.
        let mut lines = before.split('\n');
        lines.next();
        let output = lines.collect::<Vec<_>>().join("\n");
        let prompt = Regex::new(r"^(# )+").unwrap();
        Ok((status, prompt.replace(output.trim(), "").into_owned()))
    }

    /// Log in as root on the serial console and leave a Bourne shell, so
    /// that `$?` means what the checks expect.
    pub fn login(&mut self, timeout: Duration) -> Result<(), String> {
        let end = Instant::now() + timeout;
        self.line("")?;
        // A machine whose NVRAM the PROM has just initialised waits at its
        // System Maintenance Menu; option 1 starts the system. `origami nvram`
        // cannot set AutoLoad before that first boot.
        loop {
            let left = end.saturating_duration_since(Instant::now());
            let (_, seen) = self.expect(r"(Option\? |login: )", left)?;
            if seen[0] == "login: " {
                break;
            }
            self.line("1")?;
        }
        self.line("root")?;
        self.expect(r"TERM = \(vt100\) ", Duration::from_secs(120))?;
        self.line("")?;
        self.expect("# $", Duration::from_secs(120))?;
        self.line("exec /bin/sh")?;
        std::thread::sleep(Duration::from_secs(2));
        self.run("true", Duration::from_secs(60))?;
        Ok(())
    }
}
