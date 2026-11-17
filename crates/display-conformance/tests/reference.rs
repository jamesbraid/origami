//! The reference producer passes the suite, in the pull style and the push style.

use display_conformance::client::Target;
use display_conformance::{run, Outcome};
use origami_display::v4::sys::UnixListener;
use origami_display_producer::reference;

fn suite(push: bool) {
    let dir = tempfile_dir(push);
    let path = dir.join("display.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let p = reference::start(listener, 1, 256, 128, push).unwrap();
    let t = Target { path };
    let filter = std::env::var("CONF_FILTER").unwrap_or_default();
    let report = run(&t, &filter, true, |name, r, _| match r {
        Outcome::Fail(e) => eprintln!("FAIL {name}: {e}"),
        Outcome::Skip(e) => eprintln!("SKIP {name}: {e}"),
        Outcome::Pass => {}
    })
    .unwrap();
    for d in &report.diagnostics {
        eprintln!("DIAG {d}");
    }
    let skipped = report
        .results
        .iter()
        .filter(|r| matches!(r.1, Outcome::Skip(_)))
        .count();
    assert_eq!(skipped, 0, "the reference producer skipped checks");
    p.stop();
    let _ = std::fs::remove_dir_all(dir);
    assert_eq!(report.failures(), 0, "conformance failures");
}

fn tempfile_dir(push: bool) -> std::path::PathBuf {
    let base = std::env::var_os("CARGO_TARGET_TMPDIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let d = base.join(format!("conf-{}-{push}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn pull_reference_producer_passes() {
    suite(false);
}

#[test]
fn push_reference_producer_passes() {
    suite(true);
}
