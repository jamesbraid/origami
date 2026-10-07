use crate::{Catalog, Result};
use std::io::BufReader;
use std::path::Path;
use std::process::{Command, Stdio};

/// Ask a short-lived QEMU with no machine for the catalogue compiled into it.
pub fn load(qemu: &Path) -> Result<Catalog> {
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
    // Quit only after the reply is read: QEMU can exit with the end of a
    // long reply still unwritten.
    let _ = qmp.execute(&qapi::qmp::quit {});
    drop(qmp);
    let output = child.wait_with_output()?;
    reply.map_err(|error| {
        format!(
            "cannot read the machine catalogue from {}: {error} {}",
            qemu.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into()
    })
}

#[derive(serde::Serialize)]
struct QuerySgiMachines {}

impl qapi::Command for QuerySgiMachines {
    type Ok = Catalog;
    const NAME: &'static str = "query-sgi-machines";
    const ALLOW_OOB: bool = false;
}
