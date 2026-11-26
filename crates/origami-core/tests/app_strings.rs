//! What the three apps may write for themselves. Words shown in the same
//! place come from the core (`UiString`), so a user-visible literal in app
//! source must be Sentence case, must not repeat a core string, and must not
//! be shown by two apps. Self-test files are not scanned.

use origami_core::facade::UiString;
use regex::Regex;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

/// Capitalised words that are names, not Title Case.
const PROPER: &[&str] = &[
    "IRIX", "CD", "CD-ROM", "Origami", "Finder", "OK", "ID", "QEMU", "Iris", "MIPSpro", "RAD4",
    "PROM", "NFS", "GB", "MB", "SGI", "Explorer", "Files", "Sound", "Luck", "Commons", "Monitor",
    "HTTPS", "HTTP", "URL", "Segoe", "UI", "Variable", "Cascadia", "Mono", "Consolas", "Render",
    "System", "Meta",
];

/// Literals two apps may both write: platform words and shortcut text.
const SHARED_OK: &[&str] = &[
    "Quit",
    "Hide",
    "Services",
    "Window",
    "Edit",
    "View",
    "Help",
    "ADDRESS/PREFIX",
];

/// Core words an app still spells where it cannot call the core: a constant in
/// a pure-logic project or a static table. Keep this list short.
const CONST_OK: &[&str] = &[
    "Needs upgrade",
    // New 0.6.2 core words the apps still spell. Remove these when all three apps move.
    "Done",
    "Machine",
    "Machine ID",
    "Type it",
    "Disk",
    "Eject",
    "Packages to install, one per line",
    "Powering off",
    "Select a machine first.",
    "Choose…",
    "Stop sharing",
];

/// The calls and properties that put a literal in front of a person.
const SWIFT: &str = r#"(?:Text|Button|Label|Toggle|Picker|Section|TextField|LabeledContent|Stepper|Menu|help|accessibilityLabel|navigationTitle|Window|WindowGroup|addButton|messageText|informativeText)\(\s*(?:withTitle:\s*)?"([^"\\]{2,})""#;
const CSHARP: &str = r#"(?:Text|Content|Header|Label|Title|PlaceholderText|ToolTip|PrimaryButtonText|SecondaryButtonText|CloseButtonText|Description)\s*=\s*"([^"\\{]{2,})""#;
const GTK: &str = r#"\.(?:label|title|subtitle|tooltip_text|placeholder_text|description|button_label|heading|body)\(\s*&?"([^"\\{]{2,})"|(?:button|check|append|set_label|set_title|set_tooltip_text|with_label|value_row)\(\s*(?:Some\()?"([^"\\{]{2,})""#;

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
        ) && !name.contains("Selftest")
            && !name.contains("Tests")
            && !name.contains("selftest")
        {
            out.push(path);
        }
    }
}

/// Every user-visible literal in an app's sources, with where it is.
fn literals(app_dirs: &[&str], pattern: &str) -> BTreeMap<String, Vec<String>> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let rx = Regex::new(pattern).unwrap();
    let mut files = Vec::new();
    for d in app_dirs {
        sources(&root.join(d), &mut files);
    }
    assert!(
        files.len() > 8,
        "{app_dirs:?}: only {} sources",
        files.len()
    );
    let mut found: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for file in files {
        let text = fs::read_to_string(&file).unwrap_or_default();
        for m in rx.captures_iter(&text) {
            let s = m.iter().skip(1).flatten().next().unwrap().as_str();
            if s.chars().any(|c| c.is_alphabetic()) {
                let line = text[..m.get(0).unwrap().start()].matches('\n').count() + 1;
                found.entry(s.to_string()).or_default().push(format!(
                    "{}:{line}",
                    file.strip_prefix(&root).unwrap().display()
                ));
            }
        }
    }
    found
}

fn apps() -> [(&'static str, BTreeMap<String, Vec<String>>); 3] {
    [
        ("macOS", literals(&["apps/macos/Sources"], SWIFT)),
        (
            "Windows",
            literals(
                &["apps/windows/Origami.App", "apps/windows/Origami.Logic"],
                CSHARP,
            ),
        ),
        ("GTK", literals(&["crates/origami-gtk/src"], GTK)),
    ]
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
    let mut bad = Vec::new();
    for (app, found) in apps() {
        for (s, at) in found {
            if title_case(&s) {
                bad.push(format!("{app}: {s:?} at {}", at[0]));
            }
        }
    }
    assert!(bad.is_empty(), "Title Case labels:\n{}", bad.join("\n"));
}

#[test]
fn words_the_core_owns_are_not_written_in_an_app() {
    let core: BTreeSet<&str> = UiString::ALL.iter().map(|s| s.text()).collect();
    let mut bad = Vec::new();
    for (app, found) in apps() {
        for (s, at) in found {
            if (core.contains(s.as_str()) && !CONST_OK.contains(&s.as_str()))
                || s.starts_with("Version ")
            {
                bad.push(format!(
                    "{app}: {s:?} at {}: use the core's UiString",
                    at[0]
                ));
            }
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

#[test]
fn two_apps_do_not_write_the_same_words() {
    let all = apps();
    let mut seen: BTreeMap<String, BTreeSet<&str>> = BTreeMap::new();
    for (app, found) in &all {
        for s in found.keys() {
            let key = s.trim_end_matches('…').to_lowercase();
            if !s.contains("Ctrl+")
                && !SHARED_OK.contains(&s.as_str())
                && !CONST_OK.contains(&s.as_str())
            {
                seen.entry(key).or_default().insert(app);
            }
        }
    }
    let bad: Vec<_> = seen
        .iter()
        .filter(|(_, a)| a.len() > 1)
        .map(|(s, a)| format!("{s:?} in {a:?}"))
        .collect();
    assert!(
        bad.is_empty(),
        "move these into UiString:\n{}",
        bad.join("\n")
    );
}

#[test]
fn the_check_tells_title_case_from_names() {
    assert!(title_case("Power Off"));
    assert!(title_case("Add a Package…"));
    assert!(!title_case("Power off"));
    assert!(!title_case("Install IRIX…"));
    assert!(!title_case("Show in Finder"));
}
