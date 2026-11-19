//! `runtime.lock`: the QEMU and Instigator releases the product runs against.

use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;

pub const PLATFORMS: [&str; 6] = [
    "linux-x86_64",
    "linux-arm64",
    "macos-arm64",
    "macos-x86_64",
    "windows-x86_64",
    "windows-arm64",
];
pub const QEMU_REPO: &str = "jamesbraid/qemu-sgi-origami";
pub const INSTIGATOR_REPO: &str = "jamesbraid/instigator";

#[derive(Debug, Deserialize, PartialEq)]
pub struct Lock {
    pub format: u32,
    pub qemu: Pin,
    pub instigator: Pin,
}

#[derive(Debug, Deserialize, PartialEq)]
pub struct Pin {
    pub tag: String,
    pub platforms: BTreeMap<String, Asset>,
}

/// A release archive (`url` and `sha256`), or for a platform the project
/// publishes no archive for, the `commit` of its source to build.
#[derive(Debug, Deserialize, PartialEq)]
pub struct Asset {
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub sha256: String,
    #[serde(default)]
    pub commit: Option<String>,
}

impl Asset {
    pub fn release(url: String, sha256: String) -> Asset {
        Asset {
            url,
            sha256,
            commit: None,
        }
    }

    /// What identifies the bytes: the archive's hash or the source commit.
    pub fn pin(&self) -> &str {
        self.commit.as_deref().unwrap_or(&self.sha256)
    }
}

impl Lock {
    pub fn read(path: &Path) -> Result<Lock, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
        let lock: Lock = toml::from_str(&text)
            .map_err(|error| format!("cannot parse {}: {error}", path.display()))?;
        if lock.format != 1 {
            return Err(format!(
                "{} has format {}, want 1",
                path.display(),
                lock.format
            ));
        }
        for (name, pin) in [("qemu", &lock.qemu), ("instigator", &lock.instigator)] {
            for platform in PLATFORMS {
                let Some(asset) = pin.platforms.get(platform) else {
                    return Err(format!("{name} pin has no {platform} entry"));
                };
                let archive = !asset.url.is_empty() && !asset.sha256.is_empty();
                if !archive && asset.commit.is_none() {
                    return Err(format!(
                        "{name} {platform} needs a url and sha256, or a commit"
                    ));
                }
            }
        }
        Ok(lock)
    }
}

/// The platform this build runs on, named as the release archives are.
pub fn host_platform() -> Result<&'static str, String> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => Ok("linux-x86_64"),
        ("linux", "aarch64") => Ok("linux-arm64"),
        ("macos", "aarch64") => Ok("macos-arm64"),
        ("macos", "x86_64") => Ok("macos-x86_64"),
        ("windows", "x86_64") => Ok("windows-x86_64"),
        ("windows", "aarch64") => Ok("windows-arm64"),
        (os, arch) => Err(format!("no product platform for {os} on {arch}")),
    }
}

pub fn archive_extension(platform: &str) -> &'static str {
    if platform.starts_with("windows") {
        "zip"
    } else {
        "tar.gz"
    }
}

/// The QEMU release asset for a tag: `<tag>-<platform>.<ext>`, with the
/// tag being `sgi-origami-<version>-<n>`.
pub fn qemu_asset_name(tag: &str, platform: &str) -> String {
    format!("{tag}-{platform}.{}", archive_extension(platform))
}

pub fn instigator_asset_name(tag: &str, platform: &str) -> Result<String, String> {
    let version = tag.strip_prefix('v').unwrap_or(tag);
    let target = match platform {
        "linux-x86_64" => "linux_amd64",
        "linux-arm64" => "linux_arm64",
        "macos-arm64" => "darwin_arm64",
        "macos-x86_64" => "darwin_amd64",
        "windows-x86_64" => "windows_amd64",
        "windows-arm64" => "windows_arm64",
        other => return Err(format!("unknown platform {other}")),
    };
    Ok(format!(
        "instigator_{version}_{target}.{}",
        archive_extension(platform)
    ))
}

pub fn download_url(repo: &str, tag: &str, asset: &str) -> String {
    format!("https://github.com/{repo}/releases/download/{tag}/{asset}")
}

const HEADER: &str = "\
# The QEMU and Instigator releases this product runs against. `cargo xtask
# bump qemu TAG` and `cargo xtask bump instigator TAG` rewrite this file from
# the release's published checksums; do not edit the hashes by hand.
";

fn table(out: &mut String, name: &str, pin: &Pin) {
    out.push_str(&format!(
        "\n[{name}]\ntag = \"{}\"\n[{name}.platforms]\n",
        pin.tag
    ));
    for (platform, asset) in &pin.platforms {
        match &asset.commit {
            Some(commit) => {
                out.push_str(&format!("{platform:<14} = {{ commit = \"{commit}\" }}\n"))
            }
            None => out.push_str(&format!(
                "{platform:<14} = {{ url = \"{}\", sha256 = \"{}\" }}\n",
                asset.url, asset.sha256
            )),
        }
    }
}

/// The file's text.
pub fn render(lock: &Lock) -> String {
    let mut out = String::from(HEADER);
    out.push_str("\nformat = 1\n");
    table(&mut out, "qemu", &lock.qemu);
    table(&mut out, "instigator", &lock.instigator);
    out
}

/// Parses `sha256sum` output: `<hex>  <name>` per line, `*` before binary names.
pub fn parse_checksums(text: &str) -> BTreeMap<String, String> {
    text.lines()
        .filter_map(|line| {
            let (hash, name) = line.split_once(char::is_whitespace)?;
            let name = name.trim().trim_start_matches('*');
            (hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()) && !name.is_empty())
                .then(|| (name.to_string(), hash.to_ascii_lowercase()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pin(tag: &str, hash: &str) -> Pin {
        Pin {
            tag: tag.into(),
            platforms: PLATFORMS
                .iter()
                .map(|p| {
                    (
                        p.to_string(),
                        Asset::release(format!("https://example.invalid/{tag}/{p}"), hash.into()),
                    )
                })
                .collect(),
        }
    }

    #[test]
    fn names_follow_the_release_conventions() {
        let tag = "sgi-origami-11.1.50-1";
        assert_eq!(
            qemu_asset_name(tag, "linux-x86_64"),
            "sgi-origami-11.1.50-1-linux-x86_64.tar.gz"
        );
        assert_eq!(
            qemu_asset_name(tag, "windows-x86_64"),
            "sgi-origami-11.1.50-1-windows-x86_64.zip"
        );
        assert_eq!(
            qemu_asset_name(tag, "macos-x86_64"),
            "sgi-origami-11.1.50-1-macos-x86_64.tar.gz"
        );
        assert_eq!(
            qemu_asset_name(tag, "windows-arm64"),
            "sgi-origami-11.1.50-1-windows-arm64.zip"
        );
        assert_eq!(
            instigator_asset_name("v0.5.0", "macos-arm64").unwrap(),
            "instigator_0.5.0_darwin_arm64.tar.gz"
        );
        assert_eq!(
            instigator_asset_name("v0.5.0", "windows-x86_64").unwrap(),
            "instigator_0.5.0_windows_amd64.zip"
        );
        assert_eq!(
            instigator_asset_name("v0.5.0", "macos-x86_64").unwrap(),
            "instigator_0.5.0_darwin_amd64.tar.gz"
        );
        assert_eq!(
            instigator_asset_name("v0.5.0", "linux-arm64").unwrap(),
            "instigator_0.5.0_linux_arm64.tar.gz"
        );
    }

    #[test]
    fn a_rendered_lock_reads_back() {
        let lock = Lock {
            format: 1,
            qemu: pin("sgi-origami-11.1.50-1", &"cd".repeat(32)),
            instigator: pin("v0.5.0", &"ab".repeat(32)),
        };
        let text = render(&lock);
        let path = std::env::temp_dir().join(format!("xtask-lock-{}", std::process::id()));
        std::fs::write(&path, &text).unwrap();
        let back = Lock::read(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert_eq!(back, lock);
    }

    #[test]
    fn a_source_pinned_platform_reads_back() {
        let mut lock = Lock {
            format: 1,
            qemu: pin("sgi-origami-11.1.50-1", &"cd".repeat(32)),
            instigator: pin("v0.5.0", &"ab".repeat(32)),
        };
        let commit = "793926e3b1f784e26f9c14f06cc1a587a7c07507";
        lock.instigator.platforms.insert(
            "windows-arm64".into(),
            Asset {
                url: String::new(),
                sha256: String::new(),
                commit: Some(commit.into()),
            },
        );
        let text = render(&lock);
        assert!(text.contains(&format!("windows-arm64  = {{ commit = \"{commit}\" }}")));
        let path = std::env::temp_dir().join(format!("xtask-lock-src-{}", std::process::id()));
        std::fs::write(&path, &text).unwrap();
        let back = Lock::read(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert_eq!(back, lock);
    }

    #[test]
    fn checksums_parse_both_sha256sum_spellings() {
        let hash = "0f".repeat(32);
        let sums = parse_checksums(&format!("{hash}  a.tar.gz\n{hash} *b.zip\nnot a hash  c\n"));
        assert_eq!(sums.len(), 2);
        assert_eq!(sums["a.tar.gz"], hash);
        assert_eq!(sums["b.zip"], hash);
    }
}
