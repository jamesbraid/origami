//! A client for a running machine's serial console.
//!
//! The background runner exposes the console as a loopback TCP socket, whose
//! port `state/run.json` records. The client writes whatever it reads to a
//! `Write` the caller supplies, so a command line can show it and a test can
//! discard it.

use crate::{control, Result};
use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, SocketAddrV4, TcpStream};
use std::time::{Duration, Instant};

/// How much output `wait_for` keeps to look for the prompt in.
const KEEP: usize = 65536;

pub struct Console {
    stream: TcpStream,
}

impl Console {
    /// Connect to the console port the run record names.
    pub fn open(record: &control::Record) -> Result<Self> {
        let address = SocketAddrV4::new(Ipv4Addr::LOCALHOST, record.console_port);
        let stream = TcpStream::connect_timeout(&address.into(), Duration::from_secs(5))?;
        stream.set_read_timeout(Some(Duration::from_secs(1)))?;
        stream.set_write_timeout(Some(Duration::from_secs(5)))?;
        Ok(Self { stream })
    }

    /// A console over a stream the caller has connected. A read timeout on
    /// the stream sets how often a wait looks at its deadline.
    pub fn from_stream(stream: TcpStream) -> Self {
        Self { stream }
    }

    /// A second handle on the connection, so one thread can write while
    /// another reads.
    pub fn try_clone_stream(&self) -> Result<TcpStream> {
        Ok(self.stream.try_clone()?)
    }

    /// Read what the guest has written. Returns None when the read timed out
    /// or was interrupted, and Some(0) when the guest closed the console.
    pub fn read_some(&mut self, buffer: &mut [u8]) -> Result<Option<usize>> {
        match self.stream.read(buffer) {
            Ok(count) => Ok(Some(count)),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock
                        | io::ErrorKind::TimedOut
                        | io::ErrorKind::Interrupted
                ) =>
            {
                Ok(None)
            }
            Err(error) => Err(error.into()),
        }
    }

    /// Send `text` to the guest as typed.
    pub fn send(&mut self, text: impl AsRef<[u8]>) -> Result<()> {
        self.stream.write_all(text.as_ref())?;
        Ok(())
    }

    /// Echo the guest's output to `out` until it ends with `prompt`, and
    /// return the last 64 KiB of it.
    pub fn wait_for(
        &mut self,
        prompt: &[u8],
        timeout: Duration,
        out: &mut dyn Write,
    ) -> Result<String> {
        let deadline = Instant::now() + timeout;
        let mut output = Vec::new();
        let mut chunk = [0u8; 4096];
        while Instant::now() < deadline {
            match self.read_some(&mut chunk)? {
                Some(0) => return Err("guest serial console closed".into()),
                Some(count) => {
                    out.write_all(&chunk[..count])?;
                    out.flush()?;
                    output.extend_from_slice(&chunk[..count]);
                    if output.len() > KEEP {
                        output.drain(..output.len() - KEEP);
                    }
                    if output.ends_with(prompt) {
                        return Ok(String::from_utf8_lossy(&output).into_owned());
                    }
                }
                None => {}
            }
        }
        Err(format!(
            "guest did not reach {} within {} seconds; machine remains running",
            String::from_utf8_lossy(prompt),
            timeout.as_secs()
        )
        .into())
    }

    /// Type `command` and a newline, then wait for `prompt`.
    pub fn command(
        &mut self,
        command: &str,
        prompt: &[u8],
        timeout: Duration,
        out: &mut dyn Write,
    ) -> Result<String> {
        self.send(command)?;
        self.send("\n")?;
        self.wait_for(prompt, timeout, out)
    }
}
