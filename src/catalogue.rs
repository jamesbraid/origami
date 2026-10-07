use crate::{Catalog, Result};
use serde_json::Value;
use std::io::{self, BufRead, BufReader, Write};
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
    let (Some(mut stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
        return Err("QEMU's standard streams are not piped".into());
    };
    // On Windows QEMU drops stdio input that arrives while its monitor is
    // busy, so wait for the greeting and send each command after the
    // previous reply. A QEMU that exits early is reported from its output.
    let mut lines = BufReader::new(stdout).lines().map_while(io::Result::ok);
    let greeting = lines.next();
    let mut ask = |command: &str| {
        let _ = writeln!(stdin, "{{\"execute\": \"{command}\"}}");
        next_reply(&mut lines)
    };
    let reply = catalogue_reply(
        greeting
            .and_then(|_| ask("qmp_capabilities"))
            .and_then(|_| ask("query-sgi-machines")),
    );
    // Quit only after the reply is read: QEMU can exit with the end of a
    // long reply still unwritten.
    ask("quit");
    drop(lines);
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

/// The next command reply among QMP output `lines`, skipping events.
fn next_reply(lines: &mut impl Iterator<Item = String>) -> Option<Value> {
    lines
        .filter_map(|line| serde_json::from_str::<Value>(&line).ok())
        .find(|value| value.get("return").is_some() || value.get("error").is_some())
}

/// The catalogue in QEMU's `reply` to query-sgi-machines.
fn catalogue_reply(reply: Option<Value>) -> std::result::Result<String, String> {
    match reply {
        Some(Value::Object(mut reply)) => match reply.remove("return") {
            Some(catalogue) => Ok(catalogue.to_string()),
            None => Err(format!("query-sgi-machines failed: {}", reply["error"])),
        },
        _ => Err("QEMU did not answer query-sgi-machines".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reply_is_the_answer_to_the_catalogue_query() {
        let mut lines = concat!(
            "{\"event\": \"IGNORED\"}\n",
            "{\"return\": {\"schema\": \"sgi-machines\", \"offerings\": []}}\n",
            "{\"return\": {}}\n",
        )
        .lines()
        .map(String::from);
        let reply = catalogue_reply(next_reply(&mut lines)).unwrap();
        assert_eq!(parse(&reply).unwrap().offerings.len(), 0);
        let error = catalogue_reply(next_reply(
            &mut ["{\"error\": {\"class\": \"CommandNotFound\"}}".to_string()].into_iter(),
        ))
        .unwrap_err();
        assert!(error.contains("CommandNotFound"), "{error}");
        assert!(next_reply(&mut lines).is_some());
        assert!(catalogue_reply(next_reply(&mut lines)).is_err());
    }

    #[test]
    fn parse_rejects_another_schema() {
        let error = parse("{\"schema\": \"sgi-sn\", \"offerings\": []}")
            .unwrap_err()
            .to_string();
        assert!(error.contains("sgi-sn"), "{error}");
    }
}
