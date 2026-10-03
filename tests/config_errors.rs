use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "origami-config-errors-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn missing_machine_directory_names_the_path_and_create_command() {
    let root = TempDir::new();
    let missing = root.0.join("missing machine");
    let output = Command::new(env!("CARGO_BIN_EXE_origami"))
        .arg("validate")
        .arg(&missing)
        .output()
        .unwrap();
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains(missing.to_str().unwrap()), "{error}");
    assert!(error.contains("origami create"), "{error}");
}

#[test]
fn machine_configuration_errors_name_the_file() {
    let root = TempDir::new();
    let path = root.0.join("machine.toml");
    let error = origami::read_machine(&root.0).unwrap_err().to_string();
    assert!(error.contains(path.to_str().unwrap()), "{error}");
    assert!(error.contains("origami create"), "{error}");
    fs::write(&path, "format = [").unwrap();
    let error = origami::read_machine(&root.0).unwrap_err().to_string();
    assert!(error.contains(path.to_str().unwrap()), "{error}");
    assert!(error.contains("invalid machine configuration"), "{error}");
}

#[test]
fn install_configuration_errors_name_the_file_and_initialization_command() {
    let root = TempDir::new();
    let path = root.0.join("install/media.toml");
    let error = origami::install::read_media(&root.0)
        .unwrap_err()
        .to_string();
    assert!(error.contains(path.to_str().unwrap()), "{error}");
    assert!(error.contains("origami install-init"), "{error}");
    fs::create_dir(root.0.join("install")).unwrap();
    fs::write(&path, "format = [").unwrap();
    let error = origami::install::read_media(&root.0)
        .unwrap_err()
        .to_string();
    assert!(error.contains(path.to_str().unwrap()), "{error}");
    assert!(error.contains("invalid install configuration"), "{error}");
}
