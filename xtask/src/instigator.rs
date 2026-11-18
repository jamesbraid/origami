//! Instigator built from source, for a platform its releases do not cover.

use crate::archive::{self, Result};
use crate::lock::INSTIGATOR_REPO;
use std::path::Path;
use std::process::Command;

fn run(mut command: Command) -> Result<()> {
    let status = command
        .status()
        .map_err(|error| format!("{command:?}: {error}"))?;
    if !status.success() {
        return Err(format!("{command:?} failed: {status}").into());
    }
    Ok(())
}

/// The Go target of a platform.
fn go_target(platform: &str) -> Result<(&'static str, &'static str)> {
    match platform {
        "windows-arm64" => Ok(("windows", "arm64")),
        other => Err(format!("no source build of Instigator for {other}").into()),
    }
}

/// The Go toolchain's `go version` line, which a build's stamp records so a
/// new toolchain rebuilds.
pub fn go_version() -> Result<String> {
    let out = Command::new("go")
        .arg("version")
        .output()
        .map_err(|error| format!("go is needed to build Instigator from source: {error}"))?;
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Builds Instigator at `commit` into `out`, laid out as a release archive
/// is: the program, its licences and the install configs.
pub fn build(platform: &str, commit: &str, tag: &str, cache: &Path, out: &Path) -> Result<()> {
    let (goos, goarch) = go_target(platform)?;
    let src = cache.join(format!("instigator-{}", &commit[..12.min(commit.len())]));
    if !src.join(".git").exists() {
        std::fs::create_dir_all(&src)?;
        let mut init = Command::new("git");
        init.args(["init", "-q"]).arg(&src);
        run(init)?;
    }
    let git = |args: &[&str]| {
        let mut command = Command::new("git");
        command.arg("-C").arg(&src).args(args);
        run(command)
    };
    let url = format!("https://github.com/{INSTIGATOR_REPO}.git");
    // The configs are compared byte for byte, so no line-ending conversion.
    git(&["config", "core.autocrlf", "false"])?;
    // An existing checkout may lack the remote or the commit.
    let _ = git(&["remote", "add", "origin", &url]);
    git(&["fetch", "-q", "--depth", "1", "origin", commit])?;
    // Go names a build's version from the tag on the checked-out commit, so
    // fetch it for `instigator --version` to say v0.5.0, not a pseudo-version.
    let _ = git(&["fetch", "-q", "--depth", "1", "origin", "tag", tag]);
    git(&["checkout", "-q", "--force", "--detach", commit])?;
    let exe = if goos == "windows" {
        "instigator.exe"
    } else {
        "instigator"
    };
    if out.exists() {
        std::fs::remove_dir_all(out)?;
    }
    std::fs::create_dir_all(out)?;
    let mut build = Command::new("go");
    build
        .args(["build", "-trimpath", "-ldflags", "-s -w", "-o"])
        .arg(out.join(exe))
        .arg("./cmd/instigator")
        .current_dir(&src)
        .env("CGO_ENABLED", "0")
        .env("GOOS", goos)
        .env("GOARCH", goarch);
    run(build)?;
    for file in ["LICENSE", "THIRD_PARTY_NOTICES"] {
        archive::copy_file(&src.join(file), &out.join(file))?;
    }
    // A release archive carries the YAML files only, not the Go test beside them.
    for dir in ["configs", "configs/addons"] {
        for entry in std::fs::read_dir(src.join(dir))? {
            let path = entry?.path();
            if path.extension().is_some_and(|e| e == "yaml") {
                let name = path.file_name().unwrap();
                archive::copy_file(&path, &out.join(dir).join(name))?;
            }
        }
    }
    Ok(())
}
