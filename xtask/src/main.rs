//! Build tasks for the product: fetch the pinned QEMU and Instigator
//! releases, run the tests against them, and lay out the release archive.
//!
//! ```text
//! cargo xtask runtime [--platform P] [--qemu-archive FILE]
//! cargo xtask test    [--qemu-archive FILE]
//! cargo xtask dist    [--qemu-archive FILE] [--out DIR]
//! cargo xtask bump    qemu|instigator TAG
//! ```

mod archive;
mod lock;
mod notices;

use archive::Result;
use lock::{Asset, Lock, Pin, PLATFORMS};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

fn target_dir() -> PathBuf {
    std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| root().join("target"))
}

fn exe(name: &str, platform: &str) -> String {
    if platform.starts_with("windows") {
        format!("{name}.exe")
    } else {
        name.into()
    }
}

#[derive(Default)]
struct Options {
    platform: Option<String>,
    qemu_archive: Option<PathBuf>,
    out: Option<PathBuf>,
    positional: Vec<String>,
}

fn parse(args: &[String]) -> Result<Options> {
    let mut options = Options::default();
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        let mut value = |name: &str| {
            args.next()
                .cloned()
                .ok_or_else(|| format!("{name} needs a value"))
        };
        match arg.as_str() {
            "--platform" => options.platform = Some(value("--platform")?),
            "--qemu-archive" => options.qemu_archive = Some(value("--qemu-archive")?.into()),
            "--out" => options.out = Some(value("--out")?.into()),
            flag if flag.starts_with("--") => return Err(format!("unknown option {flag}").into()),
            _ => options.positional.push(arg.clone()),
        }
    }
    if let Some(platform) = &options.platform {
        if !PLATFORMS.contains(&platform.as_str()) {
            return Err(format!("unknown platform {platform}; one of {PLATFORMS:?}").into());
        }
    }
    Ok(options)
}

/// A directory's files, with their contents, for comparing two trees.
fn contents(dir: &Path) -> Result<BTreeMap<PathBuf, Vec<u8>>> {
    let mut files = BTreeMap::new();
    for rel in archive::list(dir)? {
        if dir.join(&rel).is_file() {
            files.insert(rel.clone(), fs::read(dir.join(&rel))?);
        }
    }
    Ok(files)
}

fn configs_dir() -> PathBuf {
    root().join("resources/instigator/configs")
}

/// Stops when the install configurations compiled into the product differ
/// from the ones the pinned Instigator carries, because the recipes would
/// then describe a different Instigator than the one that runs them.
fn check_configs(instigator: &Path) -> Result<()> {
    if contents(&configs_dir())? != contents(&instigator.join("configs"))? {
        return Err(format!(
            "resources/instigator/configs differs from the configs in the pinned Instigator; \
             run `cargo xtask bump instigator {}` to refresh them",
            Lock::read(&root().join("runtime.lock"))?.instigator.tag
        )
        .into());
    }
    Ok(())
}

/// Where a QEMU archive keeps its keymaps. QEMU moved them under `share/qemu`
/// in sgi-origami 11.1.90 (upstream e27ef99da0a); the pinned 11.1.50-1 still
/// has them at `share/keymaps`. Drop the old path once the pin moves past it.
fn keymaps_dir(qemu_dir: &Path) -> Result<PathBuf> {
    for rel in ["share/qemu/keymaps", "share/keymaps"] {
        let path = qemu_dir.join(rel);
        if path.is_dir() {
            return Ok(path);
        }
    }
    Err("the QEMU archive has no share/qemu/keymaps or share/keymaps".into())
}

/// Downloads the pinned releases and lays them out as the product expects
/// them under `target/runtime/<platform>/`, returning that directory:
///
/// ```text
/// bin/instigator              libexec/origami/qemu-*, DLLs
/// share/origami/qemu/keymaps  share/origami/licenses/
/// ```
fn runtime(platform: &str, qemu_archive: Option<&Path>) -> Result<PathBuf> {
    let lock = Lock::read(&root().join("runtime.lock"))?;
    let base = target_dir().join("runtime");
    let cache = base.join("downloads");
    let qemu_file = match qemu_archive {
        Some(path) => path.to_path_buf(),
        None => {
            let asset = &lock.qemu.platforms[platform];
            archive::fetch(&asset.url, &asset.sha256, &cache)?
        }
    };
    let instigator = &lock.instigator.platforms[platform];
    let instigator_file = archive::fetch(&instigator.url, &instigator.sha256, &cache)?;

    let dir = base.join(platform);
    let stamp = format!(
        "qemu {} {}\ninstigator {} {}\n",
        lock.qemu.tag,
        archive::sha256_file(&qemu_file)?,
        lock.instigator.tag,
        instigator.sha256
    );
    let stamp_file = dir.join("stamp");
    if fs::read_to_string(&stamp_file).is_ok_and(|s| s == stamp) {
        return Ok(dir.join("tree"));
    }

    let raw = dir.join("raw");
    let (qemu_dir, instigator_dir, tree) =
        (raw.join("qemu"), raw.join("instigator"), dir.join("tree"));
    archive::unpack(&qemu_file, &qemu_dir)?;
    archive::unpack(&instigator_file, &instigator_dir)?;
    check_configs(&instigator_dir)?;

    for required in ["qemu-system-mips64", "qemu-img", "qemu-sgi-machine-init"] {
        let path = qemu_dir.join("bin").join(exe(required, platform));
        if !path.is_file() {
            return Err(format!("the QEMU archive has no bin/{}", exe(required, platform)).into());
        }
    }
    let keymaps = keymaps_dir(&qemu_dir)?;
    if tree.exists() {
        fs::remove_dir_all(&tree)?;
    }
    archive::copy_tree(&qemu_dir.join("bin"), &tree.join("libexec/origami"))?;
    archive::copy_tree(&keymaps, &tree.join("share/origami/qemu/keymaps"))?;
    let licenses = tree.join("share/origami/licenses");
    if qemu_dir.join("licenses").is_dir() {
        archive::copy_tree(&qemu_dir.join("licenses"), &licenses)?;
    }
    let name = exe("instigator", platform);
    archive::copy_file(&instigator_dir.join(&name), &tree.join("bin").join(&name))?;
    for file in ["LICENSE", "THIRD_PARTY_NOTICES"] {
        if instigator_dir.join(file).is_file() {
            archive::copy_file(
                &instigator_dir.join(file),
                &licenses.join(format!("instigator.{file}")),
            )?;
        }
    }
    fs::remove_dir_all(&raw)?;
    fs::write(&stamp_file, stamp)?;
    Ok(tree)
}

fn native(options: &Options) -> Result<&'static str> {
    let host = lock::host_platform()?;
    if let Some(platform) = &options.platform {
        if platform != host {
            return Err(format!("this task builds for {host}, not {platform}").into());
        }
    }
    Ok(host)
}

fn cargo(args: &[&str]) -> Command {
    let mut command = Command::new(std::env::var_os("CARGO").unwrap_or("cargo".into()));
    command.args(args).current_dir(root());
    command
}

fn run(mut command: Command) -> Result<()> {
    let status = command.status()?;
    if !status.success() {
        return Err(format!("{command:?} failed: {status}").into());
    }
    Ok(())
}

fn test(options: &Options) -> Result<()> {
    let platform = native(options)?;
    let tree = runtime(platform, options.qemu_archive.as_deref())?;
    let runtime_dir = tree.join("libexec/origami");
    run(cargo(&["test", "--locked"]))?;
    // The contract tests create and start every preset, which needs QEMU, its
    // init tool and the keymaps laid out as an installed product has them.
    let mut command = cargo(&[
        "test",
        "--locked",
        "--test",
        "runtime",
        "--",
        "--include-ignored",
    ]);
    command.env("ORIGAMI_RUNTIME_DIR", runtime_dir);
    run(command)
}

/// `0.4.1` at a release tag, `0.4.0-5-gabc1234` after one, `-dirty` with
/// tracked edits, and the bare commit without a `v*` tag.
fn describe() -> String {
    let git = |args: &[&str]| {
        Command::new("git")
            .args(["--no-optional-locks", "-C"])
            .arg(root())
            .args(args)
            .output()
            .ok()
            .filter(|o| o.status.success())
    };
    let Some(out) = git(&["describe", "--tags", "--match", "v[0-9]*", "--always"]) else {
        return "unknown".into();
    };
    let mut version = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if version.is_empty() {
        return "unknown".into();
    }
    git(&["update-index", "-q", "--refresh"]);
    if git(&["diff-index", "--quiet", "HEAD", "--"]).is_none() {
        version.push_str("-dirty");
    }
    version
        .strip_prefix('v')
        .map(str::to_string)
        .unwrap_or(version)
}

fn dist(options: &Options) -> Result<()> {
    let platform = native(options)?;
    let tree = runtime(platform, options.qemu_archive.as_deref())?;
    run(cargo(&[
        "build",
        "--release",
        "--locked",
        "--bin",
        "origami",
    ]))?;
    let version = describe();
    let top = format!("origami-{version}");
    let stage = target_dir().join("dist").join(platform).join("tree");
    if stage.exists() {
        fs::remove_dir_all(&stage)?;
    }
    archive::copy_tree(&tree, &stage)?;
    let name = exe("origami", platform);
    archive::copy_file(
        &target_dir().join("release").join(&name),
        &stage.join("bin").join(&name),
    )?;
    let licenses = stage.join("share/origami/licenses");
    archive::copy_file(&root().join("LICENSE"), &licenses.join("origami.LICENSE"))?;
    let base = fs::read_to_string(root().join("THIRD_PARTY_NOTICES"))?;
    fs::create_dir_all(&licenses)?;
    fs::write(
        licenses.join("THIRD_PARTY_NOTICES"),
        notices::cli(&base, notices::triple(platform, None))?,
    )?;

    let out = options
        .out
        .clone()
        .unwrap_or_else(|| target_dir().join("dist"));
    fs::create_dir_all(&out)?;
    let file = out.join(format!(
        "{top}-{platform}.{}",
        lock::archive_extension(platform)
    ));
    archive::pack(&stage, &top, &file)?;
    let hash = archive::sha256_file(&file)?;
    let file_name = file.file_name().unwrap().to_string_lossy().into_owned();
    fs::write(
        format!("{}.sha256", file.display()),
        format!("{hash}  {file_name}\n"),
    )?;
    println!("{}", file.display());
    Ok(())
}

fn github_text(repo: &str, tag: &str, asset: &str) -> Result<String> {
    let url = lock::download_url(repo, tag, asset);
    Ok(String::from_utf8(archive::http_get(&url)?)?)
}

/// Each platform's checksum from the release's published list, falling back
/// to a per-archive `.sha256` file for any platform the combined list lacks.
/// A release that has neither list is reported as that, not as every
/// archive missing.
fn release_pin(
    repo: &str,
    tag: &str,
    combined: &str,
    asset_name: &dyn Fn(&str) -> Result<String>,
) -> Result<Pin> {
    let combined_sums = match github_text(repo, tag, combined) {
        Ok(text) => lock::parse_checksums(&text),
        Err(error) => {
            eprintln!("note: {repo} {tag} has no usable {combined} ({error}); trying per-archive .sha256 files");
            BTreeMap::new()
        }
    };
    let mut platforms = BTreeMap::new();
    let mut missing = Vec::new();
    for platform in PLATFORMS {
        let asset = asset_name(platform)?;
        let hash = match combined_sums.get(&asset) {
            Some(hash) => Some(hash.clone()),
            None => github_text(repo, tag, &format!("{asset}.sha256"))
                .ok()
                .and_then(|text| lock::parse_checksums(&text).remove(&asset)),
        };
        match hash {
            Some(sha256) => {
                platforms.insert(
                    platform.to_string(),
                    Asset {
                        url: lock::download_url(repo, tag, &asset),
                        sha256,
                    },
                );
            }
            None => missing.push(asset),
        }
    }
    if !missing.is_empty() {
        return Err(format!(
            "{repo} release {tag} has no checksum for: {}",
            missing.join(", ")
        )
        .into());
    }
    Ok(Pin {
        tag: tag.into(),
        platforms,
    })
}

/// Replaces the embedded configs with the ones in the Instigator archive at
/// `pin`, checking that archive has them before anything is removed.
fn refresh_configs(pin: &Pin) -> Result<()> {
    let platform = lock::host_platform()?;
    let asset = &pin.platforms[platform];
    let file = archive::fetch(
        &asset.url,
        &asset.sha256,
        &target_dir().join("runtime/downloads"),
    )?;
    let unpacked = target_dir().join("runtime/bump-instigator");
    archive::unpack(&file, &unpacked)?;
    if !unpacked.join("configs").is_dir() {
        return Err(format!("{} has no configs/ directory", asset.url).into());
    }
    let staged = configs_dir().with_extension("new");
    if staged.exists() {
        fs::remove_dir_all(&staged)?;
    }
    archive::copy_tree(&unpacked.join("configs"), &staged)?;
    fs::remove_dir_all(configs_dir())?;
    fs::rename(&staged, configs_dir())?;
    fs::remove_dir_all(&unpacked)?;
    Ok(())
}

fn bump(what: &str, tag: &str) -> Result<()> {
    let path = root().join("runtime.lock");
    let mut lock = Lock::read(&path)?;
    match what {
        "qemu" => {
            lock.qemu = release_pin(lock::QEMU_REPO, tag, "SHA256SUMS", &|p| {
                Ok(lock::qemu_asset_name(tag, p))
            })?;
        }
        "instigator" => {
            lock.instigator = release_pin(lock::INSTIGATOR_REPO, tag, "checksums.txt", &|p| {
                lock::instigator_asset_name(tag, p).map_err(Into::into)
            })?;
            // Before the lock changes, so a release that cannot supply the
            // configs leaves both the lock and the configs as they were.
            refresh_configs(&lock.instigator)?;
            eprintln!(
                "refreshed resources/instigator/configs, replacing any local edits; also update \
                 the tag named in the READMEs under resources/instigator and crates/*/tests/data, \
                 and refresh the transcripts there if Instigator changed them"
            );
        }
        other => return Err(format!("bump qemu or bump instigator, not {other}").into()),
    }
    fs::write(&path, lock::render(&lock))?;
    eprintln!("runtime.lock now pins {what} {tag}");
    Ok(())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = (|| -> Result<()> {
        let Some((command, rest)) = args.split_first() else {
            return Err(
                "usage: cargo xtask runtime | test | dist | bump qemu|instigator TAG".into(),
            );
        };
        let options = parse(rest)?;
        match command.as_str() {
            "runtime" => {
                let platform = match &options.platform {
                    Some(platform) => platform.as_str(),
                    None => lock::host_platform()?,
                };
                println!(
                    "{}",
                    runtime(platform, options.qemu_archive.as_deref())?.display()
                );
                Ok(())
            }
            "test" => test(&options),
            "dist" => dist(&options),
            "bump" => match options.positional.as_slice() {
                [what, tag] => bump(what, tag),
                _ => Err("usage: cargo xtask bump qemu|instigator TAG".into()),
            },
            other => Err(format!("unknown task {other}").into()),
        }
    })();
    if let Err(error) = result {
        eprintln!("xtask: {error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Packs a synthetic QEMU archive holding a keymap directory at each path
    /// in `keymap_dirs` (paths inside the archive) and unpacks it, so detection
    /// sees what `runtime` sees.
    fn unpacked_archive(name: &str, keymap_dirs: &[&str]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("xtask-keymaps-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let tree = dir.join("tree");
        fs::create_dir_all(tree.join("bin")).unwrap();
        fs::write(tree.join("bin/qemu-img"), b"").unwrap();
        for rel in keymap_dirs {
            fs::create_dir_all(tree.join(rel)).unwrap();
            fs::write(tree.join(rel).join("en-us"), b"").unwrap();
        }
        let archive = dir.join("qemu.tar.gz");
        archive::pack(&tree, "qemu-1", &archive).unwrap();
        let unpacked = dir.join("unpacked");
        archive::unpack(&archive, &unpacked).unwrap();
        unpacked.join("qemu-1")
    }

    #[test]
    fn finds_keymaps_under_share_qemu() {
        let qemu = unpacked_archive("new", &["share/qemu/keymaps"]);
        assert_eq!(keymaps_dir(&qemu).unwrap(), qemu.join("share/qemu/keymaps"));
    }

    #[test]
    fn finds_keymaps_at_the_pinned_old_path() {
        let qemu = unpacked_archive("old", &["share/keymaps"]);
        assert_eq!(keymaps_dir(&qemu).unwrap(), qemu.join("share/keymaps"));
    }

    #[test]
    fn prefers_the_new_path_when_both_exist() {
        let qemu = unpacked_archive("both", &["share/keymaps", "share/qemu/keymaps"]);
        assert_eq!(keymaps_dir(&qemu).unwrap(), qemu.join("share/qemu/keymaps"));
    }

    #[test]
    fn rejects_an_archive_without_keymaps() {
        let qemu = unpacked_archive("none", &[]);
        let error = keymaps_dir(&qemu).unwrap_err().to_string();
        assert_eq!(
            error,
            "the QEMU archive has no share/qemu/keymaps or share/keymaps"
        );
    }
}
