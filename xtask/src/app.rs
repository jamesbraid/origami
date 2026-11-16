//! `cargo xtask dist --app`: the native app packages, each the command-line
//! archive's tree plus the platform's app and its notices.

use crate::archive::{self, Result};
use crate::{cargo, notices, root, run, target_dir};
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Installed as `bindings/csharp/gen.sh` documents: v0.12.0+v0.32.0, `--locked`.
const BINDGEN_CS: &str = "uniffi-bindgen-cs";

/// Everything the app step needs from the shared product staging.
pub struct Product<'a> {
    pub platform: &'a str,
    pub version: &'a str,
    /// The command-line archive's tree, with `bin/origami` and the licences.
    pub stage: &'a Path,
    pub out: &'a Path,
}

fn require(path: &str, what: &str) -> Result<()> {
    if root().join(path).exists() {
        Ok(())
    } else {
        Err(format!("{what} is not in this checkout ({path})").into())
    }
}

fn write_sha256(file: &Path) -> Result<()> {
    let hash = archive::sha256_file(file)?;
    let name = file.file_name().unwrap().to_string_lossy().into_owned();
    fs::write(
        format!("{}.sha256", file.display()),
        format!("{hash}  {name}\n"),
    )?;
    println!("{}", file.display());
    Ok(())
}

/// Appends the app's Rust crates to the stage's `THIRD_PARTY_NOTICES`.
fn append_notices(stage: &Path, section: &str) -> Result<()> {
    let path = stage.join("share/origami/licenses/THIRD_PARTY_NOTICES");
    let mut text = fs::read_to_string(&path)?;
    text.push_str(section);
    fs::write(path, text)?;
    Ok(())
}

/// The notices section for the crates `roots` link beyond the command-line tool's.
fn app_notices(title: &str, roots: &[&str], triple: &str, manifest: &Path) -> Result<String> {
    let cli = notices::crates(&root().join("Cargo.toml"), &["origami"], triple)?;
    let skip = cli.iter().map(|k| k.id()).collect();
    let crates = notices::crates(manifest, roots, triple)?;
    let intro = "The app links these crates in addition to those above. Libraries it loads\nfrom the host (such as GTK on Linux) are not bundled.";
    notices::render(title, intro, &crates, &skip)
}

/// Builds only the Rust parts of the Windows app into `dir`.
pub fn prebuild(platform: &str, dir: &Path, target: Option<&str>) -> Result<()> {
    if platform != "windows-x86_64" {
        return Err("--prebuild is for windows-x86_64".into());
    }
    require("apps/windows", "the Windows app")?;
    windows_prebuild(dir, target)
}

pub fn dist(product: &Product, options: &crate::Options) -> Result<()> {
    match product.platform {
        "linux-x86_64" => linux(product),
        "macos-arm64" => macos(product),
        "windows-x86_64" => windows(product, options),
        other => Err(format!("no app for {other}").into()),
    }
}

fn pack_app(product: &Product, stage: &Path) -> Result<()> {
    fs::create_dir_all(product.out)?;
    let file = product.out.join(format!(
        "origami-{}-{}-app.{}",
        product.version,
        product.platform,
        crate::lock::archive_extension(product.platform)
    ));
    archive::pack(stage, &format!("origami-{}", product.version), &file)?;
    write_sha256(&file)
}

fn linux(product: &Product) -> Result<()> {
    require("crates/origami-gtk", "the GTK app")?;
    run(cargo(&[
        "build",
        "--release",
        "--locked",
        "-p",
        "origami-gtk",
        "--bin",
        "origami-gtk",
    ]))?;
    archive::copy_file(
        &target_dir().join("release/origami-gtk"),
        &product.stage.join("bin/origami-gtk"),
    )?;
    let id = "fans.irix.Origami";
    archive::copy_file(
        &root().join(format!("resources/linux/{id}.desktop")),
        &product
            .stage
            .join(format!("share/applications/{id}.desktop")),
    )?;
    archive::copy_file(
        &root().join(format!("resources/linux/{id}.svg")),
        &product
            .stage
            .join(format!("share/icons/hicolor/scalable/apps/{id}.svg")),
    )?;
    let triple = notices::triple(product.platform, None);
    let manifest = root().join("Cargo.toml");
    append_notices(
        product.stage,
        &app_notices("Rust crates (app)", &["origami-gtk"], triple, &manifest)?,
    )?;
    pack_app(product, product.stage)
}

fn macos(product: &Product) -> Result<()> {
    require("apps/macos/build.sh", "the macOS app")?;
    let triple = notices::triple(product.platform, None);
    let manifest = root().join("Cargo.toml");
    append_notices(
        product.stage,
        &app_notices("Rust crates (app)", &["origami-ffi"], triple, &manifest)?,
    )?;
    let work = target_dir().join("dist/macos-arm64");
    let app = work.join("Origami.app");
    let _ = fs::remove_dir_all(&app);
    let mut build = Command::new(root().join("apps/macos/build.sh"));
    build
        .env("APP_OUT", &app)
        // build.sh copies bin/, libexec/ and share/ in; the app is not named
        // Origami, as macOS file names ignore case.
        .env("ORIGAMI_PRODUCT", product.stage)
        .env("CARGO_TARGET_DIR", target_dir());
    run(build)?;
    let plist = app.join("Contents/Info.plist");
    let mut set = Command::new("/usr/libexec/PlistBuddy");
    set.arg("-c")
        .arg(format!(
            "Set :CFBundleShortVersionString {}",
            product.version
        ))
        .arg(&plist);
    run(set)?;
    let mut sign = Command::new("codesign");
    sign.args(["--force", "--deep", "--sign", "-"]).arg(&app);
    run(sign)?;
    fs::create_dir_all(product.out)?;
    let file = product
        .out
        .join(format!("origami-{}-macos-arm64-app.zip", product.version));
    let _ = fs::remove_file(&file);
    let mut zip = Command::new("ditto");
    zip.args(["-c", "-k", "--keepParent"]).arg(&app).arg(&file);
    run(zip)?;
    write_sha256(&file)
}

fn windows(product: &Product, options: &crate::Options) -> Result<()> {
    require("apps/windows", "the Windows app")?;
    let prebuilt = match &options.prebuilt {
        Some(dir) => dir.clone(),
        None => {
            let dir = target_dir().join("dist/windows-prebuilt");
            windows_prebuild(&dir, options.target.as_deref())?;
            dir
        }
    };
    let app_project = root().join("apps/windows/Origami.App");
    for (from, to) in [("native-out", "native-out"), ("Generated", "Generated")] {
        let to = app_project.join(to);
        let _ = fs::remove_dir_all(&to);
        archive::copy_tree(&prebuilt.join(from), &to)?;
    }
    let publish = target_dir().join("dist/windows-x86_64/publish");
    let _ = fs::remove_dir_all(&publish);
    let mut dotnet = Command::new("dotnet");
    dotnet
        .args(["publish"])
        .arg(app_project.join("Origami.App.csproj"))
        .args(["-c", "Release", "-p:Platform=x64", "-p:Qemu=true", "-o"])
        .arg(&publish);
    run(dotnet)?;
    archive::copy_tree(&publish, &product.stage.join("app"))?;
    archive::copy_file(
        &prebuilt.join("native-out/origami_ffi.dll"),
        &product.stage.join("app/origami_ffi.dll"),
    )?;
    let licenses = product.stage.join("share/origami/licenses");
    archive::copy_file(
        &prebuilt.join("THIRD_PARTY_NOTICES"),
        &licenses.join("THIRD_PARTY_NOTICES"),
    )?;
    let pointer = dotnet_notices(
        &app_project,
        &publish,
        &product.stage.join("share/origami/licenses/dotnet"),
    )?;
    append_notices(product.stage, &pointer)?;
    pack_app(product, product.stage)
}

/// Copies the restored NuGet packages' and the runtime pack's licence files
/// into `to`; returns notices text pointing there and naming packages with none.
fn dotnet_notices(project: &Path, publish: &Path, to: &Path) -> Result<String> {
    let read = |path: PathBuf| -> Result<Value> { Ok(serde_json::from_slice(&fs::read(path)?)?) };
    let assets = read(project.join("obj/project.assets.json"))?;
    let deps = read(publish.join("Origami.deps.json"))?;
    let folders: Vec<PathBuf> = assets["packageFolders"]
        .as_object()
        .into_iter()
        .flat_map(|f| f.keys().map(PathBuf::from))
        .collect();
    let restored = assets["libraries"]
        .as_object()
        .into_iter()
        .flat_map(|l| l.keys());
    let packs = deps["libraries"]
        .as_object()
        .into_iter()
        .flat_map(|l| l.keys())
        .filter_map(|k| k.strip_prefix("runtimepack."))
        .collect::<Vec<_>>();
    if packs.is_empty() {
        return Err(
            "Origami.deps.json names no runtime pack; is the publish self-contained?".into(),
        );
    }
    let (mut found, mut missing) = (0, Vec::new());
    for key in restored.map(String::as_str).chain(packs.iter().copied()) {
        let Some((name, version)) = key.split_once('/') else {
            continue;
        };
        let name = name.to_lowercase();
        let before = found;
        for folder in &folders {
            for entry in fs::read_dir(folder.join(&name).join(version))
                .into_iter()
                .flatten()
                .flatten()
            {
                let file = entry.file_name().to_string_lossy().to_uppercase();
                let wanted = ["LICENSE", "THIRD-PARTY", "THIRDPARTY"];
                if entry.path().is_file()
                    && (wanted.iter().any(|p| file.starts_with(p)) || file.contains("NOTICE"))
                {
                    archive::copy_file(&entry.path(), &to.join(&name).join(entry.file_name()))?;
                    found += 1;
                }
            }
        }
        if found == before {
            missing.push(format!("{name} {version}"));
        }
    }
    if found == 0 {
        return Err("the app's NuGet packages carry no licence or notice files".into());
    }
    let mut text = String::from("\nThe .NET runtime and Windows App SDK licences and notices are in\nshare/origami/licenses/dotnet/.\n");
    if !missing.is_empty() {
        text.push_str(&format!(
            "NuGet packages that ship no licence file:\n{}\n",
            missing.join("\n")
        ));
    }
    Ok(text)
}

/// Builds the Windows app's Rust parts into `out`: tools, DLLs, C# bindings,
/// `xtask.exe` and `THIRD_PARTY_NOTICES`.
fn windows_prebuild(out: &Path, target: Option<&str>) -> Result<()> {
    let triple: Vec<&str> = target.into_iter().flat_map(|t| ["--target", t]).collect();
    let release = match target {
        Some(t) => target_dir().join(t).join("release"),
        None => target_dir().join("release"),
    };
    for selection in [["--bin", "origami"], ["-p", "origami-ffi"]] {
        let mut build = vec!["build", "--release", "--locked"];
        build.extend(&triple);
        build.extend(selection);
        run(cargo(&build))?;
    }
    // The Windows runner has no compiler, so it runs this xtask cross-built.
    let mut build = vec!["build", "--release", "--locked"];
    build.extend(&triple);
    build.extend(["-p", "xtask"]);
    run(cargo(&build))?;
    let native = root().join("apps/windows/native");
    let mut presenter = Command::new(std::env::var_os("CARGO").unwrap_or("cargo".into()));
    presenter
        .args(["build", "--release", "--locked"])
        .args(&triple)
        .current_dir(&native);
    run(presenter)?;
    // The presenter is its own workspace; CARGO_TARGET_DIR, when set, covers it too.
    let presenter_target = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| native.join("target"));
    let presenter_dir = match target {
        Some(t) => presenter_target.join(t).join("release"),
        None => presenter_target.join("release"),
    };
    let _ = fs::remove_dir_all(out);
    for (from, to) in [
        (release.join("origami.exe"), "origami.exe"),
        (release.join("xtask.exe"), "xtask.exe"),
        (
            release.join("origami_ffi.dll"),
            "native-out/origami_ffi.dll",
        ),
        (
            presenter_dir.join("origami_present.dll"),
            "native-out/origami_present.dll",
        ),
    ] {
        archive::copy_file(&from, &out.join(to))?;
    }
    fs::create_dir_all(out.join("Generated"))?;
    let mut bindgen = Command::new(BINDGEN_CS);
    bindgen
        .arg("--library")
        .arg(release.join("origami_ffi.dll"))
        .arg("--out-dir")
        .arg(out.join("Generated"))
        .current_dir(root().join("crates/origami-ffi"));
    run(bindgen)?;
    let triple = notices::triple("windows-x86_64", target);
    let base = fs::read_to_string(root().join("THIRD_PARTY_NOTICES"))?;
    let mut text = notices::cli(&base, triple)?;
    let manifest = root().join("Cargo.toml");
    text.push_str(&app_notices(
        "Rust crates (app)",
        &["origami-ffi"],
        triple,
        &manifest,
    )?);
    let presenter = native.join("Cargo.toml");
    text.push_str(&app_notices(
        "Rust crates (presenter)",
        &["origami_present"],
        triple,
        &presenter,
    )?);
    fs::write(out.join("THIRD_PARTY_NOTICES"), text)?;
    Ok(())
}
