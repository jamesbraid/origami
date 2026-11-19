//! What the apps' About credits: people, projects, photographs, artwork and
//! licence texts, built from the repository's files into one JSON document.

mod notices;

use serde::{Deserialize, Serialize};

pub use notices::{parse_notices, Entry};

const CREDITS: &str = include_str!("../../../resources/credits/credits.toml");
const SCENES: &str = include_str!("../../../resources/identity/scenes/scenes.json");
const RUNTIME_LOCK: &str = include_str!("../../../runtime.lock");
const NOTICES: &str = include_str!(concat!(env!("OUT_DIR"), "/notices.txt"));
const ORIGAMI_LICENCE: &str = include_str!("../../../LICENSE");
const QEMU_REPO: &str = "https://github.com/jamesbraid/qemu-sgi-origami";

/// Licence texts kept beside `credits.toml`, by the `file` it names.
const TEXTS: &[(&str, &str)] = &[
    ("../../LICENSE", ORIGAMI_LICENCE),
    (
        "instigator.LICENSE",
        include_str!("../../../resources/credits/instigator.LICENSE"),
    ),
    (
        "iris.LICENSE",
        include_str!("../../../resources/credits/iris.LICENSE"),
    ),
    (
        "GPL-2.0.txt",
        include_str!("../../../resources/credits/GPL-2.0.txt"),
    ),
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Credits {
    /// `© 2026 James Braid`, from the repository's LICENSE.
    pub copyright: String,
    pub licence_name: String,
    pub project_url: String,
    pub trademark: String,
    pub built_on: Vec<Project>,
    pub thanks: Vec<Thanks>,
    pub photographs: Vec<Photograph>,
    pub artwork: Vec<Artwork>,
    pub qemu: QemuSource,
    pub licences: Vec<Licence>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub name: String,
    pub by: String,
    pub url: String,
    pub licence: String,
    pub note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Thanks {
    pub name: String,
    #[serde(rename = "for")]
    pub reason: String,
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Photograph {
    pub title: String,
    pub author: String,
    pub year: String,
    pub licence: String,
    pub licence_url: String,
    /// The photograph's page on Wikimedia Commons.
    pub source: String,
    /// What was done to it, as the credit line says.
    pub changes: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Artwork {
    pub name: String,
    pub from: String,
    #[serde(default)]
    pub url: String,
}

/// Where the source of the bundled QEMU build is.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QemuSource {
    pub tag: String,
    pub url: String,
    pub archive_url: String,
    /// The written statement that the source is available.
    pub note: String,
}

/// One licence and every bundled component under it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Licence {
    pub name: String,
    pub entries: Vec<LicenceEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LicenceEntry {
    pub components: Vec<String>,
    pub text: String,
}

#[derive(Deserialize)]
struct Source {
    project_url: String,
    trademark: String,
    built_on: Vec<Project>,
    thanks: Vec<Thanks>,
    artwork: Vec<Artwork>,
    licence_text: Vec<SourceText>,
}

#[derive(Deserialize)]
struct SourceText {
    licence: String,
    components: Vec<String>,
    file: String,
}

/// The credits this build ships.
pub fn credits() -> Result<Credits, String> {
    build(CREDITS, SCENES, RUNTIME_LOCK, NOTICES)
}

/// The credits as the JSON document the apps read.
pub fn credits_json() -> String {
    match credits().and_then(|c| serde_json::to_string(&c).map_err(|e| e.to_string())) {
        Ok(json) => json,
        Err(e) => format!("{{\"error\":{}}}", serde_json::Value::String(e)),
    }
}

/// The QEMU tag in `runtime.lock`.
pub fn qemu_tag() -> Result<String, String> {
    qemu_tag_of(RUNTIME_LOCK)
}

fn qemu_tag_of(lock: &str) -> Result<String, String> {
    let lock: toml::Table = lock.parse().map_err(|e| format!("runtime.lock: {e}"))?;
    lock.get("qemu")
        .and_then(|q| q.get("tag"))
        .and_then(|t| t.as_str())
        .map(str::to_string)
        .ok_or_else(|| "runtime.lock has no qemu.tag".into())
}

/// Beside QEMU's licence text in every package: the tag and where its source is.
pub fn qemu_source_file() -> Result<String, String> {
    let q = qemu_source(&qemu_tag()?);
    Ok(format!(
        "The QEMU in this package is under the GNU General Public License, version 2\n\
         (qemu.COPYING). Its corresponding source is at the release tag {}:\n\n  {}\n  {}\n",
        q.tag, q.url, q.archive_url,
    ))
}

fn qemu_source(tag: &str) -> QemuSource {
    QemuSource {
        tag: tag.into(),
        url: format!("{QEMU_REPO}/tree/{tag}"),
        archive_url: format!("{QEMU_REPO}/archive/refs/tags/{tag}.tar.gz"),
        note: format!(
            "QEMU is licensed under the GNU General Public License, version 2. \
             The source of this build is available at the tag {tag}. The libraries built into QEMU \
             (GLib, pixman, SDL 2 and others) have their licences in the package's share/origami/licenses folder."
        ),
    }
}

/// The licence text of a Creative Commons licence name, as a link.
fn cc_url(licence: &str) -> Option<String> {
    if licence == "CC0" {
        return Some("https://creativecommons.org/publicdomain/zero/1.0/".into());
    }
    let version = licence.strip_prefix("CC BY ")?;
    Some(format!(
        "https://creativecommons.org/licenses/by/{version}/"
    ))
}

/// `Roy Luck, 2011, CC BY 2.0, via Wikimedia Commons; cropped and mirror-tiled`
fn photograph(scene: &serde_json::Value) -> Result<Photograph, String> {
    let text = |key: &str| {
        scene[key]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| format!("scenes.json: {scene} has no {key}"))
    };
    let (title, credit, source) = (text("title")?, text("credit")?, text("source")?);
    let (line, changes) = credit.split_once("; ").unwrap_or((&credit, ""));
    let head = line.split(", via ").next().unwrap_or(line);
    let mut parts = head.rsplitn(3, ", ");
    let (licence, year, author) = (parts.next(), parts.next(), parts.next());
    let (Some(licence), Some(year), Some(author)) = (licence, year, author) else {
        return Err(format!(
            "{title}: the credit line {credit:?} is not author, year, licence"
        ));
    };
    if year.len() != 4 || !year.bytes().all(|b| b.is_ascii_digit()) {
        return Err(format!("{title}: {year:?} is not a year"));
    }
    let licence_url = cc_url(licence).ok_or_else(|| format!("{title}: no link for {licence}"))?;
    let mut changes = changes.to_string();
    if let Some(first) = changes.get_mut(..1) {
        first.make_ascii_uppercase();
    }
    Ok(Photograph {
        title,
        author: author.into(),
        year: year.into(),
        licence: licence.into(),
        licence_url,
        source,
        changes,
    })
}

/// `© 2026 James Braid`, from the line `Copyright (c) 2026, James Braid`.
fn copyright(licence: &str) -> Result<String, String> {
    let line = licence
        .lines()
        .find_map(|l| l.strip_prefix("Copyright (c) "))
        .ok_or("LICENSE has no copyright line")?;
    Ok(format!("© {}", line.replacen(", ", " ", 1)))
}

fn normalised(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Licences the apps list first, in this order.
const FIRST: &[&str] = &["GNU General Public License", "BSD 3-Clause"];

fn licences(source: &Source, notices: &str) -> Result<Vec<Licence>, String> {
    let mut found: Vec<Entry> = Vec::new();
    for text in &source.licence_text {
        let body = TEXTS
            .iter()
            .find(|(f, _)| *f == text.file)
            .map(|(_, t)| t.trim().to_string())
            .ok_or_else(|| format!("credits.toml: no text for {}", text.file))?;
        found.push(Entry {
            name: text.licence.clone(),
            components: text.components.clone(),
            text: body,
        });
    }
    found.extend(parse_notices(notices));
    let mut groups: Vec<Licence> = Vec::new();
    for entry in found {
        let group = match groups.iter().position(|g| g.name == entry.name) {
            Some(i) => &mut groups[i],
            None => {
                groups.push(Licence {
                    name: entry.name.clone(),
                    entries: Vec::new(),
                });
                groups.last_mut().unwrap()
            }
        };
        let key = normalised(&entry.text);
        match group
            .entries
            .iter_mut()
            .find(|e| normalised(&e.text) == key)
        {
            Some(e) => {
                for c in entry.components {
                    if !e.components.contains(&c) {
                        e.components.push(c);
                    }
                }
            }
            None => group.entries.push(LicenceEntry {
                components: entry.components,
                text: entry.text,
            }),
        }
    }
    for group in &mut groups {
        for e in &mut group.entries {
            e.components.sort_by_key(|c| c.to_lowercase());
        }
        group
            .entries
            .sort_by_key(|e| std::cmp::Reverse(e.components.len()));
    }
    groups.sort_by_key(|g| {
        (
            FIRST
                .iter()
                .position(|f| *f == g.name)
                .unwrap_or(FIRST.len()),
            g.name.to_lowercase(),
        )
    });
    Ok(groups)
}

/// The credits from their four sources.
pub fn build(
    credits: &str,
    scenes: &str,
    runtime_lock: &str,
    notices: &str,
) -> Result<Credits, String> {
    let source: Source = toml::from_str(credits).map_err(|e| format!("credits.toml: {e}"))?;
    let scenes: serde_json::Value =
        serde_json::from_str(scenes).map_err(|e| format!("scenes.json: {e}"))?;
    let photographs = scenes["scenes"]
        .as_array()
        .ok_or("scenes.json lists no scenes")?
        .iter()
        .map(photograph)
        .collect::<Result<Vec<_>, _>>()?;
    let licences = licences(&source, notices)?;
    Ok(Credits {
        copyright: copyright(ORIGAMI_LICENCE)?,
        licence_name: "BSD 3-Clause licence".into(),
        project_url: source.project_url,
        trademark: source.trademark,
        built_on: source.built_on,
        thanks: source.thanks,
        photographs,
        artwork: source.artwork,
        qemu: qemu_source(&qemu_tag_of(runtime_lock)?),
        licences,
    })
}

#[cfg(test)]
mod tests;
