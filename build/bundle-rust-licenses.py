#!/usr/bin/env python3
"""Collect license files for the crates used by a product build."""

import shutil
import sys
import tomllib
from pathlib import Path


def license_files(source_root, name, version):
    source = list(source_root.glob(f'*/{name}-{version}'))
    if not source:
        raise RuntimeError(f'missing crate source: {name} {version}')
    files = sorted(path for path in source[0].iterdir()
                   if path.name.lower().startswith('license'))
    import_libraries = {
        ('winapi-i686-pc-windows-gnu', '0.4.0'),
        ('winapi-x86_64-pc-windows-gnu', '0.4.0'),
    }
    if not files and (name, version) in import_libraries:
        parent = list(source_root.glob('*/winapi-0.3.9'))
        if parent:
            files = sorted(path for path in parent[0].iterdir()
                           if path.name.lower().startswith('license'))
    if not files:
        raise RuntimeError(f'missing license file: {name} {version}')
    return files


def main():
    if len(sys.argv) != 3:
        raise SystemExit(f"usage: {sys.argv[0]} CARGO-HOME BUNDLE")
    cargo_home, bundle = map(Path, sys.argv[1:])
    lock = tomllib.loads((Path(__file__).resolve().parents[1] / "Cargo.lock").read_text())
    source_root = cargo_home / "registry/src"
    notices = bundle / "share/sgi/licenses/rust"
    shutil.rmtree(notices, ignore_errors=True)
    notices.mkdir(parents=True)
    manifest = ["crate\tversion\tlicense files\n"]
    for package in lock["package"]:
        if "source" not in package:
            continue
        name, version = package["name"], package["version"]
        files = license_files(source_root, name, version)
        destination = notices / f"{name}-{version}"
        destination.mkdir()
        for path in files:
            shutil.copy2(path, destination / path.name)
        manifest.append(f"{name}\t{version}\t{','.join(path.name for path in files)}\n")
    (bundle / "share/sgi/rust-dependencies.tsv").write_text("".join(manifest))
    print(f"bundled license files for {len(manifest) - 1} Rust crates")


if __name__ == "__main__":
    main()
