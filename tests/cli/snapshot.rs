//! The command line's output, recorded. The product is being restructured
//! under a fixed command line, and this test is what says the restructuring
//! changed nothing a user can see: help for every subcommand, the machine
//! listings, `show` and `show-command` for several machines, the errors
//! validation gives, and an `origami console` session against a scripted guest.
//!
//! The record is `tests/fixtures/cli-snapshot.txt`. A change that is meant to
//! alter the output regenerates it with `ORIGAMI_UPDATE_SNAPSHOT=1` and
//! shows the diff in review.

use crate::support::{self, create, ok, run, wait_until, Running};
use std::fmt::Write as _;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

const FIXTURE: &str = "tests/fixtures/cli-snapshot.txt";

/// Machines whose `show` and `show-command` are recorded: SN0 with one and
/// many nodes, SN1, Octane, Octane2, Onyx2 and Fuel.
const PRESETS: &[&str] = &[
    "origin200-1",
    "origin2000-8",
    "origin300-2",
    "octane-impact",
    "octane2-vpro",
    "onyx2-infinite-reality",
    "fuel-1",
];

struct Recorder<'a> {
    root: &'a Path,
    text: String,
}

impl Recorder<'_> {
    /// `text` as the Linux record has it: the temporary root named `<ROOT>`,
    /// and on Windows no `.exe`, the quotes of the Linux shell and the
    /// Linux wording of a missing file.
    fn scrub(&self, text: &str) -> String {
        let root = self.root.to_str().unwrap().replace('\\', "/");
        let text = text.replace('\\', "/").replace(&root, "<ROOT>");
        if !cfg!(windows) {
            return text;
        }
        // Windows quotes every word with a backslash; Linux quotes only
        // the word holding the `<hosted-port>` placeholder.
        let text = text.replace(".exe", "");
        let text = text
            .split('"')
            .enumerate()
            .map(|(i, part)| match (i % 2, part.contains("<hosted-port>")) {
                (1, true) => format!("'{part}'"),
                _ => part.to_string(),
            })
            .collect::<String>();
        text.replace(
            "The system cannot find the file specified. (os error 2)",
            "No such file or directory (os error 2)",
        )
        .replace(
            "The system cannot find the path specified. (os error 3)",
            "No such file or directory (os error 2)",
        )
    }

    fn section(&mut self, title: &str, body: &str) {
        let body = self.scrub(body);
        writeln!(self.text, "=== {title}").unwrap();
        self.text.push_str(&body);
        if !body.ends_with('\n') {
            self.text.push('\n');
        }
    }

    /// Run `origami args` and record the exit status and both streams.
    fn command(&mut self, args: &[&str]) {
        let output = run(self.root, args);
        let shown: Vec<String> = args.iter().map(|arg| self.scrub(arg)).collect();
        let mut body = format!("exit {}\n", output.status.code().unwrap_or(-1));
        body.push_str("--- stdout\n");
        body.push_str(&String::from_utf8_lossy(&output.stdout));
        body.push_str("--- stderr\n");
        body.push_str(&String::from_utf8_lossy(&output.stderr));
        self.section(&format!("origami {}", shown.join(" ")), &body);
    }
}

/// The subcommand names `origami --help` lists.
fn subcommands(root: &Path) -> Vec<String> {
    let help = ok(root, &["--help"]);
    help.lines()
        .skip_while(|line| *line != "Commands:")
        .skip(1)
        .take_while(|line| line.starts_with("  "))
        .filter_map(|line| line.split_whitespace().next())
        .filter(|name| *name != "help")
        .map(str::to_owned)
        .collect()
}

fn console_session(root: &Path, rec: &mut Recorder) {
    let guest = root.join("console-guest.txt");
    fs::write(
        &guest,
        "outln IRIX Release 6.5 IP27 Origami\nout login: \nin root\\n\nout # \nin hinv\\n\noutln 1 250 MHZ IP27 Processor\nout # \nin exit\\n\noutln logout\nwait 200\nclose\n",
    )
    .unwrap();
    let machine = create(root, "console", "origin200-1", &[]);
    let path = machine.to_str().unwrap();
    let output = support::origami(root)
        .env("ORIGAMI_FAKE", format!("guest-file={}", guest.display()))
        .args(["run", path, "--background", "--display", "none"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let _guard = Running::new(root, &machine);
    wait_until("the runner", Duration::from_secs(10), || {
        machine.join("state/run.json").exists()
    });

    let mut child = support::origami(root)
        .args(["console", path])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    // The guest only reads once it has written its prompt, so the input
    // goes in steps.
    for line in ["root\n", "hinv\n", "exit\n"] {
        std::thread::sleep(Duration::from_millis(400));
        input.write_all(line.as_bytes()).unwrap();
        input.flush().unwrap();
    }
    let output = child.wait_with_output().unwrap();
    let mut body = format!("exit {}\n", output.status.code().unwrap_or(-1));
    body.push_str("--- stdout\n");
    body.push_str(&String::from_utf8_lossy(&output.stdout));
    body.push_str("--- stderr\n");
    body.push_str(&String::from_utf8_lossy(&output.stderr));
    rec.section("origami console (scripted guest)", &body);
    rec.command(&["stop", path]);
}

#[test]
fn the_command_line_output_is_unchanged() {
    let root = tempfile::tempdir().unwrap();
    // The origin of every path the product prints: macOS's temporary
    // directory is a link, and Windows's has an 8.3 short name.
    let root = dunce::canonicalize(root.path()).unwrap();
    let root = root.as_path();
    let mut rec = Recorder {
        root,
        text: String::new(),
    };

    rec.command(&["--help"]);
    rec.command(&["help"]);
    for name in subcommands(root) {
        rec.command(&[&name, "--help"]);
    }

    rec.command(&["machines"]);
    rec.command(&["machines", "--all"]);
    rec.command(&["machines", "origin200-1"]);
    rec.command(&["machines", "no-such-preset"]);

    for preset in PRESETS {
        let name = preset.replace('-', "_");
        let machine = create(root, &name, preset, &[]);
        let path = machine.to_str().unwrap();
        rec.command(&["show", path]);
        rec.command(&["show-command", path]);
        rec.command(&["validate", path]);
        rec.command(&["status", path]);
    }

    // Errors validation and the parser give.
    let machine = root.join("origin200_1");
    let path = machine.to_str().unwrap();
    rec.command(&["show", "no-such-machine"]);
    rec.command(&["validate", "no-such-machine"]);
    rec.command(&["create", "bad", "--preset", "no-such-preset"]);
    rec.command(&["graphics-set", path, "--graphics", "no-such-head"]);
    rec.command(&["graphics-set", path, "--graphics", "none"]);
    rec.command(&["console-set", path, "--port", "no-such-port"]);
    rec.command(&["console-set", path, "--port", "l1"]);
    rec.command(&["nvram", path]);
    rec.command(&["nvram", path, "get", "console"]);
    rec.command(&["nvram", path, "get", "no-such-variable"]);
    rec.command(&["nvram", path, "set", "console=d"]);
    rec.command(&["nvram", path, "get", "console"]);
    rec.command(&["network-set", path, "--mode", "nonsense"]);
    rec.command(&[
        "network-set",
        path,
        "--mode",
        "private",
        "--subnet",
        "10.42.0.0/24",
    ]);
    rec.command(&["network-forward-add", path, "--help"]);
    rec.command(&[
        "drive-attach",
        path,
        "/no/such/image.img",
        "--type",
        "disk",
        "--target",
        "1",
    ]);
    rec.command(&["drive-detach", path, "scsi9"]);
    rec.command(&["install-check", path]);
    rec.command(&["show", path]);
    rec.command(&["show-command", path]);
    rec.command(&["console", path]);
    rec.command(&["stop", path]);
    rec.command(&["no-such-command"]);

    console_session(root, &mut rec);

    let text = rec.text;
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURE);
    if std::env::var_os("ORIGAMI_UPDATE_SNAPSHOT").is_some() {
        fs::write(&path, &text).unwrap();
        return;
    }
    let expected = fs::read_to_string(&path).unwrap_or_default();
    if expected != text {
        let actual = path.with_extension("txt.actual");
        fs::write(&actual, &text).unwrap();
        panic!(
            "the command line's output changed; compare {} with {}",
            actual.display(),
            path.display()
        );
    }
}
