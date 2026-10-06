use crate::{Catalog, Result};
use serde_json::Value;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::UNIX_EPOCH;

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
    // A reader that meets a partly written file fails to parse it and asks
    // QEMU again, so the write needs no locking.
    fs::create_dir_all(path.parent().ok_or("catalogue cache has no parent")?)?;
    fs::write(path, text)?;
    Ok(())
}

const QMP_COMMANDS: &str = concat!(
    "{\"execute\": \"qmp_capabilities\"}\n",
    "{\"execute\": \"query-sgi-machines\"}\n",
    "{\"execute\": \"quit\"}\n",
);

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
    // QEMU reads the commands in order once its monitor starts. One that
    // exits first is reported from its output below.
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(QMP_COMMANDS.as_bytes());
    }
    let output = child.wait_with_output()?;
    catalogue_reply(&String::from_utf8_lossy(&output.stdout)).map_err(|error| {
        format!(
            "cannot read the machine catalogue from {}: {error} {}",
            qemu.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into()
    })
}

/// The query-sgi-machines reply in a QMP transcript: the second reply,
/// after the one to qmp_capabilities.
fn catalogue_reply(transcript: &str) -> std::result::Result<String, String> {
    let mut replies = transcript
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
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
        let reply = catalogue_reply(transcript).unwrap();
        assert_eq!(parse(&reply).unwrap().offerings.len(), 0);
        let error = catalogue_reply(concat!(
            "{\"QMP\": {}}\n",
            "{\"return\": {}}\n",
            "{\"error\": {\"class\": \"CommandNotFound\"}}\n",
        ))
        .unwrap_err();
        assert!(error.contains("CommandNotFound"), "{error}");
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
