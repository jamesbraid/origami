#![cfg(unix)]

mod common;

use std::fs;
use std::path::Path;

/// Run origami with the fake runtime; Ok(stdout) or Err(stderr).
fn origami(root: &Path, args: &[&str]) -> Result<String, String> {
    let output = common::origami(Path::new(env!("CARGO_BIN_EXE_origami")), root)
        .args(args)
        .output()
        .unwrap();
    let text = |bytes: &[u8]| String::from_utf8_lossy(bytes).into_owned();
    if output.status.success() {
        Ok(text(&output.stdout))
    } else {
        Err(text(&output.stderr))
    }
}

#[test]
fn create_fixes_the_processor_module_in_machine_toml() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    let prom = root.join("synthetic-prom.bin");
    fs::write(&prom, vec![0; 1024 * 1024]).unwrap();
    let machine = root.join("machine");
    let dir = machine.to_str().unwrap();
    let create = |extra: &[&str]| {
        let mut args = vec!["create", dir, "--preset", "origin200-1"];
        args.extend(["--prom", prom.to_str().unwrap()]);
        args.extend(extra);
        origami(root, &args)
    };

    let machines = origami(root, &["machines"]).unwrap();
    assert!(
        machines
            .contains("r12000-270mhz-4mb    270 MHz R12000, 4 MB secondary cache, 2 CPUs per node"),
        "{machines}"
    );

    let error = create(&["--processor", "r10000-195mhz"]).unwrap_err();
    assert!(
        error.contains("choose r10000-180mhz-1mb, r12000-270mhz-4mb"),
        "{error}"
    );
    // 1792 MiB needs a 256 MB bank, which the 180 MHz module's PROM disables.
    let error = create(&["--memory-per-node", "1792"]).unwrap_err();
    assert!(error.contains("choose 512, 256, 128, 64"), "{error}");
    assert!(!machine.exists());

    // The 270 MHz module holds two CPUs, so the one-CPU preset gets two.
    let output = create(&[
        "--processor",
        "r12000-270mhz-4mb",
        "--memory-per-node",
        "1792",
    ])
    .unwrap();
    assert!(output.contains("each node has 2"), "{output}");
    let file = origami::read_machine(&machine).unwrap();
    assert_eq!(file.machine.processor.as_deref(), Some("r12000-270mhz-4mb"));
    assert_eq!(file.machine.population, [2]);
    let args = fs::read_to_string(root.join("runtime/qemu-sgi-machine-init.args")).unwrap();
    assert!(args.contains(",processor=r12000-270mhz-4mb\n"), "{args}");
    let valid = origami(root, &["validate", dir]).unwrap();
    assert_eq!(valid, "valid: origin200 (2 CPUs, r12000-270mhz-4mb)\n");
    let show = origami(root, &["show", dir]).unwrap();
    assert!(
        show.contains("processor: 270 MHz R12000, 4 MB secondary cache (r12000-270mhz-4mb)"),
        "{show}"
    );

    // A hand edit to a population the module does not come in is refused.
    let toml = machine.join("machine.toml");
    let text = fs::read_to_string(&toml).unwrap();
    fs::write(&toml, text.replace("population = [2]", "population = [1]")).unwrap();
    let error = origami(root, &["validate", dir]).unwrap_err();
    assert!(
        error.contains("does not offer r12000-270mhz-4mb"),
        "{error}"
    );

    // A machine.toml without a processor, as 0.3.0 wrote, has the default.
    let text = text
        .replace("processor = \"r12000-270mhz-4mb\"\n", "")
        .replace("population = [2]", "population = [1]")
        .replace("cpus_per_node = 2", "cpus_per_node = 1")
        .replace("1792MiB", "64MiB");
    fs::write(&toml, text).unwrap();
    let valid = origami(root, &["validate", dir]).unwrap();
    assert_eq!(valid, "valid: origin200 (1 CPUs)\n");
}
