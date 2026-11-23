//! The three apps word their own labels in source; this keeps them in
//! sentence case like the core's, so `Power Off` does not creep back.

use std::fs;
use std::path::{Path, PathBuf};

/// Capitalised words that are names, not Title Case.
const PROPER: &[&str] = &[
    "IRIX", "CD", "CD-ROM", "Origami", "Finder", "OK", "ID", "QEMU", "Iris", "MIPSpro", "RAD4",
    "PROM", "NFS", "GB", "MB", "SGI", "Explorer", "Files", "Sound", "Luck", "Commons", "Monitor",
    "HTTPS", "HTTP", "URL", "Segoe", "UI", "Variable", "Cascadia", "Mono", "Consolas", "Render",
    "System", "Meta",
];

fn sources(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if path.is_dir() {
            if !matches!(
                name.as_str(),
                "target" | "obj" | "bin" | "Generated" | ".build"
            ) {
                sources(&path, out);
            }
        } else if matches!(
            path.extension().and_then(|e| e.to_str()),
            Some("swift" | "cs" | "xaml" | "rs")
        ) {
            out.push(path);
        }
    }
}

/// A two to four word literal with a capital on a later word that is no name.
fn title_case(literal: &str) -> bool {
    if literal == "GNU General Public License" {
        return false;
    }
    let words: Vec<&str> = literal.split(' ').collect();
    (2..=4).contains(&words.len())
        && words.iter().all(|w| {
            let w = w.trim_end_matches('…');
            !w.is_empty() && w.chars().all(|c| c.is_ascii_alphabetic() || c == '-')
        })
        && words[0].starts_with(|c: char| c.is_ascii_uppercase())
        && words[1..].iter().any(|w| {
            let w = w.trim_end_matches('…');
            w.starts_with(|c: char| c.is_ascii_uppercase()) && !PROPER.contains(&w)
        })
}

#[test]
fn app_labels_are_in_sentence_case() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut files = Vec::new();
    sources(&root.join("apps"), &mut files);
    sources(&root.join("crates/origami-gtk/src"), &mut files);
    assert!(files.len() > 40, "found only {} sources", files.len());
    let mut bad = Vec::new();
    for file in files {
        let text = fs::read_to_string(&file).unwrap_or_default();
        for (n, line) in text.lines().enumerate() {
            let line = line.split("//").next().unwrap_or("");
            for literal in line.split('"').skip(1).step_by(2) {
                if title_case(literal) {
                    bad.push(format!("{}:{}: {literal:?}", file.display(), n + 1));
                }
            }
        }
    }
    assert!(bad.is_empty(), "Title Case labels:\n{}", bad.join("\n"));
}

#[test]
fn the_check_tells_title_case_from_names() {
    assert!(title_case("Power Off"));
    assert!(title_case("Add a Package…"));
    assert!(!title_case("Power off"));
    assert!(!title_case("Install IRIX…"));
    assert!(!title_case("Show in Finder"));
}
