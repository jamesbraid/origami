//! `cargo xtask selftest --display [--out DIR]`: build the fake-engine app, which
//! no release contains, and run the machine-window behaviour table on this
//! machine's own screen. Readbacks and a window screenshot land in DIR.

use crate::archive::Result;
use crate::{root, run, target_dir};
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};

pub fn run_display(out: Option<PathBuf>) -> Result<()> {
    if !cfg!(target_os = "macos") {
        return Err(
            "selftest --display runs the macOS app on this screen; on Linux use \
             `crates/origami-gtk/ci/linux.sh DIR behaviour`, on Windows the fake-engine \
             self-test in .github/workflows/app-windows.yml"
                .into(),
        );
    }
    let out = out.unwrap_or_else(|| target_dir().join("selftest-display"));
    let out = if out.is_absolute() {
        out
    } else {
        std::env::current_dir()?.join(out)
    };
    let (app, library, readbacks) = (
        out.join("Origami.app"),
        out.join("machines"),
        out.join("readbacks"),
    );
    for dir in [&library, &readbacks] {
        let _ = fs::remove_dir_all(dir);
        fs::create_dir_all(dir)?;
    }
    let mut build = Command::new(root().join("apps/macos/build.sh"));
    build
        .env("APP_OUT", &app)
        .env("ORIGAMI_FFI_FEATURES", "fake")
        .env("CARGO_TARGET_DIR", target_dir());
    run(build)?;
    let report = fs::File::create(readbacks.join("selftest-display.jsonl"))?;
    let status = Command::new(app.join("Contents/MacOS/OrigamiApp"))
        .arg("--selftest-display")
        .arg(&library)
        .arg(&readbacks)
        .stdout(Stdio::from(report))
        .status()?;
    println!("readbacks and screenshots: {}", readbacks.display());
    println!(
        "{}",
        fs::read_to_string(readbacks.join("selftest-display.jsonl"))?
    );
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "the display self-test failed ({status}); see {}",
            readbacks.display()
        )
        .into())
    }
}
