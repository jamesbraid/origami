#!/usr/bin/env python3
"""Collect notices for modules linked into the Instigator executable."""

import shutil
import sys
from pathlib import Path


def main():
    if len(sys.argv) != 3:
        raise SystemExit(f"usage: {sys.argv[0]} GO-DEPS BUNDLE")
    dependencies, bundle = map(Path, sys.argv[1:])
    notices = bundle / "share/sgi/licenses/go"
    shutil.rmtree(notices, ignore_errors=True)
    notices.mkdir(parents=True)
    manifest = ["module\tversion\tlicense files\n"]
    for line in dependencies.read_text().splitlines():
        if not line:
            continue
        name, version, directory = line.split("|", 2)
        if not version:
            continue  # The main Instigator module has its own notice.
        source = Path(directory)
        if not source.is_relative_to("/work/go-mod") or not source.is_dir():
            raise RuntimeError(f"invalid module directory: {directory}")
        files = sorted(path for path in source.iterdir() if path.is_file()
                       and path.name.lower().startswith(("license", "licence", "copying", "notice")))
        if not files:
            raise RuntimeError(f"missing license notice for {name} {version}")
        destination = notices / f"{name.replace('/', '__')}@{version}"
        destination.mkdir()
        for path in files:
            shutil.copy2(path, destination / path.name)
        manifest.append(f"{name}\t{version}\t{','.join(path.name for path in files)}\n")
    (bundle / "share/sgi/go-dependencies.tsv").write_text("".join(manifest))
    print(f"bundled license files for {len(manifest) - 1} Go modules")


if __name__ == "__main__":
    main()
