use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;
use vergen_gitcl::{Emitter, GitclBuilder};

fn required_path(name: &str) -> Result<PathBuf, Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-env-changed={name}");
    let value = env::var_os(name).ok_or_else(|| {
        format!("{name} is missing. Build QEMU's portable library first: configure QEMU, then make -C <qemu-build> libsgi-firmware-core.a. Set SGI_FIRMWARE_ARCHIVE and SGI_FIRMWARE_SOURCE_DIR to the absolute archive and QEMU source paths")
    })?;
    let path = PathBuf::from(value);
    if !path.is_absolute() || !path.exists() {
        return Err(format!(
            "{name} must name an existing absolute path: {}",
            path.display()
        )
        .into());
    }
    if path.is_file() {
        println!("cargo:rerun-if-changed={}", path.display());
    }
    Ok(path)
}

fn firmware_link() -> Result<(), Box<dyn std::error::Error>> {
    let archive = required_path("SGI_FIRMWARE_ARCHIVE")?;
    let source = required_path("SGI_FIRMWARE_SOURCE_DIR")?;
    println!("cargo:rerun-if-env-changed=SGI_FIRMWARE_RUST_TARGET");
    let supplied_target = env::var("SGI_FIRMWARE_RUST_TARGET").unwrap_or_default();
    let target = env::var("TARGET")?;
    if (!supplied_target.is_empty() && supplied_target != target)
        || (supplied_target.is_empty() && env::var("HOST")? != target)
    {
        return Err(format!("firmware archive was configured for a different Rust target; build the QEMU archive with the native toolchain for {target} and set SGI_FIRMWARE_RUST_TARGET={target}").into());
    }
    if archive.file_name().and_then(|name| name.to_str()) != Some("libsgi-firmware-core.a") {
        return Err("SGI_FIRMWARE_ARCHIVE must name QEMU's libsgi-firmware-core.a".into());
    }
    for file in [
        "util/sgi-prom-image.c",
        "util/sgi-flash-image.c",
        "util/sgi-flash-layouts.c",
        "util/sgi-flash-layouts.h",
        "include/sgi/prom-image.h",
        "include/sgi/flash-image.h",
    ] {
        println!("cargo:rerun-if-changed={}", source.join(file).display());
    }
    println!(
        "cargo:rustc-link-search=native={}",
        archive.parent().unwrap().display()
    );
    println!("cargo:rustc-link-lib=static=sgi-firmware-core");
    println!("cargo:rustc-link-lib=z");
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    firmware_link()?;
    let git = GitclBuilder::default()
        .describe(true, true, Some("v[0-9]*"))
        .build()?;
    Emitter::default().add_instructions(&git)?.emit()?;
    // Git refs alone do not change when an existing source file is edited.
    let files = Command::new("git")
        .args([
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
        ])
        .output();
    if let Ok(files) = files {
        if !files.status.success() {
            return Ok(());
        }
        for file in String::from_utf8(files.stdout)?.split('\0') {
            if Path::new(file).is_file() {
                println!("cargo:rerun-if-changed={file}");
            }
        }
    }
    // Tags can change the description without changing HEAD or source files.
    // Git resolves shared refs correctly for submodules and linked worktrees.
    for name in ["index", "refs", "packed-refs"] {
        if let Ok(path) = Command::new("git")
            .args(["rev-parse", "--git-path", name])
            .output()
        {
            if path.status.success() {
                let path = String::from_utf8(path.stdout)?;
                let path = path.trim();
                if Path::new(path).exists() {
                    println!("cargo:rerun-if-changed={path}");
                }
            }
        }
    }
    Ok(())
}
