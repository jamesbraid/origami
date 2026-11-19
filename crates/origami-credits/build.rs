//! Copies the notices the credits are built from: `ORIGAMI_NOTICES` (a package
//! build's full file) or else the repository's.

use std::path::{Path, PathBuf};

fn main() {
    let root = Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("../..");
    println!("cargo:rerun-if-env-changed=ORIGAMI_NOTICES");
    let source = std::env::var_os("ORIGAMI_NOTICES")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("THIRD_PARTY_NOTICES"));
    println!("cargo:rerun-if-changed={}", source.display());
    let out = Path::new(&std::env::var("OUT_DIR").unwrap()).join("notices.txt");
    std::fs::copy(&source, out).unwrap_or_else(|e| panic!("{}: {e}", source.display()));
}
