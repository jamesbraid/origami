#!/usr/bin/env python3
"""Collect notices for Homebrew libraries already copied into a macOS bundle."""

import os
import shutil
import subprocess
import sys
from pathlib import Path


def output(*args):
    return subprocess.check_output(args, text=True).strip()


def formula_owners(libraries, listings, versions=None, kegs=None):
    owners = {}
    for library in sorted(libraries):
        matches = sorted(formula for formula, files in listings.items()
                         if any(name == library for name, _ in files))
        if not matches:
            raise RuntimeError(f"no installed Homebrew formula owns {library}")
        if len(matches) != 1:
            raise RuntimeError(f"ambiguous Homebrew owner for {library}: {', '.join(matches)}")
        formula = matches[0]
        if versions is not None and len(versions.get(formula, ())) != 1:
            raise RuntimeError(f"multiple installed versions of Homebrew formula {formula} are required")
        if kegs is not None:
            keg = Path(kegs[formula]).resolve()
            paths = [Path(path).resolve() for name, path in listings[formula]
                     if name == library]
            if any(not path.is_relative_to(keg) for path in paths):
                raise RuntimeError(f"Homebrew file listing for {formula} escapes its installed keg")
        owners.setdefault(formula, set()).add(library)
    return owners


def installed_notices(keg, formula):
    files = [path for path in keg.iterdir()
             if path.is_file() and path.name.lower().startswith(
                 ("license", "licence", "copying", "notice"))]
    docdir = keg / "share/doc" / formula
    if docdir.exists():
        files.extend(path for path in docdir.iterdir()
                     if path.is_file() and path.name.lower().startswith(
                         ("license", "licence", "copying", "notice")))
    return sorted(set(files))


def source_notices(keg, formula, scratch):
    formula_file = keg / ".brew" / f"{formula}.rb"
    if not formula_file.is_file():
        raise RuntimeError(f"missing installed Homebrew formula file: {formula_file}")
    unpacked = scratch / "unpacked" / formula
    shutil.rmtree(unpacked, ignore_errors=True)
    unpacked.mkdir(parents=True, exist_ok=True)
    subprocess.run(["brew", "unpack", "--patch", f"--destdir={unpacked}",
                    str(formula_file)], check=True)
    roots = [unpacked, *(path for path in unpacked.iterdir() if path.is_dir())]
    files = sorted(path for root in roots for path in root.iterdir()
                   if path.is_file()
                   and path.name.lower().startswith(
                       ("license", "licence", "copying", "notice")))
    if not files:
        raise RuntimeError(f"no license notice found in source for Homebrew formula {formula}")
    return files


def collect_notices(bundle, scratch, kegs, versions, listings):
    bundle = Path(bundle)
    scratch = Path(scratch)
    libraries = {path.name for path in (bundle / "lib/sgi").rglob("*.dylib")
                 if path.is_file()}
    owners = formula_owners(libraries, listings, versions, kegs)
    destination = bundle / "share/sgi/licenses/homebrew"
    shutil.rmtree(destination, ignore_errors=True)
    destination.mkdir(parents=True)
    manifest = ["formula\tversion\tlibraries\tnotices\n"]
    for formula, names in sorted(owners.items()):
        keg = Path(kegs[formula])
        files = installed_notices(keg, formula)
        if not files:
            files = source_notices(keg, formula, scratch)
        formula_dir = destination / formula
        formula_dir.mkdir()
        copied = []
        for source in files:
            target = formula_dir / source.name
            if target.exists() and target.read_bytes() != source.read_bytes():
                raise RuntimeError(f"conflicting Homebrew notice filename for {formula}: {source.name}")
            if not target.exists():
                shutil.copy2(source, target)
            copied.append(target.name)
        version = versions[formula][0]
        manifest.append(f"{formula}\t{version}\t{','.join(sorted(names))}\t{','.join(sorted(copied))}\n")
    path = bundle / "share/sgi/macos-libraries.tsv"
    path.write_text("".join(manifest))
    return path


def main():
    if len(sys.argv) != 3:
        raise SystemExit(f"usage: {sys.argv[0]} BUNDLE SCRATCH")
    bundle, scratch = map(Path, sys.argv[1:])
    os.environ["HOMEBREW_NO_AUTO_UPDATE"] = "1"
    formulas = output("brew", "list", "--formula").splitlines()
    versions = {}
    kegs = {}
    listings = {}
    for formula in formulas:
        fields = output("brew", "list", "--versions", formula).split()
        if not fields or fields[0] != formula:
            continue
        versions[formula] = fields[1:]
        prefix = Path(output("brew", "--prefix", formula)).resolve()
        kegs[formula] = prefix
        listings[formula] = [(Path(path).name, str(Path(path).resolve()))
                             for path in output("brew", "list", "--verbose", formula).splitlines()
                             if path.startswith("/")]
    print(collect_notices(bundle, scratch, kegs, versions, listings))


if __name__ == "__main__":
    main()
