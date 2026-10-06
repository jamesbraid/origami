//! Stand-ins for the bundled QEMU tools, so command tests run without a
//! product build.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Write a QEMU that answers `query-sgi-machines` from the test catalogue
/// and return the directory to select with `ORIGAMI_RUNTIME_DIR`.
fn fake_runtime(root: &Path) -> PathBuf {
    let runtime = root.join("runtime");
    if runtime.join("qemu-system-mips64").exists() {
        return runtime;
    }
    fs::create_dir_all(&runtime).unwrap();
    let catalogue = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sgi-machines.json");
    script(
        &runtime.join("qemu-system-mips64"),
        &format!(
            r#"case " $* " in *" -qmp stdio "*) ;; *) exit 9 ;; esac
printf '{{"QMP": {{"version": {{}}, "capabilities": []}}}}\n'
while IFS= read -r line; do
    case "$line" in
    *qmp_capabilities*) printf '{{"return": {{}}}}\n' ;;
    *query-sgi-machines*) printf '{{"return": '; tr -d '\n' < '{}'; printf '}}\n' ;;
    *quit*) printf '{{"return": {{}}}}\n'; exit 0 ;;
    esac
done"#,
            catalogue.display()
        ),
    );
    runtime
}

/// Write an executable script without this process ever holding it open
/// for writing. A child forked meanwhile by another test thread would
/// inherit such a descriptor, and running the script would then fail with
/// "Text file busy" until that child calls exec.
fn script(path: &Path, body: &str) {
    let mut writer = Command::new("sh")
        .args(["-c", "cat > \"$1\" && chmod 755 \"$1\"", "sh"])
        .arg(path)
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    writeln!(writer.stdin.take().unwrap(), "#!/bin/sh\n{body}").unwrap();
    assert!(writer.wait().unwrap().success());
}

/// An `origami` command that uses the fake runtime and keeps its catalogue
/// cache inside the test directory.
pub fn origami(executable: &Path, root: &Path) -> Command {
    let mut command = Command::new(executable);
    command
        .env("ORIGAMI_RUNTIME_DIR", fake_runtime(root))
        .env("XDG_CACHE_HOME", root.join("cache"));
    command
}
