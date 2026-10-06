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
    // QEMU reads the commands in order once its monitor starts. One that
    // exits first is reported from its output below.
    let _ = stdin
        .write_all(b"{\"execute\": \"qmp_capabilities\"}\n{\"execute\": \"query-sgi-machines\"}\n");
    let mut stdout = BufReader::new(stdout);
    let reply = catalogue_reply((&mut stdout).lines().map_while(io::Result::ok));
    // Quit only after the reply is read: QEMU can exit with the end of a
    // long reply still unwritten.
    let _ = stdin.write_all(b"{\"execute\": \"quit\"}\n");
    drop(stdin);
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

/// The query-sgi-machines reply among QMP output `lines`: the second reply,
/// after the one to qmp_capabilities. Reads no further than that reply.
fn catalogue_reply(lines: impl Iterator<Item = String>) -> std::result::Result<String, String> {
    let mut replies = lines
        .filter_map(|line| serde_json::from_str::<Value>(&line).ok())
        .filter(|value| value.get("return").is_some() || value.get("error").is_some());
    match replies.nth(1) {
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
        let transcript = concat!(
            "{\"QMP\": {\"version\": {}, \"capabilities\": []}}\n",
            "{\"return\": {}}\n",
            "{\"event\": \"IGNORED\"}\n",
            "{\"return\": {\"schema\": \"sgi-machines\", \"offerings\": []}}\n",
            "{\"return\": {}}\n",
        );
        let reply = catalogue_reply(transcript.lines().map(String::from)).unwrap();
        assert_eq!(parse(&reply).unwrap().offerings.len(), 0);
        let error = catalogue_reply(
            concat!(
                "{\"QMP\": {}}\n",
                "{\"return\": {}}\n",
                "{\"error\": {\"class\": \"CommandNotFound\"}}\n",
            )
            .lines()
            .map(String::from),
        )
        .unwrap_err();
        assert!(error.contains("CommandNotFound"), "{error}");
    }

    #[test]
    fn parse_rejects_another_schema() {
        let error = parse("{\"schema\": \"sgi-sn\", \"offerings\": []}")
            .unwrap_err()
            .to_string();
        assert!(error.contains("sgi-sn"), "{error}");
    }
}
