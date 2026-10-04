use origami::{catalogue, import_configured, preset, read_machine, runtime, validate};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static DIRECTORY_COUNTER: AtomicU64 = AtomicU64::new(0);

struct MachineDir(PathBuf);
impl MachineDir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "origami-firmware-{}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            DIRECTORY_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for MachineDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn prepared_import_preserves_each_node_and_rejects_partial_inputs_atomically() {
    let root = MachineDir::new();
    let catalog = catalogue().unwrap();
    let offering = preset(&catalog, "origin2000-8").unwrap();
    let layout = origami::firmware::Layout::find(&offering.topology, "cpu").unwrap();
    let inputs: Vec<_> = (0..offering.nodes)
        .map(|node| {
            let path = root.0.join(format!("source{node}.raw"));
            let mut bytes = vec![0xff; layout.size()];
            bytes[0] = 17 + node as u8;
            bytes[layout.size() - 1] = 23 + node as u8;
            fs::write(&path, bytes).unwrap();
            path
        })
        .collect();
    let machine = root.0.join("imported");
    import_configured(
        &machine,
        offering,
        &inputs,
        &[],
        None,
        None,
        Some("none"),
        Default::default(),
    )
    .unwrap();
    for (source, backend) in inputs
        .iter()
        .zip(origami::firmware::cpu_paths(&machine, offering))
    {
        assert_eq!(fs::read(source).unwrap(), fs::read(&backend).unwrap());
    }
    let file = read_machine(&machine).unwrap();
    validate(&catalog, &machine, &file).unwrap();
    let rejected = root.0.join("rejected");
    fs::write(&inputs[3], b"partial").unwrap();
    let before: Vec<_> = inputs.iter().map(|path| fs::read(path).unwrap()).collect();
    assert!(import_configured(
        &rejected,
        offering,
        &inputs,
        &[],
        None,
        None,
        Some("none"),
        Default::default()
    )
    .is_err());
    assert!(!rejected.exists());
    for (path, bytes) in inputs.iter().zip(before) {
        assert_eq!(fs::read(path).unwrap(), bytes);
    }
}

#[test]
fn invalid_io_import_leaves_destination_absent_and_sources_unchanged() {
    let root = MachineDir::new();
    let catalog = catalogue().unwrap();
    let offering = preset(&catalog, "origin200-impact").unwrap();
    let cpu = root.0.join("cpu.raw");
    let cpu_bytes = vec![
        0xa5;
        origami::firmware::Layout::find(&offering.topology, "cpu")
            .unwrap()
            .size()
    ];
    fs::write(&cpu, &cpu_bytes).unwrap();
    let io = root.0.join("io.raw");
    fs::write(&io, b"partial IO").unwrap();
    let machine = root.0.join("rejected");
    assert!(import_configured(
        &machine,
        offering,
        &[cpu.clone()],
        &[io.clone()],
        None,
        None,
        Some("none"),
        Default::default()
    )
    .is_err());
    assert!(!machine.exists());
    assert_eq!(fs::read(cpu).unwrap(), cpu_bytes);
    assert_eq!(fs::read(io).unwrap(), b"partial IO");
}

#[test]
fn supplied_identity_override_is_read_only_and_required_on_reopen() {
    let root = MachineDir::new();
    let catalog = catalogue().unwrap();
    let offering = preset(&catalog, "origin300-2").unwrap();
    let cpu = root.0.join("cpu.raw");
    fs::write(
        &cpu,
        vec![
            0xff;
            origami::firmware::Layout::find(&offering.topology, "cpu")
                .unwrap()
                .size()
        ],
    )
    .unwrap();
    let spd2 = root.0.join("spd2.bin");
    let spd3 = root.0.join("spd3.bin");
    fs::write(&spd2, [0x12; 128]).unwrap();
    fs::write(&spd3, [0x34; 128]).unwrap();
    let machine = root.0.join("imported");
    import_configured(
        &machine,
        offering,
        &[cpu],
        &[],
        None,
        Some(origami::Origin300Create {
            spd_dimm2: Some(&spd2),
            spd_dimm3: Some(&spd3),
            mac: "08:00:69:12:34:56",
        }),
        Some("none"),
        Default::default(),
    )
    .unwrap();
    let override_path = root.0.join("supplied,board.bin");
    fs::write(&override_path, b"supplied raw identity").unwrap();
    let mut file = read_machine(&machine).unwrap();
    file.identity.as_mut().unwrap().board_eeprom = Some(override_path.display().to_string());
    validate(&catalog, &machine, &file).unwrap();
    let args = runtime::arguments(&machine, &file, offering, runtime::Display::None).unwrap();
    assert!(args.iter().any(|arg| arg.contains(&format!(
        "board-eeprom.1={}",
        origami::qemu_path_option(&override_path)
    ))));
    runtime::prepare_state(&machine, &file, offering).unwrap();
    assert_eq!(fs::read(&override_path).unwrap(), b"supplied raw identity");
    fs::remove_file(&override_path).unwrap();
    assert!(validate(&catalog, &machine, &file)
        .unwrap_err()
        .to_string()
        .contains("supplied,board.bin"));
}

#[test]
fn originals_are_not_runtime_dependencies_and_imports_are_independent() {
    let root = MachineDir::new();
    let catalog = catalogue().unwrap();
    let offering = preset(&catalog, "origin200-1").unwrap();
    let source = root.0.join("original.bin");
    fs::write(&source, [0x42; 1024]).unwrap();
    let machine = root.0.join("created");
    origami::create(&machine, offering, &source, None, None).unwrap();
    let file = read_machine(&machine).unwrap();
    let flash = origami::firmware::cpu_paths(&machine, offering)[0].clone();
    let before = fs::read(&flash).unwrap();
    fs::remove_dir_all(machine.join("firmware")).unwrap();
    validate(&catalog, &machine, &file).unwrap();
    runtime::prepare_state(&machine, &file, offering).unwrap();
    assert_eq!(fs::read(&flash).unwrap(), before);
    assert!(!machine.join("state/firmware-initialization").exists());
    let args = runtime::arguments(&machine, &file, offering, runtime::Display::None).unwrap();
    assert!(!args
        .iter()
        .any(|arg| arg == "-bios" || arg == "-S" || arg.contains("firmware-initialize")));
}

#[test]
fn origin300_mac_alone_uses_qemu_spd_defaults() {
    use std::process::Command;
    let root = MachineDir::new();
    let original = root.0.join("original.bin");
    let catalog = catalogue().unwrap();
    let offering = preset(&catalog, "origin300-2").unwrap();
    fs::write(&original, vec![0x42; offering.firmware.size as usize]).unwrap();
    let machine = root.0.join("machine");
    let output = Command::new(env!("CARGO_BIN_EXE_origami"))
        .args([
            "create",
            machine.to_str().unwrap(),
            "--preset",
            "origin300-2",
            "--prom",
            original.to_str().unwrap(),
            "--mac",
            "02:00:5d:11:22:33",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let file = read_machine(&machine).unwrap();
    let identity = file.identity.as_ref().unwrap();
    assert_eq!(identity.mac, "02:00:5d:11:22:33");
    assert!(identity.spd_dimm2.is_empty() && identity.spd_dimm3.is_empty());
    assert!(!machine.join("firmware/spd-dimm2.bin").exists());
    assert!(!machine.join("firmware/spd-dimm3.bin").exists());
    let offering = validate(&catalog, &machine, &file).unwrap();
    let args = runtime::arguments(&machine, &file, offering, runtime::Display::None).unwrap();
    assert!(args
        .iter()
        .any(|arg| arg.contains("io8-mac=02:00:5d:11:22:33")));
    assert!(!args
        .iter()
        .any(|arg| arg.contains("spd-dimm") || arg.contains("spd-eeprom")));
}

#[cfg(unix)]
#[test]
fn first_run_launches_one_process_without_initialization_control() {
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;
    let root = MachineDir::new();
    let source = root.0.join("original.bin");
    fs::write(&source, [0x42; 1024]).unwrap();
    let machine = root.0.join("machine");
    let catalog = catalogue().unwrap();
    origami::create(
        &machine,
        preset(&catalog, "origin200-1").unwrap(),
        &source,
        None,
        None,
    )
    .unwrap();
    let runtime_dir = root.0.join("runtime/libexec/sgi");
    fs::create_dir_all(&runtime_dir).unwrap();
    let keymaps = root.0.join("runtime/share/sgi/qemu/keymaps");
    fs::create_dir_all(&keymaps).unwrap();
    fs::write(keymaps.join("en-us"), b"fixture").unwrap();
    let qemu = runtime_dir.join("qemu-system-mips64");
    fs::write(&qemu, b"#!/bin/sh\nprintf '%s\n' launch >> \"$ORIGAMI_TEST_LAUNCHES\"\nfor arg in \"$@\"; do case \"$arg\" in -S|*firmware-initialize*) exit 71;; esac; done\nexit 0\n").unwrap();
    fs::set_permissions(&qemu, fs::Permissions::from_mode(0o700)).unwrap();
    let launches = root.0.join("launches.txt");
    let output = Command::new(env!("CARGO_BIN_EXE_origami"))
        .args(["run", machine.to_str().unwrap(), "--display", "none"])
        .env("SGI_RUNTIME_DIR", &runtime_dir)
        .env("ORIGAMI_TEST_LAUNCHES", &launches)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fs::read_to_string(launches).unwrap(), "launch\n");
}

#[test]
fn fuel_console_selection_persists_without_changing_backend_bytes() {
    use fs2::FileExt;
    use std::process::Command;
    let root = MachineDir::new();
    let catalog = catalogue().unwrap();
    let offering = preset(&catalog, "fuel-1").unwrap();
    let input = root.0.join("prepared.raw");
    let bytes = vec![
        0xa5;
        origami::firmware::Layout::find(&offering.topology, "cpu")
            .unwrap()
            .size()
    ];
    fs::write(&input, &bytes).unwrap();
    let machine = root.0.join("fuel");
    let inputs = [
        ("fuel-board-id-word", "0x4001"),
        ("fuel-bedrock-revision", "2"),
        ("fuel-ioc3-subsystem-id", "0xc30a"),
        ("fuel-l1-type-code", "0x34"),
    ]
    .into_iter()
    .map(|(key, value)| (key.into(), value.into()))
    .collect();
    import_configured(
        &machine,
        offering,
        &[input],
        &[],
        None,
        None,
        Some("none"),
        inputs,
    )
    .unwrap();
    let set = |port: &str| {
        Command::new(env!("CARGO_BIN_EXE_origami"))
            .arg("console-set")
            .arg(&machine)
            .args(["--port", port])
            .output()
            .unwrap()
    };
    let output = set("ioc3-a");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let file = read_machine(&machine).unwrap();
    assert_eq!(file.machine.console.as_deref(), Some("ioc3-a"));
    let args = runtime::arguments(&machine, &file, offering, runtime::Display::None).unwrap();
    let serial: Vec<_> = args
        .windows(2)
        .filter(|pair| pair[0] == "-serial")
        .map(|pair| pair[1].as_str())
        .collect();
    assert_eq!(serial, ["null", "stdio"]);
    let configuration = fs::read(machine.join("machine.toml")).unwrap();
    assert!(!set("unknown").status.success());
    assert_eq!(
        fs::read(machine.join("machine.toml")).unwrap(),
        configuration
    );
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .open(machine.join("state/machine.lock"))
        .unwrap();
    lock.try_lock_exclusive().unwrap();
    assert!(!set("l1").status.success());
    assert_eq!(
        fs::read(machine.join("machine.toml")).unwrap(),
        configuration
    );
    FileExt::unlock(&lock).unwrap();
    assert!(set("l1").status.success());
    let file = read_machine(&machine).unwrap();
    let args = runtime::arguments(&machine, &file, offering, runtime::Display::None).unwrap();
    let serial: Vec<_> = args
        .windows(2)
        .filter(|pair| pair[0] == "-serial")
        .map(|pair| pair[1].as_str())
        .collect();
    assert_eq!(serial, ["stdio"]);
    assert_eq!(
        fs::read(origami::firmware::cpu_paths(&machine, offering)[0].clone()).unwrap(),
        bytes
    );
}
