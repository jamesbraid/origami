use std::fs;
use std::process::Command;

#[test]
fn missing_machine_directory_names_the_path_and_create_command() {
    let root = tempfile::tempdir().unwrap();
    let missing = root.path().join("missing machine");
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
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("machine.toml");
    let error = origami::read_machine(root.path()).unwrap_err().to_string();
    assert!(error.contains(path.to_str().unwrap()), "{error}");
    assert!(error.contains("origami create"), "{error}");
    fs::write(&path, "format = [").unwrap();
    let error = origami::read_machine(root.path()).unwrap_err().to_string();
    assert!(error.contains(path.to_str().unwrap()), "{error}");
    assert!(error.contains("invalid machine configuration"), "{error}");
}

#[test]
fn install_configuration_errors_name_the_file_and_initialization_command() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("install/media.toml");
    let error = origami::install::read_media(root.path())
        .unwrap_err()
        .to_string();
    assert!(error.contains(path.to_str().unwrap()), "{error}");
    assert!(error.contains("origami install-init"), "{error}");
    fs::create_dir(root.path().join("install")).unwrap();
    fs::write(&path, "format = [").unwrap();
    let error = origami::install::read_media(root.path())
        .unwrap_err()
        .to_string();
    assert!(error.contains(path.to_str().unwrap()), "{error}");
    assert!(error.contains("invalid install configuration"), "{error}");
}
