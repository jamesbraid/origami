use crate::{Catalog, Result};
use serde_json::{json, Value};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant, UNIX_EPOCH};

pub const SCHEMA: &str = "sgi-machines";

/// Load the machine catalogue compiled into `qemu`, reusing a cached reply
/// while the binary's path, size and modification time are unchanged.
pub fn load(qemu: &Path) -> Result<Catalog> {
    load_cached(
        qemu,
        dirs::cache_dir().map(|root| root.join("origami/catalogue")),
    )
}

fn load_cached(qemu: &Path, cache_dir: Option<PathBuf>) -> Result<Catalog> {
    let cache = cache_dir.and_then(|dir| cache_path(&dir, qemu));
    if let Some(text) = cache
        .as_ref()
        .and_then(|path| fs::read_to_string(path).ok())
    {
        if let Ok(catalog) = parse(&text) {
            return Ok(catalog);
        }
    }
    let text = query(qemu)?;
    let catalog = parse(&text)?;
    if let Some(path) = cache {
        // The cache only saves a QEMU start; a failed write costs nothing else.
        let _ = store(&path, &text);
    }
    Ok(catalog)
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

fn cache_path(dir: &Path, qemu: &Path) -> Option<PathBuf> {
    let path = qemu.canonicalize().ok()?;
    let metadata = fs::metadata(&path).ok()?;
    let modified = metadata
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_nanos();
    let key = format!("{}\0{}\0{modified}", path.display(), metadata.len());
    Some(dir.join(format!("{}.json", crate::sha256_hex(key.as_bytes()))))
}

fn store(path: &Path, text: &str) -> Result<()> {
    let parent = path.parent().ok_or("catalogue cache has no parent")?;
    fs::create_dir_all(parent)?;
    let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
    fs::write(&temporary, text)?;
    fs::rename(&temporary, path).inspect_err(|_| {
        let _ = fs::remove_file(&temporary);
    })?;
    Ok(())
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
    let stdin = child.stdin.take().ok_or("QEMU stdin unavailable")?;
    let stdout = child.stdout.take().ok_or("QEMU stdout unavailable")?;
    let mut stderr = child.stderr.take().ok_or("QEMU stderr unavailable")?;
    let errors = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = stderr.read_to_string(&mut text);
        text
    });
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = sender.send(conversation(stdin, BufReader::new(stdout)));
    });
    let reply = match receiver.recv_timeout(Duration::from_secs(30)) {
        Ok(reply) => reply,
        Err(_) => Err("timed out".into()),
    };
    // The reply is complete; a QEMU slow to honour quit is not waited on.
    let deadline = Instant::now() + Duration::from_secs(5);
    while reply.is_ok() && Instant::now() < deadline {
        if !matches!(child.try_wait(), Ok(None)) {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = child.kill();
    let _ = child.wait();
    let errors = errors.join().unwrap_or_default();
    reply.map_err(|error| {
        let detail = errors.trim();
        let detail = if detail.is_empty() {
            String::new()
        } else {
            format!(": {detail}")
        };
        format!(
            "cannot read the machine catalogue from {}: {error}{detail}",
            qemu.display()
        )
        .into()
    })
}

fn conversation(
    mut input: impl Write,
    mut output: impl BufRead,
) -> std::result::Result<String, String> {
    let mut greeting = String::new();
    output
        .read_line(&mut greeting)
        .map_err(|error| error.to_string())?;
    if serde_json::from_str::<Value>(&greeting)
        .ok()
        .and_then(|value| value.get("QMP").cloned())
        .is_none()
    {
        return Err("QEMU did not send a QMP greeting".into());
    }
    execute(&mut input, &mut output, "qmp_capabilities")?;
    let reply = execute(&mut input, &mut output, "query-sgi-machines")?;
    let _ = execute(&mut input, &mut output, "quit");
    Ok(reply.to_string())
}

fn execute(
    input: &mut impl Write,
    output: &mut impl BufRead,
    name: &str,
) -> std::result::Result<Value, String> {
    writeln!(input, "{}", json!({ "execute": name })).map_err(|error| error.to_string())?;
    input.flush().map_err(|error| error.to_string())?;
    loop {
        let mut line = String::new();
        if output
            .read_line(&mut line)
            .map_err(|error| error.to_string())?
            == 0
        {
            return Err(format!("QMP closed before answering {name}"));
        }
        let mut value: Value = serde_json::from_str(&line)
            .map_err(|error| format!("invalid QMP reply to {name}: {error}"))?;
        if let Some(error) = value.get("error") {
            return Err(format!("QMP {name} failed: {error}"));
        }
        if let Some(reply) = value.get_mut("return") {
            return Ok(reply.take());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversation_negotiates_then_returns_the_catalogue() {
        let replies = concat!(
            "{\"QMP\": {\"version\": {}, \"capabilities\": []}}\n",
            "{\"return\": {}}\n",
            "{\"event\": \"IGNORED\"}\n",
            "{\"return\": {\"schema\": \"sgi-machines\", \"version\": 1, \"offerings\": []}}\n",
            "{\"return\": {}}\n",
        );
        let mut sent = Vec::new();
        let reply = conversation(&mut sent, replies.as_bytes()).unwrap();
        assert_eq!(parse(&reply).unwrap().offerings.len(), 0);
        let sent = String::from_utf8(sent).unwrap();
        let commands: Vec<_> = sent.lines().collect();
        assert_eq!(
            commands,
            [
                "{\"execute\":\"qmp_capabilities\"}",
                "{\"execute\":\"query-sgi-machines\"}",
                "{\"execute\":\"quit\"}"
            ]
        );
    }

    #[test]
    fn conversation_reports_a_missing_command() {
        let replies = concat!(
            "{\"QMP\": {}}\n",
            "{\"return\": {}}\n",
            "{\"error\": {\"class\": \"CommandNotFound\"}}\n",
        );
        let error = conversation(Vec::new(), replies.as_bytes()).unwrap_err();
        assert!(error.contains("query-sgi-machines"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn cached_reply_is_reused_until_the_binary_changes() {
        let root = std::env::temp_dir().join(format!(
            "origami-catalogue-cache-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&root).unwrap();
        let qemu = root.join("qemu-system-mips64");
        let starts = root.join("starts");
        let write_qemu = |extra: &str| {
            crate::write_script(
                &qemu,
                &format!(
                    "#!/bin/sh\necho start >> '{}'\n{extra}\nprintf '{{\"QMP\": {{}}}}\\n'\nwhile read -r line; do\n  case \"$line\" in\n  *query-sgi-machines*) printf '{{\"return\": {{\"schema\": \"sgi-machines\", \"offerings\": []}}}}\\n' ;;\n  *) printf '{{\"return\": {{}}}}\\n' ;;\n  esac\ndone\n",
                    starts.display()
                ),
            );
        };
        write_qemu("");
        let cache = Some(root.join("cache"));
        load_cached(&qemu, cache.clone()).unwrap();
        load_cached(&qemu, cache.clone()).unwrap();
        assert_eq!(fs::read_to_string(&starts).unwrap().lines().count(), 1);
        write_qemu("# replaced");
        load_cached(&qemu, cache).unwrap();
        assert_eq!(fs::read_to_string(&starts).unwrap().lines().count(), 2);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn parse_rejects_another_schema() {
        let error = parse("{\"schema\": \"sgi-sn\", \"offerings\": []}")
            .unwrap_err()
            .to_string();
        assert!(error.contains("sgi-sn"), "{error}");
    }
}
