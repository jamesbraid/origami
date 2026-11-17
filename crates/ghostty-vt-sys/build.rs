// Finds libghostty-vt: GHOSTTY_VT_LIB_DIR or GHOSTTY_VT_DIR, else fetched or
// built once into <target>/ghostty-vt, SHA-256 pinned (see README.md).
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::{env, fs, io};

const VERSION: &str = "1.0.1";
const BASE: &str = "https://release.files.ghostty.org/libghostty-vt/1.0.1";
const ZIG_VERSION: &str = "0.17.1";
const MAC: (&str, &str) = (
    "ghostty-vt-1.0.1.xcframework.zip",
    "c0f9631277f726154c8f521954d17cdc41aac4626fc0df8cf925a19ee58e829b",
);
const WIN_X64: (&str, &str) = (
    "ghostty-vt-1.0.1-x86_64-windows-gnu.zip",
    "8e2eb9e1822e85d786b456c8beb86f49d09c318c91f0c2913f91e7ffa12cda29",
);
const WIN_ARM64: (&str, &str) = (
    "ghostty-vt-1.0.1-aarch64-windows-gnu.zip",
    "cc61da5f968ea0b08639bdf019c6b7da46bad1d985774fd8a2de57af32862892",
);

fn main() {
    for v in ["GHOSTTY_VT_LIB_DIR", "GHOSTTY_VT_DIR"] {
        println!("cargo:rerun-if-env-changed={v}");
    }
    println!("cargo:rerun-if-changed=ci/build-lib.sh");
    let os = env::var("CARGO_CFG_TARGET_OS").unwrap();
    let arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap();
    let var = |k| env::var(k).ok().filter(|v| !v.is_empty());
    let given = var("GHOSTTY_VT_LIB_DIR")
        .map(PathBuf::from)
        .or_else(|| var("GHOSTTY_VT_DIR").map(|d| Path::new(&d).join("lib")));
    let (dir, name) = match given {
        Some(dir) => (dir, lib_name(&os)),
        None => fetch(&os, &arch),
    };
    println!("cargo:rustc-link-search=native={}", dir.display());
    if os == "windows" {
        // mingw's linker and rustc only look for lib*.a by name.
        println!("cargo:rustc-link-lib=dylib:+verbatim={name}");
        println!(
            "cargo:warning=ghostty-vt.dll must ship next to the executable: {}",
            dir.join("ghostty-vt.dll").display()
        );
    } else {
        println!("cargo:rustc-link-lib=static={name}");
    }
}

fn lib_name(os: &str) -> &'static str {
    if os == "windows" {
        "ghostty-vt.lib"
    } else {
        "ghostty-vt"
    }
}

/// The library's directory and link name, fetched or built into the cache.
fn fetch(os: &str, arch: &str) -> (PathBuf, &'static str) {
    // The cache sits in the target directory, shared by profiles: above the
    // `build` directory is <profile>, and above that <triple> when cross-building.
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    let build = out
        .ancestors()
        .find(|p| p.file_name().is_some_and(|n| n == "build"));
    let mut root = build
        .expect("OUT_DIR is not under a build directory")
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    if root
        .file_name()
        .is_some_and(|n| *n == *env::var("TARGET").unwrap())
    {
        root = root.parent().unwrap();
    }
    let cache = root.join("ghostty-vt");
    let lib = cache.join(format!("{VERSION}-{os}-{arch}"));
    let file = lib.join(match os {
        "windows" => "ghostty-vt.lib",
        _ => "libghostty-vt.a",
    });
    if !file.exists() {
        fs::create_dir_all(&cache).unwrap();
        // Work in a private directory and rename it in, so two builds at
        // once never see a half-written library.
        let tmp = cache.join(format!("tmp-{os}-{arch}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        match os {
            "macos" => unzip_member(
                &download(&tmp, MAC),
                "macos-arm64_x86_64/libghostty-vt.a",
                &tmp,
            ),
            "windows" => {
                let zip = if arch == "aarch64" {
                    WIN_ARM64
                } else {
                    WIN_X64
                };
                let zip = download(&tmp, zip);
                unzip_member(&zip, "lib/ghostty-vt.lib", &tmp);
                unzip_member(&zip, "bin/ghostty-vt.dll", &tmp);
            }
            "linux" => build_linux(&tmp),
            _ => panic!("libghostty-vt has no build for {os}"),
        }
        for z in fs::read_dir(&tmp).unwrap().flatten() {
            if z.path().extension().is_some_and(|e| e == "zip") {
                let _ = fs::remove_file(z.path());
            }
        }
        let built = match os {
            "linux" => tmp.join("lib"),
            _ => tmp.clone(),
        };
        let _ = fs::remove_dir_all(&lib);
        if fs::rename(&built, &lib).is_err() && !file.exists() {
            panic!("could not place libghostty-vt in {}", lib.display());
        }
        let _ = fs::remove_dir_all(&tmp);
    }
    (lib, lib_name(os))
}

fn build_linux(into: &Path) {
    let script = Path::new(&env::var("CARGO_MANIFEST_DIR").unwrap()).join("ci/build-lib.sh");
    let mut sh = Command::new("sh");
    sh.arg(&script).arg(into);
    // A zig on PATH is used only if it is the pinned version; otherwise the
    // pinned one is fetched and unpacked here (no xz binary needed).
    let have = Command::new("zig").arg("version").output();
    if !have.is_ok_and(|o| o.stdout.trim_ascii() == ZIG_VERSION.as_bytes()) {
        let path = env::var("PATH").unwrap_or_default();
        sh.env("PATH", format!("{}:{path}", zig(into).display()));
    }
    let status = sh.status().expect("cannot run sh");
    assert!(
        status.success(),
        "building libghostty-vt failed (needs curl)"
    );
}

/// The pinned Zig, unpacked beside the cache.
fn zig(into: &Path) -> PathBuf {
    let (arch, sha) = match env::var("CARGO_CFG_TARGET_ARCH").unwrap().as_str() {
        "x86_64" => (
            "x86_64",
            "3581863e3cbcfd0fce0efc682ce7e9e93da96668a977791d356a26d2a7e9b17a",
        ),
        "aarch64" => (
            "aarch64",
            "a1546d5646eb184437918d6635ca6b762a0e889eac09cef720ee7a1ed3fe7795",
        ),
        a => panic!("no pinned Zig for {a}"),
    };
    let dir = into
        .parent()
        .unwrap()
        .join(format!("zig-{ZIG_VERSION}-{arch}"));
    if dir.join("zig").exists() {
        return dir;
    }
    let name = format!("zig-{arch}-linux-{ZIG_VERSION}.tar.xz");
    let url = format!("https://ziglang.org/download/{ZIG_VERSION}/{name}");
    let tarball = fetch_to(into, &url, &name, sha);
    let mut xz = Vec::new();
    let mut reader = io::BufReader::new(fs::File::open(&tarball).unwrap());
    lzma_rs::xz_decompress(&mut reader, &mut xz).unwrap();
    let tmp = dir.with_extension(format!("tmp{}", std::process::id()));
    let _ = fs::remove_dir_all(&tmp);
    for entry in tar::Archive::new(xz.as_slice()).entries().unwrap() {
        let mut entry = entry.unwrap();
        // Drop the archive's top directory.
        let rest: PathBuf = entry.path().unwrap().components().skip(1).collect();
        if rest.as_os_str().is_empty() {
            continue;
        }
        assert!(
            rest.components()
                .all(|c| matches!(c, std::path::Component::Normal(_))),
            "unsafe path in the Zig archive: {}",
            rest.display()
        );
        let to = tmp.join(rest);
        fs::create_dir_all(to.parent().unwrap()).unwrap();
        entry.unpack(&to).unwrap();
    }
    let _ = fs::remove_file(tarball);
    if fs::rename(&tmp, &dir).is_err() {
        let _ = fs::remove_dir_all(&tmp);
    }
    dir
}

fn download(dir: &Path, (name, sha): (&str, &str)) -> PathBuf {
    fetch_to(dir, &format!("{BASE}/{name}"), name, sha)
}

/// Download `url` to `dir`/`name` and check its SHA-256.
fn fetch_to(dir: &Path, url: &str, name: &str, sha: &str) -> PathBuf {
    let path = dir.join(name);
    let status = Command::new("curl")
        .args([
            "-fsSL",
            "--proto",
            "=https",
            "--tlsv1.2",
            "--retry",
            "3",
            "-o",
        ])
        .arg(&path)
        .arg(url)
        .status()
        .expect("cannot run curl");
    assert!(status.success(), "downloading {name} failed");
    let mut hasher = Sha256::new();
    io::copy(&mut fs::File::open(&path).unwrap(), &mut hasher).unwrap();
    let got: String = hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert_eq!(got, sha, "{name} does not match its pinned SHA-256");
    path
}

/// Copy the archive member whose path ends in `suffix` to `to`/<its file name>.
fn unzip_member(zip: &Path, suffix: &str, to: &Path) {
    let mut archive = zip::ZipArchive::new(fs::File::open(zip).unwrap()).unwrap();
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).unwrap();
        if entry.name().ends_with(suffix) {
            let leaf = suffix.rsplit('/').next().unwrap();
            io::copy(&mut entry, &mut fs::File::create(to.join(leaf)).unwrap()).unwrap();
            return;
        }
    }
    panic!("{suffix} is not in {}", zip.display());
}
