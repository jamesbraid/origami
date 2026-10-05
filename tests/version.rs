#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

static EXECUTABLES: Mutex<()> = Mutex::new(());

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "origami-version-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(root.join("bin")).unwrap();
        fs::create_dir_all(root.join("libexec/origami")).unwrap();
        fs::copy(env!("CARGO_BIN_EXE_origami"), root.join("bin/origami")).unwrap();
        Self(root)
    }

    fn script(&self, relative: &str, body: &str) {
        let path = self.0.join(relative);
        fs::write(
            &path,
            format!("#!/bin/sh\n[ \"$1\" = --version ] || exit 9\n{body}\n"),
        )
        .unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn report(&self, argument: &str, runtime: Option<&Path>) -> String {
        let mut command = Command::new(self.0.join("bin/origami"));
        command.arg(argument).env_remove("ORIGAMI_RUNTIME_DIR");
        if let Some(runtime) = runtime {
            command.env("ORIGAMI_RUNTIME_DIR", runtime);
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn version_reports_the_selected_binaries_after_relocation() {
    let _guard = EXECUTABLES.lock().unwrap();
    let fixture = Fixture::new();
    fixture.script("bin/instigator", "printf 'instigator version v0.3.2\\n'");
    fixture.script(
        "libexec/origami/qemu-system-mips64",
        "printf 'QEMU emulator version 11.1.50 (v11.0.0-123-g1234567)\\nCopyright ignored\\n'",
    );
    let report = fixture.report("--version", None);
    let lines: Vec<_> = report.lines().collect();
    assert_eq!(lines.len(), 3, "{report}");
    assert_eq!(lines[0], format!("origami {}", embedded_version()));
    assert_eq!(lines[1], "qemu 11.1.50 (v11.0.0-123-g1234567)");
    assert_eq!(lines[2], "instigator v0.3.2");
    assert_eq!(fixture.report("version", None), report);
    fixture.script("bin/instigator", "printf 'instigator version (devel)\\n'");
    assert!(fixture
        .report("--version", None)
        .contains("instigator (devel)"));
    fixture.script(
        "libexec/origami/qemu-system-mips64",
        "printf 'QEMU emulator version 11.2.0 (vendor-build)\\n'",
    );
    let replacement = fixture.report("--version", None);
    assert!(
        replacement.contains("qemu 11.2.0 (vendor-build)"),
        "{replacement}"
    );
    fs::create_dir(fixture.0.join("external")).unwrap();
    fixture.script(
        "external/qemu-system-mips64",
        "printf 'QEMU emulator version 12.0.0\\n'",
    );
    let external = fixture.report("--version", Some(&fixture.0.join("external")));
    assert!(external.contains("qemu 12.0.0"), "{external}");
}

#[test]
fn version_reports_missing_or_uncooperative_binaries_without_hiding_origami() {
    let _guard = EXECUTABLES.lock().unwrap();
    let fixture = Fixture::new();
    let report = fixture.report("--version", None);
    assert_eq!(
        report.lines().next().unwrap(),
        format!("origami {}", embedded_version())
    );
    assert!(report.contains("qemu unavailable"), "{report}");
    assert!(report.contains("instigator unavailable"), "{report}");
    fixture.script(
        "bin/instigator",
        "printf 'Usage: old instigator\\n'; exit 1",
    );
    fixture.script(
        "libexec/origami/qemu-system-mips64",
        "printf 'Usage: not a version\\n'",
    );
    let report = fixture.report("--version", None);
    assert!(report.contains("instigator unavailable"), "{report}");
    assert!(report.contains("qemu unavailable"), "{report}");
    assert!(!report.contains("Usage:"), "{report}");
}

fn embedded_version() -> &'static str {
    match env!("VERGEN_GIT_DESCRIBE") {
        "VERGEN_IDEMPOTENT_OUTPUT" => "unknown",
        version => version,
    }
}
