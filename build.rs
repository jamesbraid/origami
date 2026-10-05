use std::path::Path;
use std::process::Command;
use vergen_gitcl::{Emitter, Gitcl};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let git = Gitcl::builder()
        .describe(true, true, Some("v[0-9]*"))
        .build();
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
