//! The product under test, driven as a subprocess, the way a user runs it.

use std::ffi::OsStr;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use super::{secs, Result};

#[derive(Clone)]
pub struct Product {
    pub cli: PathBuf,
    pub qemu_img: PathBuf,
    /// This test's scratch directory.
    pub work: PathBuf,
    env: Vec<(String, String)>,
}

pub struct Out {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl Out {
    pub fn text(&self) -> String {
        format!("{}{}", self.stdout, self.stderr).trim().to_string()
    }
}

fn required(name: &str) -> String {
    match std::env::var(name) {
        Ok(v) if !v.is_empty() => v,
        _ => panic!(
            "{name} is not set. These tests run a built product: set ORIGAMI_PRODUCT to an \
             extracted product tree and ORIGAMI_WORK to a scratch directory (tests/integration/README.md)"
        ),
    }
}

impl Product {
    /// The product named by `ORIGAMI_PRODUCT`, with scratch space for the
    /// test called `test` under `ORIGAMI_WORK`. Fails the test when either
    /// is missing: an explicitly requested run must not pass by skipping.
    pub fn from_env(test: &str) -> Product {
        let root = PathBuf::from(required("ORIGAMI_PRODUCT"));
        let exe = if cfg!(windows) { ".exe" } else { "" };
        let cli = root.join("bin").join(format!("origami{exe}"));
        let qemu_img = root.join("libexec/origami").join(format!("qemu-img{exe}"));
        assert!(cli.exists(), "{} does not exist", cli.display());
        let work = PathBuf::from(required("ORIGAMI_WORK")).join(test);
        let _ = std::fs::remove_dir_all(&work);
        std::fs::create_dir_all(&work).expect("create the test's work directory");
        // `canonicalize` would give Windows a `\\?\` path that QEMU does not take.
        let work = std::path::absolute(&work).expect("resolve the work directory");
        // Keep the product's cache and configuration out of the runner's profile.
        let home = work.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let env = vec![
            ("HOME".into(), home.display().to_string()),
            (
                "XDG_CACHE_HOME".into(),
                work.join("xdg").display().to_string(),
            ),
            ("USERPROFILE".into(), home.display().to_string()),
            (
                "LOCALAPPDATA".into(),
                home.join("local").display().to_string(),
            ),
            ("APPDATA".into(), home.join("roaming").display().to_string()),
        ];
        Product {
            cli,
            qemu_img,
            work,
            env,
        }
    }

    pub fn machines(&self) -> PathBuf {
        let d = self.work.join("machines");
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// Where logs and captures the job keeps go.
    pub fn evidence(&self) -> PathBuf {
        let d = self.work.join("evidence");
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    pub fn command(&self, program: &Path) -> Command {
        let mut c = Command::new(program);
        c.envs(self.env.iter().map(|(k, v)| (k, v)))
            .env_remove("ORIGAMI_RUNTIME_DIR")
            .env_remove("LD_LIBRARY_PATH")
            .env_remove("DYLD_LIBRARY_PATH")
            .current_dir(&self.work);
        c
    }

    /// Run `origami` and return its output; the exit status is not checked.
    pub fn origami<I, S>(&self, args: I) -> Result<Out>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.origami_for(args, secs(300))
    }

    pub fn origami_for<I, S>(&self, args: I, limit: Duration) -> Result<Out>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let args: Vec<_> = args.into_iter().collect();
        let shown: Vec<_> = args.iter().map(|a| a.as_ref().to_string_lossy()).collect();
        let mut cmd = self.command(&self.cli);
        cmd.args(&args);
        let out = run_limited(cmd, limit)?;
        let text = out.text();
        let tail = super::tail(&text, 300);
        println!("+ origami {} -> {}: {}", shown.join(" "), out.code, tail);
        Ok(out)
    }

    /// Run `origami`, failing with its message when it exits non-zero.
    pub fn origami_ok<I, S>(&self, args: I) -> Result<Out>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.origami_ok_for(args, secs(300))
    }

    pub fn origami_ok_for<I, S>(&self, args: I, limit: Duration) -> Result<Out>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let args: Vec<_> = args.into_iter().collect();
        let out = self.origami_for(&args, limit)?;
        if out.code != 0 {
            let first = args
                .first()
                .map(|a| a.as_ref().to_string_lossy().into_owned());
            return Err(format!("origami {}: {}", first.unwrap_or_default(), out.text()).into());
        }
        Ok(out)
    }

    pub fn qemu_img<I, S>(&self, args: I) -> Result<Out>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let mut cmd = self.command(&self.qemu_img);
        cmd.args(args);
        let out = run_limited(cmd, secs(300))?;
        if out.code != 0 {
            return Err(format!("qemu-img: {}", out.text()).into());
        }
        Ok(out)
    }

    /// True when `origami status` says the machine is not running.
    pub fn stopped(&self, machine: &Path) -> bool {
        self.origami([OsStr::new("status"), machine.as_os_str()])
            .map(|o| o.stdout.trim() == "stopped")
            .unwrap_or(false)
    }
}

/// Run a command to completion within `limit`, collecting its output. A
/// process that outlives the limit is killed.
pub fn run_limited(mut cmd: Command, limit: Duration) -> Result<Out> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("cannot start {:?}: {e}", cmd.get_program()))?;
    let (tx, rx) = mpsc::channel();
    for (which, pipe) in [
        (
            0,
            child
                .stdout
                .take()
                .map(|p| Box::new(p) as Box<dyn Read + Send>),
        ),
        (
            1,
            child
                .stderr
                .take()
                .map(|p| Box::new(p) as Box<dyn Read + Send>),
        ),
    ] {
        let tx = tx.clone();
        std::thread::spawn(move || {
            let mut text = Vec::new();
            if let Some(mut p) = pipe {
                let _ = p.read_to_end(&mut text);
            }
            let _ = tx.send((which, text));
        });
    }
    drop(tx);
    let end = Instant::now() + limit;
    let code = loop {
        match child.try_wait()? {
            Some(status) => break status.code().unwrap_or(-1),
            None if Instant::now() > end => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("{:?} ran past {limit:?}", cmd.get_program()).into());
            }
            None => std::thread::sleep(Duration::from_millis(50)),
        }
    };
    // A detached grandchild can keep a pipe open after the command itself
    // has exited, so wait for the readers only briefly.
    let mut parts = [Vec::new(), Vec::new()];
    while let Ok((which, text)) = rx.recv_timeout(secs(2)) {
        parts[which] = text;
    }
    Ok(Out {
        code,
        stdout: String::from_utf8_lossy(&parts[0]).into_owned(),
        stderr: String::from_utf8_lossy(&parts[1]).into_owned(),
    })
}
