//! The core operations against the QEMU adapter over the fake runtime: what
//! the command line does through them, done by calling them.

#[path = "../support/mod.rs"]
mod support;

use origami_core::{
    apply_changes, apply_first_boot, create, nvram_read, Change, DriveKind, DriveRequest,
    MachineSpec,
};
use origami_qemu::{QemuEngine, Runtime};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// One runtime directory for the file; each test works in its own folder.
fn root() -> &'static Path {
    static ROOT: OnceLock<tempfile::TempDir> = OnceLock::new();
    let root = ROOT.get_or_init(|| {
        let root = tempfile::tempdir().unwrap();
        std::env::set_var("ORIGAMI_RUNTIME_DIR", support::fake_runtime(root.path()));
        std::env::set_var("ORIGAMI_CACHE_DIR", root.path().join("cache"));
        root
    });
    root.path()
}

fn engine() -> QemuEngine {
    QemuEngine::new(Runtime::locate().unwrap())
}

fn snapshot(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn walk(dir: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.file_name().is_some_and(|name| name != "machine.lock") {
                // Windows cannot read the lock file a held lock covers.
                out.insert(path.clone(), fs::read(&path).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, &mut out);
    out
}

/// A file's contents by its path below `dir`.
fn relative(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    snapshot(dir)
        .into_iter()
        .map(|(path, bytes)| (path.strip_prefix(dir).unwrap().to_path_buf(), bytes))
        .collect()
}

#[test]
fn create_from_a_spec_equals_the_command_line() {
    let root = root();
    let prom = support::synthetic_prom(root);
    for (preset, emu_id) in [("octane2-vpro", 5), ("origin200-1", 9)] {
        let cli_dir = root.join("cli").join(preset);
        let api_dir = root.join("api").join(preset);
        let id = emu_id.to_string();
        let said = support::ok(
            root,
            &[
                "create",
                cli_dir.to_str().unwrap(),
                "--preset",
                preset,
                "--prom",
                prom.to_str().unwrap(),
                "--emu-id",
                &id,
            ],
        );

        let mut spec = MachineSpec::new(&api_dir, preset);
        spec.firmware.boot_prom = Some(prom.clone());
        spec.identity.emu_id = Some(emu_id);
        let report = create(&engine(), &spec).unwrap();

        assert_eq!(relative(&api_dir), relative(&cli_dir), "{preset}");
        let printed: String = report.rendered().map(|(_, line)| line + "\n").collect();
        assert_eq!(
            printed.replace(api_dir.to_str().unwrap(), "DIR"),
            said.replace(cli_dir.to_str().unwrap(), "DIR")
        );
    }
}

#[test]
fn a_failing_step_leaves_machine_toml_and_the_stores_untouched() {
    let root = root();
    let prom = support::synthetic_prom(root);
    let dir = root.join("atomic").join("origin200");
    let mut spec = MachineSpec::new(&dir, "origin200-1");
    spec.firmware.boot_prom = Some(prom);
    create(&engine(), &spec).unwrap();
    let dir = fs::canonicalize(&dir).unwrap();
    let image = root.join("atomic").join("extra.img");
    fs::write(&image, vec![0; 512]).unwrap();
    let attach = |target| {
        Change::AttachDrive(DriveRequest {
            file: image.clone(),
            kind: DriveKind::Disk,
            target,
            overlay: false,
        })
    };
    let nvram = Change::SetNvram(vec![("console".into(), "d".into())]);

    // The drive is fine; a machine that has not booted has no NVRAM to set.
    let before = snapshot(&dir);
    let engine = engine();
    let error = apply_changes(&engine, &dir, &[attach(2), nvram]).unwrap_err();
    assert!(error.to_string().contains("boot once"), "{error}");
    assert_eq!(snapshot(&dir), before);

    // A step that fails after another changed the file leaves it as it was.
    let error = apply_changes(&engine, &dir, &[attach(2), attach(99)]).unwrap_err();
    assert!(error.to_string().contains("99"), "{error}");
    assert_eq!(snapshot(&dir), before);

    apply_changes(&engine, &dir, &[attach(2)]).unwrap();
    assert_ne!(snapshot(&dir), before);
}

/// An Octane PROM's NVRAM after its first boot: console `g`.
const OCTANE_NVRAM: &[u8] = include_bytes!("../fixtures/nvram/octane-ip30prom-4.17.raw");

fn console(dir: &Path) -> String {
    let report = nvram_read(&engine(), dir, Some("console")).unwrap();
    report.rendered().map(|(_, line)| line).collect()
}

#[test]
fn a_machine_without_graphics_takes_the_serial_console_once_its_prom_has_run() {
    let root = root();
    let prom = support::synthetic_prom(root);
    let dir = root.join("headless").join("octane2");
    let mut spec = MachineSpec::new(&dir, "octane2-vpro");
    spec.firmware.boot_prom = Some(prom);
    spec.graphics.product = Some("none".into());
    create(&engine(), &spec).unwrap();
    let dir = fs::canonicalize(&dir).unwrap();

    // The new NVRAM is blank, so the setting waits in machine.toml.
    let text = fs::read_to_string(dir.join("machine.toml")).unwrap();
    assert!(
        text.contains("[firstboot.nvram]\nconsole = \"d\"\n"),
        "{text}"
    );
    assert!(apply_first_boot(&engine(), &dir)
        .unwrap()
        .rendered()
        .next()
        .is_none());
    assert_eq!(fs::read_to_string(dir.join("machine.toml")).unwrap(), text);

    // The PROM writes its defaults, which put the console on the graphics head.
    fs::write(dir.join("state/nvram0.raw"), OCTANE_NVRAM).unwrap();
    assert_eq!(console(&dir), "console=g");
    let report = apply_first_boot(&engine(), &dir).unwrap();
    assert_eq!(report.rendered().next().unwrap().1, "console=d");
    assert_eq!(console(&dir), "console=d");
    let text = fs::read_to_string(dir.join("machine.toml")).unwrap();
    assert!(!text.contains("firstboot"), "{text}");
    assert!(apply_first_boot(&engine(), &dir)
        .unwrap()
        .rendered()
        .next()
        .is_none());
}

#[test]
fn a_machine_with_graphics_keeps_the_proms_console() {
    let root = root();
    let prom = support::synthetic_prom(root);
    let dir = root.join("graphical").join("octane2");
    let mut spec = MachineSpec::new(&dir, "octane2-vpro");
    spec.firmware.boot_prom = Some(prom);
    create(&engine(), &spec).unwrap();
    let text = fs::read_to_string(dir.join("machine.toml")).unwrap();
    assert!(!text.contains("firstboot"), "{text}");
}

fn origin200(name: &str) -> PathBuf {
    let root = root();
    let dir = root.join(name).join("origin200");
    let mut spec = MachineSpec::new(&dir, "origin200-1");
    spec.firmware.boot_prom = Some(support::synthetic_prom(root));
    create(&engine(), &spec).unwrap();
    fs::canonicalize(&dir).unwrap()
}

fn settings(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

#[test]
fn processor_memory_and_settings_change_after_creation() {
    let dir = origin200("edit");
    let changes = [
        Change::SetProcessor {
            module: Some("r10000-180mhz-1mb".into()),
            cpus_per_node: Some(2),
        },
        Change::SetMemory(192),
        Change::SetSettings(settings(&[("prid", "0xe24")])),
    ];
    apply_changes(&engine(), &dir, &changes).unwrap();
    let text = fs::read_to_string(dir.join("machine.toml")).unwrap();
    assert!(text.contains("cpus_per_node = 2"), "{text}");
    assert!(text.contains("memory_per_node = \"192MiB\""), "{text}");
    assert!(text.contains("prid = \"0xe24\""), "{text}");
    // Repeating them changes nothing, and a later setting keeps the earlier.
    let before = snapshot(&dir);
    apply_changes(&engine(), &dir, &changes).unwrap();
    assert_eq!(snapshot(&dir), before);
    apply_changes(
        &engine(),
        &dir,
        &[Change::SetSettings(settings(&[("junk-uart", "on")]))],
    )
    .unwrap();
    let text = fs::read_to_string(dir.join("machine.toml")).unwrap();
    assert!(text.contains("prid = \"0xe24\""), "{text}");
    assert!(text.contains("junk-uart = \"on\""), "{text}");
}

#[test]
fn an_invalid_edit_refuses_the_whole_batch_and_changes_nothing() {
    let dir = origin200("refuse");
    let before = snapshot(&dir);
    for bad in [
        Change::SetMemory(7),
        Change::SetSettings(settings(&[("no-such-setting", "1")])),
        Change::SetProcessor {
            module: None,
            cpus_per_node: Some(3),
        },
        // The module is recorded in the node flash at creation.
        Change::SetProcessor {
            module: Some("r12000-270mhz-4mb".into()),
            cpus_per_node: None,
        },
    ] {
        let error =
            apply_changes(&engine(), &dir, &[Change::SetMemory(192), bad.clone()]).unwrap_err();
        assert!(!error.to_string().is_empty(), "{bad:?}");
        assert_eq!(snapshot(&dir), before, "{bad:?}");
    }
}

#[test]
fn a_running_machine_refuses_edits_to_its_hardware() {
    let dir = origin200("running");
    let _running = origami_machine::control::lock_for_edit(&dir, "running").unwrap();
    let before = snapshot(&dir);
    for change in [
        Change::SetMemory(192),
        Change::SetProcessor {
            module: None,
            cpus_per_node: Some(2),
        },
        Change::SetSettings(settings(&[("prid", "0xe24")])),
    ] {
        let error = apply_changes(&engine(), &dir, &[change]).unwrap_err();
        assert!(error.to_string().contains("stop the machine"), "{error}");
    }
    assert_eq!(snapshot(&dir), before);
}
