//! `cargo xtask tokens [--check]`: turns `resources/identity/tokens.toml`
//! into the constants each app compiles, so a shared number lives once.

use crate::archive::Result;
use std::fs;
use std::path::{Path, PathBuf};

enum Val {
    Num(f64),
    Size(f64, f64),
    Colour(u32),
    Colours(Vec<u32>),
}

type Sections = Vec<(String, Vec<(String, Val)>)>;

fn colour(v: &toml::Value) -> Result<u32> {
    let hex = v.as_str().and_then(|s| s.strip_prefix('#'));
    match hex {
        Some(h) if h.len() == 6 => Ok(u32::from_str_radix(h, 16)?),
        _ => Err(format!("{v} is not a #rrggbb colour").into()),
    }
}

fn num(v: Option<&toml::Value>) -> Result<f64> {
    match v {
        Some(toml::Value::Integer(i)) => Ok(*i as f64),
        Some(toml::Value::Float(f)) => Ok(*f),
        other => Err(format!("{other:?} is not a number").into()),
    }
}

fn parse(src: &str) -> Result<Sections> {
    let mut out = Sections::new();
    for (section, body) in src.parse::<toml::Table>()? {
        let toml::Value::Table(body) = body else {
            if section == "version" {
                continue;
            }
            return Err(format!("{section}: expected a [section]").into());
        };
        let mut keys = Vec::new();
        for (key, v) in body {
            let what = format!("{section}.{key}");
            if key
                .chars()
                .any(|c| !matches!(c, 'a'..='z' | '0'..='9' | '_'))
            {
                return Err(format!("{what}: names are lower_snake_case").into());
            }
            let val = match &v {
                toml::Value::Integer(_) | toml::Value::Float(_) => Val::Num(num(Some(&v))?),
                toml::Value::String(_) => Val::Colour(colour(&v)?),
                toml::Value::Table(t) if t.len() == 2 => Val::Size(
                    num(t.get("w")).map_err(|e| format!("{what}: {e}"))?,
                    num(t.get("h"))?,
                ),
                toml::Value::Array(a) if !a.is_empty() => {
                    Val::Colours(a.iter().map(colour).collect::<Result<_>>()?)
                }
                _ => return Err(format!("{what}: unsupported value type").into()),
            };
            keys.push((key, val));
        }
        out.push((section, keys));
    }
    Ok(out)
}

fn pascal(name: &str) -> String {
    name.split('_')
        .map(|w| w[..1].to_uppercase() + &w[1..])
        .collect()
}

fn camel(name: &str) -> String {
    let p = pascal(name);
    p[..1].to_lowercase() + &p[1..]
}

fn hexes(c: &[u32], upper: bool) -> String {
    let each = |c: &u32| {
        if upper {
            format!("#{c:06X}")
        } else {
            format!("0x{c:06x}")
        }
    };
    c.iter().map(each).collect::<Vec<_>>().join(", ")
}

/// The four generated files as (path under the repo root, contents).
fn render(src: &str) -> Result<Vec<(&'static str, String)>> {
    let note =
        "generated from resources/identity/tokens.toml by `cargo xtask tokens`; do not edit.";
    let mut rs = format!("// {note}\n");
    let mut swift = format!("// {note}\n\nimport CoreGraphics\n\nenum Tokens {{\n");
    let mut cs = format!("// {note}\nnamespace Origami.Logic;\n\npublic static class Tokens\n{{\n");
    let mut xaml = format!(
        "<!-- {note} -->\n<ResourceDictionary xmlns=\"http://schemas.microsoft.com/winfx/2006/xaml/presentation\"\n                    xmlns:x=\"http://schemas.microsoft.com/winfx/2006/xaml\">\n"
    );
    for (section, keys) in parse(src)? {
        let sect = pascal(&section);
        rs += &format!("pub mod {section} {{\n");
        swift += &format!("    enum {sect} {{\n");
        cs += &format!("    public static class {sect}\n    {{\n");
        for (key, val) in keys {
            let (up, cc, pc) = (key.to_uppercase(), camel(&key), pascal(&key));
            let xk = |suffix: &str| format!("{sect}.{pc}{suffix}");
            match val {
                Val::Num(n) => {
                    rs += &format!("    pub const {up}: f64 = {n:?};\n");
                    swift += &format!("        static let {cc}: CGFloat = {n}\n");
                    cs += &format!("        public const double {pc} = {n};\n");
                    xaml += &format!("    <x:Double x:Key=\"{}\">{n}</x:Double>\n", xk(""));
                }
                Val::Size(w, h) => {
                    rs += &format!(
                        "    pub const {up}_W: f64 = {w:?};\n    pub const {up}_H: f64 = {h:?};\n"
                    );
                    swift +=
                        &format!("        static let {cc} = CGSize(width: {w}, height: {h})\n");
                    cs += &format!("        public const double {pc}W = {w};\n        public const double {pc}H = {h};\n");
                    xaml += &format!("    <x:Double x:Key=\"{}\">{w}</x:Double>\n", xk("W"));
                    xaml += &format!("    <x:Double x:Key=\"{}\">{h}</x:Double>\n", xk("H"));
                }
                Val::Colour(c) => {
                    rs += &format!("    pub const {up}: u32 = 0x{c:06x};\n");
                    swift += &format!("        static let {cc}: UInt32 = 0x{c:06x}\n");
                    cs += &format!("        public const uint {pc} = 0x{c:06x};\n");
                    xaml += &format!("    <Color x:Key=\"{}\">#{c:06X}</Color>\n", xk(""));
                }
                Val::Colours(list) => {
                    rs += &format!(
                        "    pub const {up}: [u32; {}] = [{}];\n",
                        list.len(),
                        hexes(&list, false)
                    );
                    swift += &format!(
                        "        static let {cc}: [UInt32] = [{}]\n",
                        hexes(&list, false)
                    );
                    cs += &format!(
                        "        public static readonly uint[] {pc} = {{ {} }};\n",
                        hexes(&list, false)
                    );
                    for (i, c) in list.iter().enumerate() {
                        xaml += &format!(
                            "    <Color x:Key=\"{}\">#{c:06X}</Color>\n",
                            xk(&format!(".{i}"))
                        );
                    }
                }
            }
        }
        rs += "}\n";
        swift += "    }\n";
        cs += "    }\n";
    }
    swift += "}\n";
    cs += "}\n";
    xaml += "</ResourceDictionary>\n";
    Ok(vec![
        ("crates/origami-core/src/tokens.rs", rs),
        ("apps/macos/Sources/Generated/Tokens.swift", swift),
        ("apps/windows/Origami.Logic/Generated/Tokens.cs", cs),
        ("apps/windows/Origami.App/Generated/Tokens.xaml", xaml),
    ])
}

/// Writes the generated files, or with `check` lists the stale ones and fails.
pub fn run(root: &Path, check: bool) -> Result<()> {
    let src = fs::read_to_string(root.join("resources/identity/tokens.toml"))?;
    let mut stale: Vec<PathBuf> = Vec::new();
    for (rel, text) in render(&src)? {
        let path = root.join(rel);
        if fs::read_to_string(&path).is_ok_and(|old| old == text) {
            continue;
        }
        if check {
            stale.push(rel.into());
        } else {
            fs::create_dir_all(path.parent().unwrap())?;
            fs::write(&path, text)?;
        }
    }
    if stale.is_empty() {
        return Ok(());
    }
    Err(format!("stale, run `cargo xtask tokens`: {stale:?}").into())
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = "version = 1\n[size]\ncard = { w = 140, h = 104 }\nbar = 34\n[colour]\ntone = [\"#282a30\", \"#1f2126\"]\n";

    fn file<'a>(out: &'a [(&str, String)], name: &str) -> &'a str {
        &out.iter().find(|(p, _)| p.ends_with(name)).unwrap().1
    }

    #[test]
    fn renders_every_language() {
        let out = render(FIXTURE).unwrap();
        let rs = file(&out, "tokens.rs");
        assert!(rs.contains("pub mod size {\n    pub const BAR: f64 = 34.0;\n    pub const CARD_W: f64 = 140.0;\n    pub const CARD_H: f64 = 104.0;\n}\n"));
        assert!(rs.contains("pub const TONE: [u32; 2] = [0x282a30, 0x1f2126];"));
        let swift = file(&out, "Tokens.swift");
        assert!(swift.contains("static let bar: CGFloat = 34\n"));
        assert!(swift.contains("static let card = CGSize(width: 140, height: 104)\n"));
        assert!(swift.contains("static let tone: [UInt32] = [0x282a30, 0x1f2126]\n"));
        let cs = file(&out, "Tokens.cs");
        assert!(cs.contains("public const double CardW = 140;\n"));
        assert!(cs.contains("public static readonly uint[] Tone = { 0x282a30, 0x1f2126 };\n"));
        let xaml = file(&out, "Tokens.xaml");
        assert!(xaml.contains("<x:Double x:Key=\"Size.CardH\">104</x:Double>\n"));
        assert!(xaml.contains("<Color x:Key=\"Colour.Tone.1\">#1F2126</Color>\n"));
    }

    #[test]
    fn rejects_values_it_cannot_emit() {
        assert!(render("[size]\nx = true\n").is_err());
        assert!(render("[colour]\nx = \"red\"\n").is_err());
        assert!(render("[size]\nBadName = 1\n").is_err());
    }
}
