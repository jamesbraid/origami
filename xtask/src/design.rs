//! The design reference and its guard, run by `cargo xtask tokens [--check]`:
//! `design/index.html` carries the tokens as data, and the apps may not add
//! colour literals beyond those listed in `design/colour-baseline.txt`.

use crate::archive::Result;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

const PAGE: &str = "design/index.html";
const BASELINE: &str = "design/colour-baseline.txt";
const OPEN: &str = "<script id=\"design-data\" type=\"application/json\">";
const CLOSE: &str = "</script>";
const SCANNED: &[&str] = &[
    "apps/macos/Sources",
    "apps/windows",
    "crates/origami-gtk/src",
];
const SKIPPED: &[&str] = &["Generated", "Selftest", "bin", "obj", "ci"];
const EXTENSIONS: &[&str] = &["swift", "cs", "xaml", "rs", "css"];
const HEADER: &str =
    "# Colour literals the apps held when the design check began: path, literal, count.\n\
# `cargo xtask tokens` removes entries as code stops using them. Add one only by hand,\n\
# with a reason in the commit, for a colour no platform colour or token can give.\n";

/// `(path, literal) -> count`, with paths relative to the repository root.
type Found = BTreeMap<(String, String), usize>;

/// The data block the page renders from: the tokens, the platform reference,
/// the chassis colours from the stills manifest and the granite tile.
fn data(root: &Path) -> Result<String> {
    let read = |rel: &str| fs::read_to_string(root.join(rel));
    let tokens: toml::Table = read("resources/identity/tokens.toml")?.parse()?;
    let platform: toml::Table = read("design/platform.toml")?.parse()?;
    let manifest: serde_json::Value =
        serde_json::from_str(&read("resources/identity/stills/manifest.json")?)?;
    let chassis: BTreeMap<String, serde_json::Value> = manifest["stills"]
        .as_object()
        .ok_or("the stills manifest has no stills")?
        .iter()
        .filter_map(|(k, v)| {
            Some((
                k.clone(),
                serde_json::json!({ "name": v["name"], "colour": v.get("colour")? }),
            ))
        })
        .collect();
    let tile = fs::read(root.join("resources/identity/granite-tile.png"))?;
    let json = serde_json::to_string_pretty(&serde_json::json!({
        "tokens": tokens,
        "platform": platform,
        "chassis": chassis,
        "granite_png": base64(&tile),
    }))?;
    // A "</" inside the JSON would end the script element early.
    Ok(json.replace("</", "<\\/"))
}

fn base64(bytes: &[u8]) -> String {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, b)| n | (*b as u32) << (16 - 8 * i));
        for i in 0..4 {
            out.push(if i <= chunk.len() {
                ABC[(n >> (18 - 6 * i)) as usize & 63] as char
            } else {
                '='
            });
        }
    }
    out
}

/// The page with its data block replaced.
fn page(old: &str, data: &str) -> Result<String> {
    let start = old
        .find(OPEN)
        .ok_or("design/index.html has no design-data block")?
        + OPEN.len();
    let end = start
        + old[start..]
            .find(CLOSE)
            .ok_or("the design-data block is not closed")?;
    Ok(format!("{}\n{data}\n{}", &old[..start], &old[end..]))
}

/// Colour literals in one source file: `0xRRGGBB`, `#RRGGBB`, `#AARRGGBB`
/// and `Color.FromArgb(...)`.
fn literals(text: &str) -> Vec<String> {
    let b = text.as_bytes();
    let word = |i: usize| i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_');
    let hex_run = |i: usize| b[i..].iter().take_while(|c| c.is_ascii_hexdigit()).count();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let before = i > 0 && word(i - 1);
        if !before && b[i..].starts_with(b"0x") {
            let n = hex_run(i + 2);
            if n == 6 && !word(i + 2 + n) {
                out.push(text[i..i + 8].to_ascii_lowercase());
            }
            i += 2 + n;
            continue;
        }
        if !before && b[i] == b'#' {
            let n = hex_run(i + 1);
            if (n == 6 || n == 8) && !word(i + 1 + n) {
                out.push(text[i..i + 1 + n].to_ascii_lowercase());
            }
            i += 1 + n;
            continue;
        }
        if b[i..].starts_with(b"Color.FromArgb(") {
            let end = text[i..].find(')').map_or(b.len(), |e| i + e + 1);
            out.push(text[i..end].split_whitespace().collect());
            i = end;
            continue;
        }
        i += 1;
    }
    out
}

fn scan(root: &Path) -> Result<Found> {
    fn walk(root: &Path, dir: &Path, found: &mut Found) -> Result<()> {
        let Ok(entries) = fs::read_dir(dir) else {
            return Ok(());
        };
        for entry in entries {
            let path = entry?.path();
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            if path.is_dir() {
                if !SKIPPED.contains(&name.as_ref()) && !name.ends_with("Tests") && name != "tests"
                {
                    walk(root, &path, found)?;
                }
                continue;
            }
            let ext = path.extension().unwrap_or_default().to_string_lossy();
            if !EXTENSIONS.contains(&ext.as_ref()) || name.to_lowercase().contains("selftest") {
                continue;
            }
            let rel = path
                .strip_prefix(root)?
                .to_string_lossy()
                .replace('\\', "/");
            for lit in literals(&fs::read_to_string(&path)?) {
                *found.entry((rel.clone(), lit)).or_default() += 1;
            }
        }
        Ok(())
    }
    let mut found = Found::new();
    for dir in SCANNED {
        walk(root, &root.join(dir), &mut found)?;
    }
    Ok(found)
}

fn parse_baseline(text: &str) -> Result<Found> {
    let mut out = Found::new();
    for line in text
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
    {
        let mut parts = line.split('\t');
        match (
            parts.next(),
            parts.next(),
            parts.next().map(str::parse::<usize>),
        ) {
            (Some(p), Some(l), Some(Ok(n))) => out.insert((p.into(), l.into()), n),
            _ => return Err(format!("{BASELINE}: bad line {line:?}").into()),
        };
    }
    Ok(out)
}

fn render_baseline(header: &str, entries: &Found) -> String {
    let mut out = header.to_string();
    for ((path, lit), n) in entries {
        out += &format!("{path}\t{lit}\t{n}\n");
    }
    out
}

/// New literals (more than the baseline allows) and baseline entries that
/// are now higher than what the code holds.
fn compare(found: &Found, baseline: &Found) -> (Vec<String>, Found) {
    let added = found
        .iter()
        .filter(|(k, n)| baseline.get(k).is_none_or(|b| *n > b))
        .map(|((p, l), n)| {
            format!(
                "{p}: {l} ({n}, allowed {})",
                baseline.get(&(p.clone(), l.clone())).unwrap_or(&0)
            )
        })
        .collect();
    let pruned = baseline
        .iter()
        .filter_map(|(k, b)| {
            let n = found.get(k).copied().unwrap_or(0).min(*b);
            (n > 0).then(|| (k.clone(), n))
        })
        .collect();
    (added, pruned)
}

/// Refreshes the page's data and shrinks the baseline, or with `check`
/// reports what is stale or new. The baseline only grows by hand.
pub fn run(root: &Path, check: bool) -> Result<Vec<String>> {
    let mut problems = Vec::new();
    let path = root.join(PAGE);
    let old = fs::read_to_string(&path)?;
    let new = page(&old, &data(root)?)?;
    if new != old {
        if check {
            problems.push(format!("{PAGE} is stale, run `cargo xtask tokens`"));
        } else {
            fs::write(&path, new)?;
        }
    }

    let found = scan(root)?;
    let base_path = root.join(BASELINE);
    if !check && !base_path.exists() {
        // The first run records what the apps hold today; after that the list only shrinks.
        fs::write(&base_path, render_baseline(HEADER, &found))?;
    }
    let text = fs::read_to_string(&base_path).unwrap_or_default();
    let header: String = text
        .lines()
        .take_while(|l| l.starts_with('#'))
        .map(|l| format!("{l}\n"))
        .collect();
    let baseline = parse_baseline(&text)?;
    let (added, pruned) = compare(&found, &baseline);
    for a in added {
        problems.push(format!(
            "hard-coded colour {a}: use a platform colour or a token (design/README.md); \
             a deliberate exception goes in {BASELINE} by hand"
        ));
    }
    if pruned != baseline {
        if check {
            problems.push(format!(
                "{BASELINE} lists colours the code no longer has, run `cargo xtask tokens`"
            ));
        } else {
            fs::write(&base_path, render_baseline(&header, &pruned))?;
        }
    }
    Ok(problems)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_colour_literals_and_nothing_else() {
        let src = "let a = 0x2E354E; css(\"#33d17a\"); x = 0x12345678; y = 0x1234; \
                   #[derive(Debug)] <Color>#FF112233</Color> id0x123456 Color.FromArgb(255, 0x2E, 0x8B, 0x57)";
        assert_eq!(
            literals(src),
            [
                "0x2e354e",
                "#33d17a",
                "#ff112233",
                "Color.FromArgb(255,0x2E,0x8B,0x57)"
            ]
        );
    }

    #[test]
    fn new_literals_fail_and_removed_ones_shrink_the_baseline() {
        let k = |p: &str, l: &str| (p.to_string(), l.to_string());
        let baseline = Found::from([(k("a.rs", "0x111111"), 2), (k("a.rs", "0x222222"), 1)]);
        let found = Found::from([(k("a.rs", "0x111111"), 1), (k("b.rs", "#333333"), 1)]);
        let (added, pruned) = compare(&found, &baseline);
        assert_eq!(added, ["b.rs: #333333 (1, allowed 0)"]);
        assert_eq!(pruned, Found::from([(k("a.rs", "0x111111"), 1)]));
    }

    #[test]
    fn replaces_only_the_data_block() {
        let old = format!("<p>keep</p>{OPEN}\nold\n{CLOSE}<p>tail</p>");
        assert_eq!(
            page(&old, "{}").unwrap(),
            format!("<p>keep</p>{OPEN}\n{{}}\n{CLOSE}<p>tail</p>")
        );
    }

    #[test]
    fn encodes_base64() {
        assert_eq!(base64(b"Man"), "TWFu");
        assert_eq!(base64(b"Ma"), "TWE=");
        assert_eq!(base64(b"M"), "TQ==");
    }
}
