use std::path::Path;
use std::process::Command;
use vergen_gitcl::{Emitter, GitclBuilder};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let git = GitclBuilder::default().sha(true).dirty(true).build()?;
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
    if let Ok(index) = Command::new("git")
        .args(["rev-parse", "--git-path", "index"])
        .output()
    {
        if index.status.success() {
            println!(
                "cargo:rerun-if-changed={}",
                String::from_utf8(index.stdout)?.trim()
            );
        }
    }
    Ok(())
}
