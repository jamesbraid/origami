#![cfg(unix)]

mod common;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Output;

fn run(executable: &Path, root: &Path, args: &[&str]) -> Output {
    common::origami(executable, root)
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn install_serve_preserves_each_runs_output_and_capture_on_failure() {
    let root = tempfile::tempdir().unwrap();
    let executable = root.path().join("origami");
    fs::copy(env!("CARGO_BIN_EXE_origami"), &executable).unwrap();
    let server = root.path().join("instigator");
    fs::write(
        &server,
        r#"#!/bin/sh
printf 'server stdout\n'
printf 'server stderr\n' >&2
while [ "$#" -gt 0 ]; do
    if [ "$1" = --capture-dir ]; then
        printf '{"event":"command_exit","elapsed_ms":125}\n' > "$2/events.jsonl"
        exit 7
    fi
    shift
done
exit 7
"#,
    )
    .unwrap();
    fs::set_permissions(&server, fs::Permissions::from_mode(0o755)).unwrap();
    let prom = root.path().join("synthetic-prom.bin");
    fs::write(&prom, vec![0; 1024 * 1024]).unwrap();
    let machine = root.path().join("machine with spaces");
    for args in [
        vec![
            "create",
            machine.to_str().unwrap(),
            "--preset",
            "origin200-1",
            "--prom",
            prom.to_str().unwrap(),
        ],
        vec![
            "install-init",
            machine.to_str().unwrap(),
            "--mac",
            "08:00:69:12:34:56",
            "--profile",
            "desktop",
        ],
    ] {
        let output = run(&executable, root.path(), &args);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    for _ in 0..2 {
        let output = run(
            &executable,
            root.path(),
            &["install-serve", machine.to_str().unwrap()],
        );
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("server.log"));
    }
    let captures: Vec<_> = fs::read_dir(machine.join("install"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.is_dir()
                && path
                    .file_name()
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .starts_with("instigator-")
        })
        .collect();
    assert_eq!(captures.len(), 2, "each attempt must keep its own evidence");
    for capture in captures {
        assert_eq!(
            fs::read_to_string(capture.join("server.log")).unwrap(),
            "server stdout\nserver stderr\n"
        );
        assert_eq!(
            fs::read_to_string(capture.join("events.jsonl")).unwrap(),
            "{\"event\":\"command_exit\",\"elapsed_ms\":125}\n"
        );
    }
}
