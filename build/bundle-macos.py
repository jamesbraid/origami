#!/usr/bin/env python3
"""Copy Mach-O dependencies into a macOS bundle with dylibbundler."""

import shutil
import subprocess
import sys
from pathlib import Path


def bundle_groups(bundle):
    return [
        ([bundle / "bin/origami", bundle / "bin/instigator"],
         bundle / "lib/sgi/bin", "@executable_path/../lib/sgi/bin/"),
        ([bundle / "libexec/sgi/qemu-system-mips64", bundle / "libexec/sgi/qemu-img"],
         bundle / "lib/sgi/qemu", "@executable_path/../../lib/sgi/qemu/"),
    ]


def load_names(path):
    output = subprocess.check_output(["otool", "-L", str(path)], text=True)
    lines = [line.strip().split(" (", 1)[0] for line in output.splitlines()[1:]
             if line.strip()]
    return lines[1:] if path.suffix == ".dylib" else lines


def system_library(name):
    return name.startswith(("/usr/lib/", "/System/Library/"))


def validate_load_paths(target, executable, libdir):
    for name in load_names(target):
        if system_library(name):
            continue
        if name.startswith("@executable_path/"):
            candidate = executable.parent / name[len("@executable_path/"):]
        elif name.startswith("@loader_path/"):
            candidate = target.parent / name[len("@loader_path/"):]
        else:
            raise RuntimeError(f"{target} has an unsupported non-system load path: {name}")
        if not candidate.is_file() or not candidate.resolve().is_relative_to(libdir.resolve()):
            raise RuntimeError(f"{target} has a missing or external bundled load path: {name}")


def bundle_dependencies(bundle, run=subprocess.run):
    libdir = bundle / "lib/sgi"
    shutil.rmtree(libdir, ignore_errors=True)
    libdir.mkdir(parents=True)
    libraries = []
    groups = bundle_groups(bundle)
    for roots, destination, prefix in groups:
        destination.mkdir(parents=True)
        command = ["dylibbundler", "-od", "-b"]
        for root in roots:
            command.extend(["-x", str(root)])
        command.extend(["-d", str(destination), "-p", prefix])
        run(command, check=True)
        libraries.extend(path for path in destination.rglob("*.dylib") if path.is_file())
    return groups, libdir, libraries


def main():
    if len(sys.argv) != 2:
        raise SystemExit(f"usage: {sys.argv[0]} BUNDLE")
    bundle = Path(sys.argv[1]).resolve()
    groups = bundle_groups(bundle)
    roots = [root for group, _, _ in groups for root in group]
    for root in roots:
        if not root.is_file():
            raise RuntimeError(f"missing packaged executable: {root}")
    for tool in ("dylibbundler", "otool", "codesign"):
        if not shutil.which(tool):
            raise RuntimeError(f"missing macOS packaging tool: {tool}")

    groups, libdir, libraries = bundle_dependencies(bundle)

    for group, destination, _ in groups:
        targets = [*group, *(path for path in libraries if path.is_relative_to(destination))]
        for executable in group:
            for target in targets:
                validate_load_paths(target, executable, libdir)
    for target in [*roots, *libraries]:
        subprocess.run(["codesign", "--force", "--sign", "-", str(target)], check=True)
    print(f"bundled {len(libraries)} macOS libraries")


if __name__ == "__main__":
    main()
