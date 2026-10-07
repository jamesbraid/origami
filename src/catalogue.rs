use crate::{control, Catalog, Result};
use std::io::BufReader;
use std::path::Path;
use std::process::{Command, Stdio};

pub const SCHEMA: &str = "sgi-machines";

/// Load the machine catalogue compiled into `qemu`.
pub fn load(qemu: &Path) -> Result<Catalog> {
    parse(&query(qemu)?)
}

pub fn parse(text: &str) -> Result<Catalog> {
    let catalog: Catalog = serde_json::from_str(text)
        .map_err(|error| format!("invalid QEMU machine catalogue: {error}"))?;
    if catalog.schema != SCHEMA {
        return Err(format!(
            "QEMU machine catalogue has schema {}, expected {SCHEMA}",
            catalog.schema
        )
        .into());
    }
    Ok(catalog)
}

/// Ask a short-lived QEMU with no machine for its catalogue over QMP.
fn query(qemu: &Path) -> Result<String> {
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
    // On Windows QEMU drops stdio input that arrives while its monitor is
    // busy, so wait for the greeting and send each command after the
    // previous reply. A QEMU that exits early is reported from its output.
    let mut qmp = qapi::Qmp::new(qapi::Stream::new(BufReader::new(stdout), stdin));
    let reply = control::handshake(&mut qmp)
        .and_then(|()| Ok(qmp.execute(&QuerySgiMachines {})?.to_string()));
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
    type Ok = serde_json::Value;
    const NAME: &'static str = "query-sgi-machines";
    const ALLOW_OOB: bool = false;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_rejects_another_schema() {
        let error = parse("{\"schema\": \"sgi-sn\", \"offerings\": []}")
            .unwrap_err()
            .to_string();
        assert!(error.contains("sgi-sn"), "{error}");
    }
}
