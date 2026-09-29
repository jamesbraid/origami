#!/usr/bin/env python3
"""Bundle non-system Mach-O libraries and rewrite their load paths."""

import os
import re
import shutil
import subprocess
import sys
from pathlib import Path


def output(*args):
    return subprocess.check_output(args, text=True).strip()


def load_commands(path):
    lines = output("otool", "-L", str(path)).splitlines()[1:]
    names = [line.strip().split(" (", 1)[0] for line in lines]
    if path.suffix == ".dylib":
        names = names[1:]  # The first entry is the library's own install name.
    return names


def rpaths(path):
    listing = output("otool", "-l", str(path))
    return re.findall(r"cmd LC_RPATH\s+cmdsize \d+\s+path (.*?) \(offset", listing)


def system_library(name):
    return name.startswith(("/usr/lib/", "/System/Library/"))


def resolve_name(name, loader, executable):
    if name.startswith("@loader_path/"):
        candidates = [loader.parent / name[len("@loader_path/"):]]
    elif name.startswith("@executable_path/"):
        candidates = [executable.parent / name[len("@executable_path/"):]]
    elif name.startswith("@rpath/"):
        suffix = name[len("@rpath/"):]
        candidates = []
        for owner in (loader, executable):
            for rpath in rpaths(owner):
                if rpath.startswith("@loader_path/"):
                    base = owner.parent / rpath[len("@loader_path/"):]
                elif rpath.startswith("@executable_path/"):
                    base = executable.parent / rpath[len("@executable_path/"):]
                elif rpath.startswith("/"):
                    base = Path(rpath)
                else:
                    continue
                candidates.append(base / suffix)
    elif name.startswith("/"):
        candidates = [Path(name)]
    else:
        raise RuntimeError(f"unsupported Mach-O load path {name} in {loader}")
    for candidate in candidates:
        if candidate.is_file():
            return candidate.resolve()
    raise RuntimeError(f"unresolved Mach-O load path {name} in {loader}")


def formula_for(path, cellar):
    try:
        return path.relative_to(cellar).parts[0]
    except ValueError as error:
        raise RuntimeError(f"non-system library is outside Homebrew Cellar: {path}") from error


def copy_notice(formula, keg, notices):
    candidates = list(keg.glob("LICEN[CS]E*")) + list(keg.glob("COPYING*"))
    candidates += list((keg / "share/doc" / formula).glob("LICEN[CS]E*"))
    candidates += list((keg / "share/doc" / formula).glob("COPYING*"))
    candidates += list((keg / "share/doc" / formula).glob("NOTICE*"))
    files = sorted(path for path in candidates if path.is_file())
    if not files:
        raise RuntimeError(f"no bundled license notice for Homebrew formula {formula}")
    destination = notices / formula
    destination.mkdir(exist_ok=True)
    for path in files:
        shutil.copy2(path, destination / path.name)
    return ",".join(path.name for path in files)


def main():
    if len(sys.argv) != 2:
        raise SystemExit(f"usage: {sys.argv[0]} BUNDLE")
    bundle = Path(sys.argv[1]).resolve()
    roots = [bundle / "bin/origami", bundle / "bin/instigator",
             bundle / "libexec/sgi/qemu-system-mips64", bundle / "libexec/sgi/qemu-img"]
    for root in roots:
        if not root.is_file():
            raise RuntimeError(f"missing packaged executable: {root}")
    cellar = Path(output("brew", "--cellar")).resolve()
    libdir = bundle / "lib/sgi"
    notices = bundle / "share/sgi/licenses/homebrew"
    shutil.rmtree(libdir, ignore_errors=True)
    shutil.rmtree(notices, ignore_errors=True)
    libdir.mkdir(parents=True)
    notices.mkdir(parents=True)
    destinations = {}
    used_names = {}
    links = {}
    queue = [(root, root) for root in roots]
    while queue:
        loader, executable = queue.pop()
        key = (loader, executable)
        if key in links:
            continue
        resolved = []
        for name in load_commands(loader):
            if system_library(name):
                continue
            source = resolve_name(name, loader, executable)
            if source not in destinations:
                basename = Path(name).name
                other = used_names.get(basename)
                if other is not None and other != source:
                    raise RuntimeError(f"conflicting Mach-O library name {basename}: {other} and {source}")
                used_names[basename] = source
                destinations[source] = libdir / basename
                shutil.copy2(source, destinations[source])
            resolved.append((name, source))
            queue.append((source, executable))
        links[key] = resolved
    formulas = {}
    kegs = {}
    for source in destinations:
        formula = formula_for(source, cellar)
        keg = cellar / formula / source.relative_to(cellar).parts[1]
        if formula in kegs and kegs[formula] != keg:
            raise RuntimeError(f"multiple installed versions of {formula} are required")
        formulas[source] = formula
        kegs[formula] = keg
    for formula, keg in sorted(kegs.items()):
        copy_notice(formula, keg, notices)
    manifest = ["library\thomebrew formula\tsource\n"]
    for source, target in sorted(destinations.items(), key=lambda pair: pair[1].name):
        manifest.append(f"{target.name}\t{formulas[source]}\t{source}\n")
    (bundle / "share/sgi/macos-libraries.tsv").write_text("".join(manifest))
    rewrites = {}
    for (loader, _), dependencies in links.items():
        target = destinations.get(loader, loader)
        changes = rewrites.setdefault(target, {})
        for old, source in dependencies:
            library = destinations[source]
            replacement = f"@loader_path/{os.path.relpath(library, target.parent)}"
            if old in changes and changes[old] != replacement:
                raise RuntimeError(f"ambiguous Mach-O load path {old} in {target}")
            changes[old] = replacement
    for target, changes in rewrites.items():
        target.chmod(target.stat().st_mode | 0o200)
        for old, replacement in changes.items():
            subprocess.run(["install_name_tool", "-change", old, replacement,
                            str(target)], check=True)
    for library in destinations.values():
        subprocess.run(["install_name_tool", "-id",
                        f"@loader_path/{library.name}", str(library)], check=True)
    for target in [*roots, *destinations.values()]:
        for name in load_commands(target):
            if system_library(name):
                continue
            dependency = resolve_name(name, target, target)
            if not dependency.is_relative_to(libdir):
                raise RuntimeError(f"{target} still loads a library outside the bundle: {name}")
        subprocess.run(["codesign", "--force", "--sign", "-", str(target)], check=True)
    print(f"bundled {len(destinations)} macOS libraries")


if __name__ == "__main__":
    main()
