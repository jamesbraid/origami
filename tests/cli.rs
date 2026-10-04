use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "origami-cli-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&path).unwrap();
        fs::write(path.join("prom.bin"), [0; 1024]).unwrap();
        Self(path)
    }

    fn cli(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_origami"));
        command.current_dir(&self.0);
        command
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn each_public_command_has_help_without_opening_a_machine() {
    for name in [
        "machines",
        "create",
        "validate",
        "show",
        "show-command",
        "run",
        "status",
        "console",
        "stop",
        "drive-create",
        "drive-attach",
        "drive-detach",
        "network-set",
        "network-forward-add",
        "network-forward-remove",
        "install-init",
        "install-addon",
        "install-check",
        "install-serve",
        "install-apply",
        "install-finish",
        "version",
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_origami"))
            .args([name, "--help"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{name}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("Usage:"),
            "{name}"
        );
    }
}

#[test]
fn invalid_creation_arguments_do_not_create_a_machine() {
    for extra in [
        vec!["--unknown"],
        vec!["--memory-per-node", "invalid"],
        vec!["unexpected-positional"],
    ] {
        let fixture = Fixture::new();
        let output = fixture
            .cli()
            .args([
                "create",
                "machine",
                "--preset",
                "origin200-1",
                "--prom",
                "prom.bin",
            ])
            .args(extra)
            .output()
            .unwrap();
        assert!(!output.status.success(), "unexpected success");
        assert!(
            !fixture.0.join("machine").exists(),
            "invalid arguments changed the filesystem"
        );
    }
}

#[test]
#[cfg(unix)]
fn creation_rejects_non_utf8_paths() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;
    let fixture = Fixture::new();
    let output = fixture
        .cli()
        .arg("create")
        .arg(OsString::from_vec(b"machine-\xff".to_vec()))
        .args(["--preset", "origin200-1", "--prom", "prom.bin"])
        .output()
        .unwrap();
    assert!(!output.status.success(), "unexpected success");
    assert!(String::from_utf8_lossy(&output.stderr).contains("valid UTF-8"));
}
