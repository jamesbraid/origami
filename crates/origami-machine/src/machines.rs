use crate::{Machines, Result, MACHINES_VERSION};
use std::io::BufReader;
use std::path::Path;
use std::process::{Command, Stdio};

/// Ask a short-lived QEMU with no machine for the machine list compiled into it.
pub fn load(qemu: &Path) -> Result<Machines> {
    let mut child = Command::new(qemu)
        .args([
            "-M",
            "none",
            "-qmp",
            "stdio",
            "-nodefaults",
            "-display",
            "none",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("cannot start {}: {error}", qemu.display()))?;
    let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
        return Err("QEMU's standard streams are not piped".into());
    };
    let mut qmp = qapi::Qmp::new(qapi::Stream::new(BufReader::new(stdout), stdin));
    let reply = qmp
        .handshake()
        .and_then(|_| qmp.execute(&QuerySgiMachines {}));
    // Stop QEMU only after the reply is read: it can exit with the end of a
    // long reply still unwritten. Kill it rather than send quit, because
    // QEMU may exit before the rest of that command is written, and the
    // write to its closed stdin then ends this process with SIGPIPE.
    drop(qmp);
    let _ = child.kill();
    let output = child.wait_with_output()?;
    let machines = reply.map_err(|error| {
        format!(
            "cannot read the machine list from {}: {error} {}",
            qemu.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        )
    })?;
    check_version(machines, qemu)
}

/// Refuse a machine list of another version: this Origami reads only the
/// version it was built with.
pub fn check_version(machines: Machines, qemu: &Path) -> Result<Machines> {
    if machines.version == MACHINES_VERSION {
        return Ok(machines);
    }
    Err(format!(
        "{} reports machine list version {}, but this Origami reads version {MACHINES_VERSION}; \
         use the QEMU it was built with",
        qemu.display(),
        machines.version
    )
    .into())
}

#[derive(serde::Serialize)]
struct QuerySgiMachines {}

impl qapi::Command for QuerySgiMachines {
    type Ok = Machines;
    const NAME: &'static str = "query-sgi-machines";
    const ALLOW_OOB: bool = false;
}
