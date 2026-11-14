//! Machine emu-id numbers: `create` mints and records them, derives the MAC
//! from them, and a launch passes them on. Runs against the fake runtime,
//! whose machine list offers `emu-id` on the Origin 200 and Octane only.

use crate::support;
use std::fs;
use std::path::Path;

/// Run origami with the fake runtime; Ok(stdout) or Err(stderr).
fn origami(root: &Path, args: &[&str]) -> Result<String, String> {
    let output = support::origami(root).args(args).output().unwrap();
    let text = |bytes: &[u8]| String::from_utf8_lossy(bytes).into_owned();
    if output.status.success() {
        // Warnings go to stderr, so keep both for the tests that look.
        Ok(text(&output.stdout) + &text(&output.stderr))
    } else {
        Err(text(&output.stderr))
    }
}

fn create(root: &Path, name: &str, preset: &str, extra: &[&str]) -> Result<String, String> {
    let prom = root.join("synthetic-prom.bin");
    fs::write(&prom, vec![0; 1024 * 1024]).unwrap();
    let dir = root.join(name);
    let mut args = vec![
        "create",
        dir.to_str().unwrap(),
        "--preset",
        preset,
        "--prom",
        prom.to_str().unwrap(),
        "--io-prom",
        prom.to_str().unwrap(),
    ];
    args.extend(extra);
    origami(root, &args)
}

fn identity(root: &Path, name: &str) -> (String, Option<u32>) {
    let file = origami::read_machine(&root.join(name)).unwrap();
    let identity = file.identity.unwrap();
    (identity.mac, identity.emu_id)
}

#[test]
fn create_takes_the_next_free_emu_id_and_derives_the_mac() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    create(root, "a", "origin200-1", &[]).unwrap();
    create(root, "b", "origin200-1", &[]).unwrap();
    assert_eq!(identity(root, "a"), ("08:00:69:e1:00:10".into(), Some(1)));
    assert_eq!(identity(root, "b"), ("08:00:69:e1:00:20".into(), Some(2)));
    let show = origami(root, &["show", root.join("b").to_str().unwrap()]).unwrap();
    assert!(show.contains("emu-id: 2\n"), "{show}");
}

#[test]
fn an_explicit_emu_id_is_checked_and_a_taken_one_is_skipped() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    create(root, "a", "origin200-1", &["--emu-id", "42"]).unwrap();
    assert_eq!(identity(root, "a"), ("08:00:69:e1:02:a0".into(), Some(42)));

    let error = create(root, "b", "origin200-1", &["--emu-id", "42"]).unwrap_err();
    assert!(error.contains("emu-id 42 is already"), "{error}");
    assert!(error.contains("/a"), "{error}");
    assert!(!root.join("b").exists());
    for reserved in ["3000", "4095"] {
        let error = create(root, "b", "origin200-1", &["--emu-id", reserved]).unwrap_err();
        assert!(
            error.contains("reserved for harness and CI machines"),
            "{reserved}: {error}"
        );
    }
    for bad in ["0", "4096", "x"] {
        let error = create(root, "b", "origin200-1", &["--emu-id", bad]).unwrap_err();
        assert!(error.contains("--emu-id"), "{bad}: {error}");
    }

    // The auto-minted ones fill from 1, stepping over what siblings hold.
    create(root, "c", "origin200-1", &["--emu-id", "2"]).unwrap();
    create(root, "d", "origin200-1", &[]).unwrap();
    create(root, "e", "origin200-1", &[]).unwrap();
    assert_eq!(identity(root, "d").1, Some(1));
    assert_eq!(identity(root, "e").1, Some(3));

    create(root, "f", "origin200-1", &["--emu-id", "2999"]).unwrap();
    assert_eq!(
        identity(root, "f"),
        ("08:00:69:e1:bb:70".into(), Some(2999))
    );
}

#[test]
fn an_explicit_mac_wins_over_the_emu_id() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    create(root, "a", "origin200-1", &["--mac", "08:00:69:12:34:56"]).unwrap();
    assert_eq!(identity(root, "a"), ("08:00:69:12:34:56".into(), Some(1)));
}

#[test]
fn a_launch_passes_the_emu_id_and_the_derived_mac_on_every_machine() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    for (name, preset, emu_id, mac) in [
        ("o200", "origin200-1", "42", "08:00:69:e1:02:a0"),
        ("o2k", "origin2000-8", "43", "08:00:69:e1:02:b0"),
        ("o3k", "origin3200", "44", "08:00:69:e1:02:c0"),
    ] {
        create(root, name, preset, &["--emu-id", emu_id]).unwrap();
        assert_eq!(identity(root, name).1, Some(emu_id.parse().unwrap()));
        let command = origami(root, &["show-command", root.join(name).to_str().unwrap()]).unwrap();
        assert!(command.contains(&format!(",emu-id={emu_id}")), "{command}");
        assert!(command.contains(&format!(",mac={mac}")), "{command}");
    }
}

#[test]
fn emu_id_cannot_be_set_as_a_hardware_input() {
    let root = tempfile::tempdir().unwrap();
    let error = create(root.path(), "a", "origin200-1", &["--set", "emu-id=5"]).unwrap_err();
    assert!(error.contains("--emu-id"), "{error}");
}

#[test]
fn a_copied_machine_keeps_its_emu_id_and_validate_warns() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    create(root, "a", "origin200-1", &["--emu-id", "42"]).unwrap();
    let clone = root.join("clone");
    support::copy_dir(&root.join("a"), &clone);
    assert_eq!(identity(root, "clone"), identity(root, "a"));
    let output = origami(root, &["validate", clone.to_str().unwrap()]).unwrap();
    assert!(
        output.contains("warning: this machine's emu-id 42 is also the emu-id of"),
        "{output}"
    );
    // A new machine never inherits it.
    create(root, "fresh", "origin200-1", &[]).unwrap();
    assert_eq!(identity(root, "fresh").1, Some(1));
}

#[test]
fn upgrade_gives_an_old_machine_a_emu_id_and_keeps_its_mac() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    create(root, "first", "origin200-1", &["--emu-id", "1"]).unwrap();
    create(root, "old", "origin200-1", &["--mac", "08:00:69:12:34:56"]).unwrap();
    let dir = root.join("old");
    // A machine made before emu-ids.
    let path = dir.join("machine.toml");
    let text = fs::read_to_string(&path).unwrap();
    let text: String = text
        .lines()
        .filter(|line| !line.starts_with("emu-id ="))
        .map(|line| format!("{line}\n"))
        .collect();
    fs::write(&path, text).unwrap();
    assert_eq!(identity(root, "old"), ("08:00:69:12:34:56".into(), None));

    let output = origami(root, &["upgrade", dir.to_str().unwrap()]).unwrap();
    assert!(output.contains("note: emu-id 2 added"), "{output}");
    assert!(
        output.contains("host id") && output.contains("unmarked"),
        "{output}"
    );
    assert_eq!(identity(root, "old"), ("08:00:69:12:34:56".into(), Some(2)));

    let output = origami(root, &["upgrade", dir.to_str().unwrap()]).unwrap();
    assert!(output.contains("is current"), "{output}");
    assert_eq!(identity(root, "old").1, Some(2));
}

#[test]
fn network_set_gives_a_machine_without_an_identity_a_emu_id_and_its_mac() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    create(root, "first", "origin200-1", &["--emu-id", "1"]).unwrap();
    for (name, extra, mac, emu_id) in [
        ("derived", vec![], "08:00:69:e1:00:20", 2),
        (
            "explicit",
            vec!["--mac", "08:00:69:12:34:56"],
            "08:00:69:12:34:56",
            3,
        ),
    ] {
        create(root, name, "origin200-1", &[]).unwrap();
        let dir = root.join(name);
        // A machine made without an identity.
        let path = dir.join("machine.toml");
        let text = fs::read_to_string(&path).unwrap();
        let (_, rest) = text.split_once("[identity]\n").unwrap();
        let rest = rest
            .lines()
            .skip_while(|l| !l.is_empty())
            .collect::<Vec<_>>();
        let head = text.split_once("[identity]").unwrap().0;
        fs::write(&path, format!("{head}{}\n", rest.join("\n"))).unwrap();
        assert!(origami::read_machine(&dir).unwrap().identity.is_none());

        let mut args = vec!["network-set", dir.to_str().unwrap(), "--mode", "private"];
        args.extend(extra);
        origami(root, &args).unwrap();
        assert_eq!(identity(root, name), (mac.into(), Some(emu_id)), "{name}");
    }
}

#[test]
fn concurrent_creates_never_share_a_emu_id() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    // The fake runtime and PROM are made once, as a real session has them.
    support::fake_runtime(root);
    let handles: Vec<_> = (0..8)
        .map(|index| {
            let root = root.to_path_buf();
            std::thread::spawn(move || {
                create(&root, &format!("m{index}"), "origin200-1", &[]).unwrap();
            })
        })
        .collect();
    for handle in handles {
        handle.join().unwrap();
    }
    let mut emu_ids: Vec<_> = (0..8)
        .map(|index| identity(root, &format!("m{index}")).1.unwrap())
        .collect();
    emu_ids.sort();
    assert_eq!(emu_ids, (1..=8).collect::<Vec<_>>());
}

#[test]
fn run_warns_about_a_clone_while_its_twin_is_running_and_still_starts_it() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    create(root, "twin", "origin200-1", &["--emu-id", "42"]).unwrap();
    let clone = root.join("clone");
    support::copy_dir(&root.join("twin"), &clone);
    let run = |extra: &[&str]| {
        // The fake QEMU exits at once, so a start that gets as far as the
        // runner fails; either way the output holds the warnings.
        let mut args = vec!["run", clone.to_str().unwrap()];
        args.extend(extra);
        let output = support::origami(root)
            .env("ORIGAMI_FAKE", "exit-early")
            .args(&args)
            .output()
            .unwrap();
        String::from_utf8_lossy(&output.stdout).into_owned()
            + &String::from_utf8_lossy(&output.stderr)
    };
    let warning = "warning: emu-id 42 is also the emu-id of the running machine";
    assert!(
        !run(&["--background"]).contains(warning),
        "no twin runs yet"
    );
    // A running machine holds its edit lock.
    let _running = origami::control::lock_for_edit(&root.join("twin"), "test").unwrap();
    let foreground = run(&[]);
    assert!(foreground.contains(warning), "{foreground}");
    assert!(foreground.contains("twin") && foreground.contains("clash"));
    assert!(!foreground.contains("Stop it first"), "{foreground}");
    let background = run(&["--background"]);
    assert!(background.contains(warning), "{background}");
    // The runner process warns into its own log, so the guest was started
    // and not refused; the log holds only what it wrote.
    let log = fs::read_to_string(clone.join("logs/runner.log")).unwrap();
    assert!(log.contains(warning), "{log}");
}

#[test]
fn upgrade_says_when_the_host_id_changes() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    create(root, "old", "origin3200", &[]).unwrap();
    let dir = root.join("old");
    let path = dir.join("machine.toml");
    let text: String = fs::read_to_string(&path)
        .unwrap()
        .lines()
        .filter(|line| !line.starts_with("emu-id ="))
        .map(|line| format!("{line}\n"))
        .collect();
    fs::write(&path, text).unwrap();
    let output = origami(root, &["upgrade", dir.to_str().unwrap()]).unwrap();
    assert!(output.contains("new host id at its next boot"), "{output}");
    assert!(output.contains("emu-id = 0"), "{output}");
    assert!(!output.contains("unmarked"), "{output}");
}
