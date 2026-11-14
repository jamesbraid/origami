//! What the component tests and the contract tests share: a runtime
//! directory, the `origami` command pointed at it, and a guard that stops
//! the machines a test starts.
//!
//! The component tests run against the fake runtime (`tests/bin/fake.rs`),
//! which makes a runtime directory out of one binary under four names. The
//! contract tests run the same commands against a real QEMU.
#![allow(dead_code)]

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

/// The product binary Cargo built for this test.
pub const ORIGAMI: &str = env!("CARGO_BIN_EXE_origami");

/// Where a test's runtime directory goes: it holds fake programs, and the
/// QEMU data directory two levels above it, as the release archive lays
/// them out.
pub fn fake_runtime(root: &Path) -> PathBuf {
    let prefix = root.join("prefix");
    let runtime = prefix.join("libexec/origami");
    let keymaps = prefix.join("share/origami/qemu/keymaps");
    let keymap = keymaps.join("en-us");
    // The keymap is made last. A test that takes a program away to see
    // what fails without it has it stay away.
    if keymap.exists() {
        return runtime;
    }
    fs::create_dir_all(&runtime).unwrap();
    fs::create_dir_all(&keymaps).unwrap();
    let fake = Path::new(env!("CARGO_BIN_EXE_origami-fake"));
    let suffix = if cfg!(windows) { ".exe" } else { "" };
    for name in [
        "qemu-system-mips64",
        "qemu-img",
        "qemu-sgi-machine-init",
        "instigator",
    ] {
        let target = runtime.join(format!("{name}{suffix}"));
        // The fake finds out which program it is from the name it is run
        // under. Linking it does not write a file this process holds open,
        // which a copy would: a child forked meanwhile by another test
        // thread would inherit that descriptor, and running the copy would
        // fail with "Text file busy".
        match fs::hard_link(fake, &target) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(_) => match link_or_copy(fake, &target) {
                Ok(_) => {}
                Err(error) if target.exists() => drop(error),
                Err(error) => panic!("cannot make {}: {error}", target.display()),
            },
        }
    }
    fs::write(keymap, "").unwrap();
    runtime
}

/// A symbolic link where the platform has them (the fake finds out which
/// program it is from the name it is run under, not from where it lives),
/// and a copy where not.
fn link_or_copy(fake: &Path, target: &Path) -> io::Result<()> {
    #[cfg(unix)]
    return std::os::unix::fs::symlink(fake, target);
    #[cfg(not(unix))]
    fs::copy(fake, target).map(drop)
}

/// The arguments the fake init tool last ran with, one per line.
pub fn init_args(root: &Path) -> String {
    fs::read_to_string(fake_runtime(root).join("qemu-sgi-machine-init.args")).unwrap()
}

/// The init tool under `root`'s runtime directory.
pub fn init_tool(root: &Path) -> PathBuf {
    fake_runtime(root).join(format!(
        "qemu-sgi-machine-init{}",
        if cfg!(windows) { ".exe" } else { "" }
    ))
}

/// An `origami` command that uses the fake runtime and keeps its PROM cache
/// inside the test directory.
pub fn origami(root: &Path) -> Command {
    let mut command = Command::new(ORIGAMI);
    command
        .env("ORIGAMI_RUNTIME_DIR", fake_runtime(root))
        .env("ORIGAMI_CACHE_DIR", root.join("cache"))
        .env_remove("ORIGAMI_FAKE");
    command
}

/// Run `origami args`, and the output whether or not it succeeded.
pub fn run(root: &Path, args: &[&str]) -> Output {
    origami(root).args(args).output().unwrap()
}

/// Standard output of `origami args`, which must succeed.
pub fn ok(root: &Path, args: &[&str]) -> String {
    let output = run(root, args);
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// Standard error of `origami args`, which must fail.
pub fn fail(root: &Path, args: &[&str]) -> String {
    let output = run(root, args);
    assert!(!output.status.success(), "{args:?} succeeded");
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// A stand-in for firmware: the fake init tool and the product only check
/// that the file exists.
pub fn synthetic_prom(root: &Path) -> PathBuf {
    let prom = root.join("synthetic-prom.bin");
    if !prom.exists() {
        fs::write(&prom, vec![0; 1024 * 1024]).unwrap();
    }
    prom
}

/// Create the machine `name` from `preset` under `root`; returns its
/// directory. `extra` goes after the preset's own arguments.
pub fn create(root: &Path, name: &str, preset: &str, extra: &[&str]) -> PathBuf {
    let dir = root.join(name);
    let prom = synthetic_prom(root);
    let mut args = vec![
        "create",
        dir.to_str().unwrap(),
        "--preset",
        preset,
        "--prom",
        prom.to_str().unwrap(),
    ];
    args.extend(extra);
    ok(root, &args);
    dir
}

/// Poll `condition` until it holds, or fail after `timeout`.
pub fn wait_until(what: &str, timeout: Duration, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + timeout;
    while !condition() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// What the fake QEMU wrote about itself in `machine/logs/fake-qemu.log`.
pub fn fake_log(machine: &Path) -> Vec<String> {
    fs::read_to_string(machine.join("logs/fake-qemu.log"))
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

/// The command line the fake QEMU was started with.
pub fn fake_argv(machine: &Path) -> Vec<String> {
    let log = fake_log(machine);
    let line = log
        .iter()
        .rev()
        .find_map(|line| line.strip_prefix("argv "))
        .expect("the fake QEMU recorded no command line");
    serde_json::from_str(line).unwrap()
}

/// Whether `argv` holds `option` followed by `value`.
pub fn has_option(argv: &[String], option: &str, value: &str) -> bool {
    argv.windows(2)
        .any(|pair| pair[0] == option && pair[1] == value)
}

/// The value after `option` in `argv`, for the first time it appears
/// whose value starts with `prefix`.
pub fn option_value<'a>(argv: &'a [String], option: &str, prefix: &str) -> Option<&'a str> {
    argv.windows(2)
        .find(|pair| pair[0] == option && pair[1].starts_with(prefix))
        .map(|pair| pair[1].as_str())
}

/// Stops the machine in `dir` when dropped, so a failed test does not leave
/// a QEMU behind. A test that stops the machine itself is not affected.
pub struct Running {
    root: PathBuf,
    dir: PathBuf,
}

impl Running {
    pub fn new(root: &Path, dir: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
            dir: dir.to_path_buf(),
        }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        let stop = origami(&self.root)
            .arg("stop")
            .arg(&self.dir)
            .output()
            .is_ok_and(|output| output.status.success());
        if !stop {
            // The machine was not running, or would not answer: end its
            // process by the pid it published.
            if let Ok(text) = fs::read_to_string(self.dir.join("state/run.json")) {
                if let Some(pid) = serde_json::from_str::<serde_json::Value>(&text)
                    .ok()
                    .and_then(|record| record["pid"].as_u64())
                {
                    kill(pid as u32);
                }
            }
        }
    }
}

#[cfg(unix)]
fn kill(pid: u32) {
    let _ = Command::new("kill").arg(pid.to_string()).status();
}

#[cfg(windows)]
fn kill(pid: u32) {
    let _ = Command::new("taskkill")
        .args(["/F", "/T", "/PID", &pid.to_string()])
        .status();
}

/// Copy the directory `from` to `to`, recursively.
pub fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

/// Accept one connection on `listener`, or fail after `timeout`.
pub fn accept_within(listener: &std::net::TcpListener, timeout: Duration) -> std::net::TcpStream {
    listener.set_nonblocking(true).unwrap();
    let deadline = Instant::now() + timeout;
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                stream.set_nonblocking(false).unwrap();
                return stream;
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                assert!(
                    Instant::now() < deadline,
                    "nothing connected within {timeout:?}"
                );
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(error) => panic!("accept: {error}"),
        }
    }
}

/// Kills a child process when dropped.
pub struct KillOnDrop(pub std::process::Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
