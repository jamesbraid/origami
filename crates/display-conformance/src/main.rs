use display_conformance::client::Target;
use display_conformance::{run, Outcome};

fn main() {
    let (mut socket, mut filter, mut diag, mut list, mut allow_skips) =
        (None, String::new(), true, false, false);
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--socket" => socket = args.next(),
            "--filter" => filter = args.next().unwrap_or_default(),
            "--no-diagnostics" => diag = false,
            "--allow-skips" => allow_skips = true,
            "--list" => list = true,
            _ => usage(),
        }
    }
    if list {
        for (n, _) in display_conformance::checks::CHECKS {
            println!("{n}");
        }
        return;
    }
    let Some(socket) = socket else { usage() };
    let t = Target {
        path: socket.into(),
    };
    let report = match run(&t, &filter, diag, |name, r, d| match r {
        Outcome::Pass => println!("PASS {name} ({:.1}s)", d.as_secs_f64()),
        Outcome::Fail(e) => println!("FAIL {name}: {e}"),
        Outcome::Skip(e) => println!("SKIP {name}: {e}"),
    }) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("display-conformance: {e}");
            std::process::exit(2);
        }
    };
    for d in &report.diagnostics {
        println!("DIAG {d}");
    }
    let (n, f) = (report.results.len(), report.failures());
    let skipped = report
        .results
        .iter()
        .filter(|r| matches!(r.1, Outcome::Skip(_)))
        .count();
    println!("{} passed, {f} failed, {skipped} skipped", n - f - skipped);
    std::process::exit(if f > 0 || (skipped > 0 && !allow_skips) {
        1
    } else {
        0
    });
}

fn usage() -> ! {
    eprintln!(
        "usage: display-conformance --socket PATH [--filter SUBSTRING] [--no-diagnostics] [--allow-skips] [--list]"
    );
    std::process::exit(2);
}
