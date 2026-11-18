//! Licence notices for the Rust crates a package links, from `cargo metadata`.

use crate::archive::Result;
use crate::root;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use std::process::Command;

pub struct Krate {
    pub name: String,
    pub version: String,
    license: String,
    dir: std::path::PathBuf,
}

impl Krate {
    pub fn id(&self) -> String {
        format!("{} {}", self.name, self.version)
    }
}

/// The cargo target a platform's package is built for.
pub fn triple<'a>(platform: &str, target: Option<&'a str>) -> &'a str {
    target.unwrap_or(match platform {
        "macos-arm64" => "aarch64-apple-darwin",
        "windows-x86_64" => "x86_64-pc-windows-msvc",
        _ => "x86_64-unknown-linux-gnu",
    })
}

fn metadata(manifest: &Path, triple: &str) -> Result<Value> {
    let mut command = Command::new(std::env::var_os("CARGO").unwrap_or("cargo".into()));
    command.args(["metadata", "--format-version", "1", "--locked"]);
    command.args(["--filter-platform", triple, "--manifest-path"]);
    let out = command.arg(manifest).output()?;
    if !out.status.success() {
        return Err(format!("cargo metadata: {}", String::from_utf8_lossy(&out.stderr)).into());
    }
    Ok(serde_json::from_slice(&out.stdout)?)
}

/// The third-party crates linked into `roots` (normal dependencies only).
/// Stops if Slint or origami-gui is among them.
pub fn crates(manifest: &Path, roots: &[&str], triple: &str) -> Result<Vec<Krate>> {
    let meta = metadata(manifest, triple)?;
    let packages = meta["packages"].as_array().cloned().unwrap_or_default();
    let nodes = meta["resolve"]["nodes"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let mut todo: Vec<String> = packages
        .iter()
        .filter(|p| roots.iter().any(|r| p["name"] == *r))
        .map(|p| p["id"].as_str().unwrap_or_default().to_string())
        .collect();
    if todo.len() != roots.len() {
        return Err(format!("{roots:?}: not all are packages of {}", manifest.display()).into());
    }
    let mut seen = BTreeSet::new();
    while let Some(id) = todo.pop() {
        if !seen.insert(id.clone()) {
            continue;
        }
        let node = nodes.iter().find(|n| n["id"] == id.as_str());
        for dep in node
            .and_then(|n| n["deps"].as_array())
            .into_iter()
            .flatten()
        {
            let linked = dep["dep_kinds"]
                .as_array()
                .is_some_and(|k| k.iter().any(|k| k["kind"].is_null()));
            if linked {
                todo.push(dep["pkg"].as_str().unwrap_or_default().to_string());
            }
        }
    }
    let mut out = Vec::new();
    for package in packages
        .iter()
        .filter(|p| seen.contains(p["id"].as_str().unwrap_or_default()))
    {
        let name = package["name"].as_str().unwrap_or_default();
        if name == "origami-gui" || name.contains("slint") {
            return Err(format!("{name} is linked; release packages carry no Slint").into());
        }
        if package["source"].is_null() {
            continue;
        }
        out.push(Krate {
            name: name.into(),
            version: package["version"].as_str().unwrap_or_default().into(),
            license: package["license"]
                .as_str()
                .unwrap_or("no licence field")
                .into(),
            dir: Path::new(package["manifest_path"].as_str().unwrap_or_default())
                .parent()
                .unwrap()
                .into(),
        });
    }
    Ok(out)
}

fn licence_files(dir: &Path) -> Result<String> {
    let mut text = String::new();
    let mut files: Vec<_> = fs::read_dir(dir)?.filter_map(|e| e.ok()).collect();
    files.sort_by_key(|e| e.file_name());
    for entry in files {
        let name = entry.file_name().to_string_lossy().to_uppercase();
        let wanted = ["LICENSE", "LICENCE", "COPYING", "UNLICENSE", "NOTICE"];
        if entry.path().is_file() && wanted.iter().any(|p| name.starts_with(p)) {
            let body = String::from_utf8_lossy(&fs::read(entry.path())?).replace("\r\n", "\n");
            text.push_str(body.trim());
            text.push_str("\n\n");
        }
    }
    Ok(text)
}

/// A notices section for `crates` other than those in `skip`, grouped by
/// licence text, with crates that ship no licence file named separately.
pub fn render(
    title: &str,
    intro: &str,
    crates: &[Krate],
    skip: &BTreeSet<String>,
) -> Result<String> {
    let bar = "=".repeat(78);
    let mut out = format!("{bar}\n{title}\n{bar}\n\n{intro}\n");
    let mut groups: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut without = BTreeSet::new();
    for krate in crates.iter().filter(|k| !skip.contains(&k.id())) {
        let text = licence_files(&krate.dir)?;
        if text.is_empty() {
            without.insert(format!("{} ({})", krate.id(), krate.license));
        } else {
            groups.entry(text).or_default().insert(krate.id());
        }
    }
    for (text, names) in &groups {
        let names: Vec<_> = names.iter().cloned().collect();
        out.push_str(&format!(
            "\n--- {}\n{}\n",
            names.join(", "),
            text.trim_end()
        ));
    }
    if !without.is_empty() {
        let list: Vec<_> = without.into_iter().collect();
        out.push_str(&format!(
            "\n--- Crates with no licence file in their package; the licence named applies\n{}\n",
            list.join("\n")
        ));
    }
    Ok(out)
}

const CLI_TITLE: &str = "Rust crates (origami)";

/// `base` (the repository's THIRD_PARTY_NOTICES) with its crate section
/// replaced by one generated for every crate `origami` links.
pub fn cli(base: &str, triple: &str) -> Result<String> {
    // A Windows checkout converts the file to CRLF, which the headings below do not match.
    let base = &base.replace("\r\n", "\n");
    let crates = crates(&root().join("Cargo.toml"), &["origami"], triple)?;
    let bar = "=".repeat(78);
    let title = base
        .find(&format!("{CLI_TITLE}\n"))
        .ok_or("no crate section in THIRD_PARTY_NOTICES")?;
    let start = base[..title]
        .rfind(&bar)
        .ok_or("no crate section heading")?;
    let end = base
        .find(&format!("{bar}\nGo modules"))
        .ok_or("no section after the crates")?;
    if end < start {
        return Err("THIRD_PARTY_NOTICES sections are out of order".into());
    }
    let section = render(CLI_TITLE, "", &crates, &BTreeSet::new())?;
    Ok(format!("{}{}\n{}", &base[..start], section, &base[end..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    const LINUX: &str = "x86_64-unknown-linux-gnu";

    #[test]
    #[ignore = "cargo metadata fetches the git dependencies of every workspace member"]
    fn release_packages_refuse_slint() {
        let error = crates(&root().join("Cargo.toml"), &["origami-gui"], LINUX).err();
        assert!(error.is_some_and(|e| e.to_string().contains("no Slint")));
    }

    #[test]
    #[ignore = "cargo metadata fetches the git dependencies of every workspace member"]
    fn the_core_bindings_have_a_notices_section() {
        let found = crates(&root().join("Cargo.toml"), &["origami-ffi"], LINUX).unwrap();
        let text = render("t", "", &found, &BTreeSet::new()).unwrap();
        assert!(found.len() > 50 && text.contains("--- "));
    }

    #[test]
    #[ignore = "cargo metadata fetches the git dependencies of every workspace member"]
    fn the_cli_notices_name_every_crate_it_links() {
        let base = fs::read_to_string(root().join("THIRD_PARTY_NOTICES")).unwrap();
        let text = cli(&base, LINUX).unwrap();
        let found = crates(&root().join("Cargo.toml"), &["origami"], LINUX).unwrap();
        for krate in found {
            assert!(
                text.contains(&krate.id()),
                "{} is not in the notices",
                krate.id()
            );
        }
        assert!(text.contains("Go modules (instigator)"));
    }
}
