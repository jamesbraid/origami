use std::path::{Path, PathBuf};
use std::process::Command;

fn gitlink_revision(root: &Path, name: &str) -> Option<String> {
    let output = Command::new("git")
        .args(["-C"])
        .arg(root)
        .args(["ls-files", "--stage", "--", name])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }

    let text = String::from_utf8(output.stdout).ok()?;
    let mut lines = text.lines();
    let line = lines.next()?;
    if lines.next().is_some() {
        return None;
    }
    let mut fields = line.split_whitespace();
    let mode = fields.next()?;
    let revision = fields.next()?;
    let stage = fields.next()?;
    let path = fields.next()?;
    if mode != "160000"
        || stage != "0"
        || path != name
        || revision.len() != 40
        || !revision.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return None;
    }
    Some(revision.to_owned())
}

fn git_index_path(root: &Path) -> Option<PathBuf> {
    let output = Command::new("git")
        .args(["-C"])
        .arg(root)
        .args(["rev-parse", "--git-path", "index"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let path = PathBuf::from(String::from_utf8(output.stdout).ok()?.trim());
    Some(if path.is_absolute() { path } else { root.join(path) })
}

fn main() {
    println!("cargo:rerun-if-env-changed=SGI_QEMU_REVISION");
    println!("cargo:rerun-if-env-changed=SGI_INSTIGATOR_REVISION");
    let root = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    if let Some(index) = git_index_path(&root) {
        println!("cargo:rerun-if-changed={}", index.display());
    }

    let qemu = std::env::var("SGI_QEMU_REVISION")
        .ok()
        .filter(|value| value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .or_else(|| gitlink_revision(&root, "qemu"));
    let instigator = std::env::var("SGI_INSTIGATOR_REVISION")
        .ok()
        .filter(|value| value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .or_else(|| gitlink_revision(&root, "instigator"));

    let unavailable = "unavailable (Git submodule metadata is unavailable to this Cargo build)";
    println!(
        "cargo:rustc-env=SGI_QEMU_REVISION={}",
        qemu.as_deref().unwrap_or(unavailable)
    );
    println!(
        "cargo:rustc-env=SGI_INSTIGATOR_REVISION={}",
        instigator.as_deref().unwrap_or(unavailable)
    );
}
