//! Reads `THIRD_PARTY_NOTICES`: sections between `====` bars, each holding
//! `--- component, component` groups followed by their licence text.

use std::collections::BTreeMap;

/// One group of components and the licence text they share.
#[derive(Debug, Clone)]
pub struct Entry {
    /// The licence's name, from the text: `MIT`, `Apache 2.0 and MIT`.
    pub name: String,
    pub components: Vec<String>,
    pub text: String,
}

/// Sections the apps do not bundle: the Slint viewer is built separately.
const SKIPPED: &[&str] = &["Libraries in origami-gui"];

const NO_FILE: &str = "Crates with no licence file";

/// Needles that name a licence in a text, in the order the name lists them.
/// A `title` needle counts only at the start of a line: the MPL and Apache
/// texts mention the GPL in prose.
const LICENCES: &[(&str, &str, bool)] = &[
    (
        "GCC RUNTIME LIBRARY EXCEPTION",
        "GCC Runtime Library Exception",
        false,
    ),
    (
        "GNU LESSER GENERAL PUBLIC LICENSE",
        "GNU Lesser General Public License",
        true,
    ),
    (
        "GNU GENERAL PUBLIC LICENSE",
        "GNU General Public License",
        true,
    ),
    ("MOZILLA PUBLIC LICENSE", "Mozilla Public License 2.0", true),
    ("SLINT ROYALTY-FREE", "Slint Royalty-free", false),
    ("APACHE LICENSE", "Apache License 2.0", true),
    ("PERMISSION IS HEREBY GRANTED, FREE OF CHARGE", "MIT", false),
    (
        "PERMISSION TO USE, COPY, MODIFY, AND/OR DISTRIBUTE",
        "ISC",
        false,
    ),
    (
        "PERMISSION TO USE, COPY, MODIFY, AND DISTRIBUTE THIS SOFTWARE",
        "ISC",
        false,
    ),
    ("THIS SOFTWARE IS PROVIDED 'AS-IS'", "zlib", false),
    ("UNICODE LICENSE", "Unicode", false),
    ("UNICODE, INC. LICENSE", "Unicode", false),
    ("SIL OPEN FONT LICENSE", "SIL Open Font License", false),
    ("BITSTREAM VERA", "Bitstream Vera", false),
];

/// The licence a text carries, or `None`. A text with several (a crate's MIT
/// and Apache files) is named for all of them.
pub fn kind(text: &str) -> Option<String> {
    let upper = text.to_uppercase();
    let mut names: Vec<&str> = Vec::new();
    for (needle, name, title) in LICENCES {
        let hit = if *title {
            upper.lines().any(|l| l.trim_start().starts_with(needle))
        } else {
            upper.contains(needle)
        };
        if hit && !names.contains(name) {
            names.push(name);
        }
    }
    if upper.contains("REDISTRIBUTION AND USE IN SOURCE AND BINARY FORMS") {
        let three = upper.contains("NEITHER THE NAME") || upper.contains("MAY BE USED TO ENDORSE");
        names.push(if three {
            "BSD 3-Clause"
        } else {
            "BSD 2-Clause"
        });
    }
    if names.is_empty() {
        return None;
    }
    names.sort();
    Some(names.join(" and "))
}

/// The licence an SPDX expression's first term names.
fn spdx_kind(expression: &str) -> String {
    let first = expression
        .split(" OR ")
        .next()
        .unwrap_or(expression)
        .trim_matches(|c| c == '(' || c == ')');
    match first {
        "MIT" => "MIT".into(),
        "Apache-2.0" => "Apache License 2.0".into(),
        "BSD-3-Clause" => "BSD 3-Clause".into(),
        "BSD-2-Clause" => "BSD 2-Clause".into(),
        "ISC" => "ISC".into(),
        "Zlib" => "zlib".into(),
        "MPL-2.0" => "Mozilla Public License 2.0".into(),
        "Unlicense" | "CC0-1.0" => "Public domain".into(),
        other => other.into(),
    }
}

/// An SPDX expression on its own line: `MIT OR Apache-2.0`.
fn is_spdx(line: &str) -> bool {
    !line.is_empty()
        && line.len() < 80
        && line.split_whitespace().all(|w| {
            let w = w.trim_matches(|c| c == '(' || c == ')');
            matches!(
                w,
                "OR" | "AND" | "WITH" | "MIT" | "ISC" | "Zlib" | "Unlicense"
            ) || (w.contains(|c: char| c.is_ascii_digit() || c == '-')
                && w.chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '+')))
        })
}

/// A header with a parenthesis is one component, `libstdc++ (statically
/// linked)`; otherwise a comma-separated list of crates or modules.
pub(crate) fn components(header: &str) -> Vec<String> {
    if header.contains('(') {
        return vec![header.trim().to_string()];
    }
    header
        .split(',')
        .map(|c| c.trim().to_string())
        .filter(|c| !c.is_empty())
        .collect()
}

fn is_bar(line: &str) -> bool {
    line.len() >= 70 && line.bytes().all(|b| b == b'=')
}

/// Every group in the notices, named for the licence its text carries.
pub fn parse_notices(text: &str) -> Vec<Entry> {
    let text = text.replace("\r\n", "\n");
    let lines: Vec<&str> = text.lines().collect();
    let mut sections: Vec<(String, Vec<&str>)> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if is_bar(lines[i]) && i + 2 < lines.len() && is_bar(lines[i + 2]) {
            sections.push((lines[i + 1].trim().to_string(), Vec::new()));
            i += 3;
            continue;
        }
        if let Some((_, body)) = sections.last_mut() {
            body.push(lines[i]);
        }
        i += 1;
    }
    let mut out = Vec::new();
    for (title, body) in sections {
        if SKIPPED.contains(&title.as_str()) {
            continue;
        }
        let mut groups: Vec<(String, Vec<&str>)> = Vec::new();
        for line in &body {
            match line.strip_prefix("--- ") {
                // `--- LLVM Exceptions ... ----` is a rule line inside a licence text.
                Some(header) if !header.trim_end().ends_with("--") => {
                    groups.push((header.trim().to_string(), Vec::new()))
                }
                _ => {
                    if let Some((_, text)) = groups.last_mut() {
                        text.push(line);
                    }
                }
            }
        }
        if groups.is_empty() {
            out.extend(whole_section(&title, &body));
            continue;
        }
        for (header, body) in groups {
            out.extend(group(&header, &body));
        }
    }
    out
}

/// A section with no groups: a licence text with a `Used by:` list, or a
/// description of one library with its text.
fn whole_section(title: &str, body: &[&str]) -> Option<Entry> {
    let mut used: Vec<String> = Vec::new();
    let mut rest: Vec<&str> = Vec::new();
    for line in body {
        match line.strip_prefix("Used by: ") {
            Some(list) => used = components(list),
            None => rest.push(line),
        }
    }
    let text = rest.join("\n").trim().to_string();
    let name = kind(&text)?;
    let components = if used.is_empty() {
        vec![title.to_string()]
    } else {
        used
    };
    Some(Entry {
        name,
        components,
        text,
    })
}

fn group(header: &str, body: &[&str]) -> Vec<Entry> {
    let text = body.join("\n").trim().to_string();
    if header.starts_with(NO_FILE) {
        let mut by: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            if let Some((krate, spdx)) = line
                .rsplit_once(" (")
                .and_then(|(k, s)| Some((k, s.strip_suffix(')')?)))
            {
                by.entry(spdx.to_string())
                    .or_default()
                    .push(krate.to_string());
            }
        }
        return by
            .into_iter()
            .map(|(spdx, components)| Entry {
                name: spdx_kind(&spdx),
                components,
                text: format!(
                    "These crates ship no licence file in their package. Each declares {spdx}."
                ),
            })
            .collect();
    }
    let first = text.lines().next().unwrap_or("").trim();
    let name = kind(&text).unwrap_or_else(|| {
        if is_spdx(first) {
            spdx_kind(first)
        } else {
            "Other licences".into()
        }
    });
    vec![Entry {
        name,
        components: components(header),
        text,
    }]
}
