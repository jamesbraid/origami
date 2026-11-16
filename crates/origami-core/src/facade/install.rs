//! The install steps as operations: check the media, serve it, and drive
//! the installer on the guest's console.
//!
//! Instigator children are owned by the operation that started them: they
//! die when it is cancelled and when the core drops. The installer takes the
//! console's input lease through the runner (see [`super::lease`]), which
//! turns every other viewer read-only until the operation ends.

use super::child;
use super::core::Core;
use super::error::{CoreError, CoreResult, ErrorKind};
use super::lease::{self, InstallStart};
use super::operation::{Control as OpControl, Operation};
use origami_machine::install;
use std::cell::Cell;
use std::io::{self, Write};
use std::path::PathBuf;

fn cancelled(what: &str) -> CoreError {
    CoreError::new(ErrorKind::Cancelled, format!("{what} was cancelled"))
}

/// Turns the installer's console echo into progress lines.
struct ProgressSink<'a> {
    control: &'a OpControl,
    line: Vec<u8>,
}

impl Write for ProgressSink<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        for &byte in bytes {
            match byte {
                b'\n' | b'\r' => {
                    let text = String::from_utf8_lossy(&self.line);
                    if !text.trim().is_empty() {
                        self.control.progress(text.trim());
                    }
                    self.line.clear();
                }
                _ if self.line.len() < 512 => self.line.push(byte),
                _ => {}
            }
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Core {
    /// Check the install media of the machine at `key` with the Instigator
    /// at `instigator`. Cancelling ends the check and its child.
    pub fn install_check(
        &self,
        key: &str,
        full: bool,
        instigator: PathBuf,
    ) -> CoreResult<Operation> {
        let machine = self.open_machine(key)?;
        let (dir, engine) = (PathBuf::from(machine.key()), machine.engine());
        Ok(
            self.install_operation(&machine, "Check install media", move |control| {
                let loaded = crate::open(engine.as_ref(), &dir).map_err(CoreError::machine)?;
                control.progress("Checking the install media");
                let (mut command, document) =
                    install::check_command(|| Ok(instigator), &dir, &loaded.file, full)
                        .map_err(CoreError::machine)?;
                match child::run(&mut command, Some(document), control)? {
                    None => Err(cancelled("the check")),
                    Some(done) if done.status.success() => Ok(None),
                    Some(done) => Err(CoreError::new(
                        ErrorKind::Invalid,
                        install::check_rejected(&done.stderr),
                    )),
                }
            }),
        )
    }

    /// Serve the install media of the machine at `key` with the Instigator
    /// at `instigator`, until the operation is cancelled or the server
    /// exits. The server is the operation's child; cancelling ends it and
    /// anything it started.
    pub fn install_serve(&self, key: &str, instigator: PathBuf) -> CoreResult<Operation> {
        let machine = self.open_machine(key)?;
        let (dir, engine) = (PathBuf::from(machine.key()), machine.engine());
        Ok(
            self.install_operation(&machine, "Serve install media", move |control| {
                let loaded = crate::open(engine.as_ref(), &dir).map_err(CoreError::machine)?;
                let install::ServeSetup {
                    mut command,
                    log_path,
                    ..
                } = install::serve_setup(|| Ok(instigator), &dir, &loaded.file)
                    .map_err(CoreError::machine)?;
                control.progress(format!("Instigator log: {}", log_path.display()));
                match child::run(&mut command, None, control)? {
                    None => Err(cancelled("the media server")),
                    Some(done) if done.status.success() => Ok(None),
                    Some(done) => Err(CoreError::new(
                        ErrorKind::Io,
                        format!("Instigator exited with {}", done.status),
                    )),
                }
            }),
        )
    }

    /// Drive the installer on the console of the machine at `key` from
    /// `Inst>` to the installed system's login prompt. The operation holds
    /// the console's input lease and releases it when it ends, is cancelled
    /// or loses its connection; cancelling leaves the machine running.
    pub fn install_apply(&self, key: &str, start: InstallStart) -> CoreResult<Operation> {
        let machine = self.open_machine(key)?;
        let (dir, engine) = (PathBuf::from(machine.key()), machine.engine());
        Ok(
            self.install_operation(&machine, "Install IRIX", move |control| {
                let loaded = crate::open(engine.as_ref(), &dir).map_err(CoreError::machine)?;
                let start = Cell::new(Some(start));
                let cancel = control.canceller();
                let mut sink = ProgressSink {
                    control,
                    line: Vec::new(),
                };
                let result = install::apply(
                    &dir,
                    &loaded.file,
                    &|dir| {
                        let start = start.take().expect("the installer connects once");
                        lease::connect(dir, start, cancel.clone())
                    },
                    &mut sink,
                );
                match result {
                    Ok(()) => Ok(None),
                    Err(_) if control.is_cancelled() => Err(cancelled("the install")),
                    Err(error) => Err(CoreError::machine(error)),
                }
            }),
        )
    }
}
