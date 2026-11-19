//! `cargo xtask universal`: one macOS package from the arm64 and x86_64 ones.
//! Mach-O files are joined with `lipo`; any other file must match in both.

use crate::archive::{self, Result};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Mach-O thin files, either byte order and width. A fat file is not one:
/// there is nothing to join.
fn is_thin_macho(bytes: &[u8]) -> bool {
    matches!(
        bytes.get(..4),
        Some([0xcf, 0xfa, 0xed, 0xfe] | [0xce, 0xfa, 0xed, 0xfe])
    )
}

fn run(mut command: Command) -> Result<()> {
    let status = command.status()?;
    if !status.success() {
        return Err(format!("{command:?} failed: {status}").into());
    }
    Ok(())
}

/// Signs a Mach-O file or bundle ad hoc, since `lipo` drops signatures and
/// Apple silicon refuses unsigned code. A real identity goes here.
pub fn sign(path: &Path) -> Result<()> {
    let mut codesign = Command::new("codesign");
    codesign
        .args(["--force", "--deep", "--sign", "-"])
        .arg(path);
    run(codesign)
}

/// Joins the trees `a` (arm64) and `b` (x86_64) into `out`, returning the
/// Mach-O files it wrote.
fn merge(a: &Path, b: &Path, out: &Path) -> Result<Vec<PathBuf>> {
    let mut joined = Vec::new();
    // Each build's seal differs and `sign` makes a new one for the joined bundle.
    let sealed = |rel: &PathBuf| rel.components().any(|c| c.as_os_str() == "_CodeSignature");
    let in_b: Vec<_> = archive::list(b)?
        .into_iter()
        .filter(|r| !sealed(r))
        .collect();
    let in_a: Vec<_> = archive::list(a)?
        .into_iter()
        .filter(|r| !sealed(r))
        .collect();
    if let Some(extra) = in_b.iter().find(|rel| !in_a.contains(rel)) {
        return Err(format!("{} is only in the x86_64 package", extra.display()).into());
    }
    fs::create_dir_all(out)?;
    for rel in in_a {
        let (pa, pb, po) = (a.join(&rel), b.join(&rel), out.join(&rel));
        if pa.is_dir() {
            fs::create_dir_all(&po)?;
            continue;
        }
        if !pb.is_file() {
            return Err(format!("{} is only in the arm64 package", rel.display()).into());
        }
        let (bytes_a, bytes_b) = (fs::read(&pa)?, fs::read(&pb)?);
        if bytes_a == bytes_b {
            archive::copy_file(&pa, &po)?;
        } else if is_thin_macho(&bytes_a) && is_thin_macho(&bytes_b) {
            if let Some(parent) = po.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut lipo = Command::new("lipo");
            lipo.arg("-create")
                .arg(&pa)
                .arg(&pb)
                .arg("-output")
                .arg(&po);
            run(lipo)?;
            fs::set_permissions(&po, fs::metadata(&pa)?.permissions())?;
            joined.push(po);
        } else {
            return Err(format!(
                "{} differs between the arm64 and x86_64 packages and is not a Mach-O file",
                rel.display()
            )
            .into());
        }
    }
    Ok(joined)
}

fn only_child(dir: &Path) -> Result<PathBuf> {
    let mut children = fs::read_dir(dir)?.collect::<std::io::Result<Vec<_>>>()?;
    match children.as_slice() {
        [one] => Ok(one.path()),
        _ => {
            children.sort_by_key(|c| c.file_name());
            Err(format!("{} should hold one entry", dir.display()).into())
        }
    }
}

fn universal_name(file: &Path) -> Result<String> {
    let name = file.file_name().unwrap().to_string_lossy();
    if !name.contains("macos-arm64") {
        return Err(format!("{name} is not a macos-arm64 package").into());
    }
    Ok(name.replace("macos-arm64", "macos-universal"))
}

/// `arm64` and `x86_64` are archives of one package kind: a `.tar.gz`
/// command-line package, or the zipped `Origami.app`.
pub fn run_universal(arm64: &Path, x86_64: &Path, out: &Path) -> Result<()> {
    let name = universal_name(arm64)?;
    let scratch = out.join("universal-work");
    if scratch.exists() {
        fs::remove_dir_all(&scratch)?;
    }
    let (dir_a, dir_b, dir_out) = (
        scratch.join("arm64"),
        scratch.join("x86_64"),
        scratch.join("out"),
    );
    let target = out.join(&name);
    fs::create_dir_all(out)?;
    if name.ends_with(".zip") {
        for (file, dir) in [(arm64, &dir_a), (x86_64, &dir_b)] {
            fs::create_dir_all(dir)?;
            let mut unzip = Command::new("ditto");
            unzip.args(["-x", "-k"]).arg(file).arg(dir);
            run(unzip)?;
        }
        let (app_a, app_b) = (only_child(&dir_a)?, only_child(&dir_b)?);
        let app_out = dir_out.join(app_a.file_name().unwrap());
        merge(&app_a, &app_b, &app_out)?;
        // After the merge and before the zip, so a signing identity slots in here.
        sign(&app_out)?;
        let _ = fs::remove_file(&target);
        let mut zip = Command::new("ditto");
        zip.args(["-c", "-k", "--keepParent"])
            .arg(&app_out)
            .arg(&target);
        run(zip)?;
    } else {
        archive::unpack(arm64, &dir_a)?;
        archive::unpack(x86_64, &dir_b)?;
        let (top_a, top_b) = (only_child(&dir_a)?, only_child(&dir_b)?);
        let top_out = dir_out.join(top_a.file_name().unwrap());
        for file in merge(&top_a, &top_b, &top_out)? {
            sign(&file)?;
        }
        archive::pack(
            &top_out,
            &top_a.file_name().unwrap().to_string_lossy(),
            &target,
        )?;
    }
    fs::remove_dir_all(&scratch)?;
    println!("{}", target.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_thin_mach_o_files_are_joined() {
        assert!(is_thin_macho(&[0xcf, 0xfa, 0xed, 0xfe, 7, 0, 0, 1]));
        assert!(!is_thin_macho(&[0xca, 0xfe, 0xba, 0xbe, 0, 0, 0, 2]));
        assert!(!is_thin_macho(b"#!/bin/sh\n"));
        assert!(!is_thin_macho(&[]));
    }

    #[test]
    fn the_universal_name_replaces_the_arm64_platform() {
        let file = Path::new("dist/origami-0.6.1-macos-arm64.zip");
        assert_eq!(
            universal_name(file).unwrap(),
            "origami-0.6.1-macos-universal.zip"
        );
        assert!(universal_name(Path::new("origami-0.6.1-linux-arm64.zip")).is_err());
    }

    #[test]
    fn equal_files_merge_and_unequal_text_is_refused() {
        let dir = std::env::temp_dir().join(format!("xtask-universal-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        for side in ["a", "b"] {
            fs::create_dir_all(dir.join(side).join("share")).unwrap();
            fs::write(dir.join(side).join("share/same"), b"same").unwrap();
        }
        merge(&dir.join("a"), &dir.join("b"), &dir.join("ok")).unwrap();
        assert_eq!(fs::read(dir.join("ok/share/same")).unwrap(), b"same");
        fs::write(dir.join("b/share/same"), b"other").unwrap();
        let error = merge(&dir.join("a"), &dir.join("b"), &dir.join("bad"))
            .unwrap_err()
            .to_string();
        let path = Path::new("share").join("same").display().to_string();
        assert!(
            error.contains(&path) && error.contains("not a Mach-O"),
            "{error}"
        );
        fs::remove_dir_all(&dir).unwrap();
    }
}
