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

fn proms(root: &Path) -> (String, String) {
    let prom = root.join("prom.bin");
    fs::write(&prom, vec![0; 1024 * 1024]).unwrap();
    let io_prom = root.join("io-prom.bin");
    fs::write(&io_prom, vec![0; 1024 * 1024]).unwrap();
    (
        prom.to_str().unwrap().into(),
        io_prom.to_str().unwrap().into(),
    )
}

#[test]
fn an_onyx_3400_takes_up_to_four_pipes() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    let (prom, io_prom) = proms(root);
    let machine = root.join("onyx");
    let dir = machine.to_str().unwrap();
    let create = |extra: &[&str]| {
        let mut args = vec![
            "create",
            dir,
            "--preset",
            "onyx3400-infinite-reality",
            "--prom",
            &prom,
            "--io-prom",
            &io_prom,
        ];
        args.extend(extra);
        origami(root, &args)
    };

    let listing = origami(root, &["machines", "onyx3400-infinite-reality"]).unwrap();
    assert!(
        listing.contains("Pipes: 1 to 4 (create --graphics-pipes N)"),
        "{listing}"
    );

    for (count, expected) in [("5", "at most 4"), ("0", "invalid value")] {
        let error = create(&["--graphics-pipes", count]).unwrap_err();
        assert!(error.contains(expected), "{error}");
        assert!(!machine.exists());
    }
    let error = create(&["--graphics", "none", "--graphics-pipes", "2"]).unwrap_err();
    assert!(error.contains("is not offered without graphics"), "{error}");

    create(&["--graphics-pipes", "4"]).unwrap();
    let file = origami::read_machine(&machine).unwrap();
    let slots: Vec<_> = file.pipe.iter().map(|pipe| pipe.slot.as_str()).collect();
    assert_eq!(
        slots,
        [
            "gbrick0-pipe0-pipe",
            "gbrick0-pipe1-pipe",
            "gbrick1-pipe0-pipe",
            "gbrick1-pipe1-pipe"
        ]
    );
    // Each pipe keeps its own flash, which create made.
    for slot in slots {
        let flash = machine.join(format!("state/{slot}-kona-flash.raw"));
        assert_eq!(fs::metadata(flash).unwrap().len(), 524288);
    }
    let shown = origami(root, &["show-command", dir]).unwrap();
    assert_eq!(shown.matches("sgi-kona,").count(), 4, "{shown}");
}

#[test]
fn an_onyx2_rack_gets_the_second_node_its_second_pipe_needs() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    let (prom, io_prom) = proms(root);
    let machine = root.join("rack");
    let dir = machine.to_str().unwrap();
    let args = |count: &'static str| {
        vec![
            "create",
            dir,
            "--preset",
            "onyx2-rack-infinite-reality",
            "--prom",
            Box::leak(prom.clone().into_boxed_str()),
            "--io-prom",
            Box::leak(io_prom.clone().into_boxed_str()),
            "--graphics-pipes",
            count,
        ]
    };
    let error = origami(root, &args("3")).unwrap_err();
    assert!(error.contains("at most 2"), "{error}");
    let output = origami(root, &args("2")).unwrap();
    assert!(output.contains("2 graphics pipes need 2 nodes"), "{output}");
    let file = origami::read_machine(&machine).unwrap();
    assert_eq!(file.machine.nodes, 2);
    assert_eq!(file.machine.population, [1, 1]);
    let slots: Vec<_> = file.pipe.iter().map(|pipe| pipe.slot.as_str()).collect();
    assert_eq!(slots, ["io3", "io7"]);
    origami(root, &["validate", dir]).unwrap();
}
