use crate::support;
use std::fs;
use std::path::Path;

fn origami(root: &Path, args: &[&str]) -> Result<String, String> {
    let output = support::origami(root).args(args).output().unwrap();
    let text = |bytes: &[u8]| String::from_utf8_lossy(bytes).into_owned();
    if output.status.success() {
        Ok(text(&output.stdout))
    } else {
        Err(text(&output.stderr))
    }
}

#[test]
fn machine_set_changes_cpus_memory_and_settings_together_or_not_at_all() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    let machine = support::create(root, "o200", "origin200-1", &[]);
    let dir = machine.to_str().unwrap();
    let toml = machine.join("machine.toml");
    let before = fs::read_to_string(&toml).unwrap();

    let said = origami(
        root,
        &[
            "machine-set",
            dir,
            "--cpus-per-node",
            "2",
            "--memory-per-node",
            "192",
            "--set",
            "prid=0xe24",
        ],
    )
    .unwrap();
    assert!(said.contains("processor: 2 CPUs per node"), "{said}");
    assert!(said.contains("memory: 192MiB per node"), "{said}");
    assert!(said.contains("prid=0xe24"), "{said}");
    let file = origami_machine::read_machine(&machine).unwrap();
    assert_eq!(file.machine.cpus_per_node, 2);
    assert_eq!(file.machine.memory_per_node, "192MiB");
    assert_eq!(file.machine.inputs["prid"], "0xe24");
    origami(root, &["validate", dir]).unwrap();

    // A bad value anywhere leaves the file as the last good change left it.
    let good = fs::read_to_string(&toml).unwrap();
    assert_ne!(good, before);
    for bad in [
        ["--memory-per-node", "7"].as_slice(),
        &["--cpus-per-node", "3"],
        &["--processor", "r12000-270mhz-4mb"],
        &["--set", "no-such-setting=1"],
    ] {
        let mut args = vec!["machine-set", dir, "--memory-per-node", "256"];
        args.extend(bad);
        origami(root, &args).unwrap_err();
        assert_eq!(fs::read_to_string(&toml).unwrap(), good, "{bad:?}");
    }
    let error = origami(root, &["machine-set", dir]).unwrap_err();
    assert!(error.contains("required"), "{error}");
}
