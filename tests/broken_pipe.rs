#![cfg(unix)]

use std::os::unix::process::ExitStatusExt;
use std::process::Command;

#[test]
fn printing_to_a_closed_pipe_ends_quietly() {
    let (reader, writer) = std::io::pipe().unwrap();
    drop(reader);
    let output = Command::new(env!("CARGO_BIN_EXE_origami"))
        .arg("--version")
        .stdout(writer)
        .output()
        .unwrap();
    assert_eq!(output.status.signal(), Some(libc::SIGPIPE));
    assert!(
        output.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
