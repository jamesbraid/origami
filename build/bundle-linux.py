#!/usr/bin/env python3
"""Copy QEMU's ELF dependencies and their license notices into a bundle."""

import os
import re
import shutil
import subprocess
import sys
from pathlib import Path


SYSTEM_LIBRARIES = {
    "ld-linux-x86-64.so.2",
    "libc.so.6",
    "libdl.so.2",
    "libm.so.6",
    "libmvec.so.1",
    "libpthread.so.0",
    "libresolv.so.2",
    "librt.so.1",
}


def run(*args):
    return subprocess.check_output(args, text=True)


def dependencies(binary):
    result = {}
    for line in run("ldd", str(binary)).splitlines():
        if "=> not found" in line:
            raise RuntimeError(f"unresolved dependency of {binary}: {line.strip()}")
        match = re.match(r"\s*(\S+) => (/.+?) \(0x[0-9a-f]+\)\s*$", line)
        if match and match.group(1) not in SYSTEM_LIBRARIES:
            result[match.group(1)] = Path(match.group(2))
    return result


def package_for(path):
    for candidate in (path, path.resolve()):
        query = subprocess.run(
            ["dpkg-query", "-S", str(candidate)],
            capture_output=True,
            text=True,
        )
        if query.returncode == 0:
            return query.stdout.split(": ", 1)[0].split(",", 1)[0]
    raise RuntimeError(f"no Debian package owns {path}")


def main():
    if len(sys.argv) != 2:
        raise SystemExit(f"usage: {sys.argv[0]} BUNDLE")
    bundle = Path(sys.argv[1]).resolve()
    binaries = [
        bundle / "bin/origami",
        bundle / "bin/qemu-sgi-firmware",
        bundle / "libexec/sgi/qemu-system-mips64",
        bundle / "libexec/sgi/qemu-img",
    ]
    needed = {binary: dependencies(binary) for binary in binaries}
    libraries = {}
    for entries in needed.values():
        libraries.update(entries)
    libdir = bundle / "lib/sgi"
    notices = bundle / "share/sgi/licenses/debian"
    shutil.rmtree(libdir, ignore_errors=True)
    shutil.rmtree(notices, ignore_errors=True)
    libdir.mkdir(parents=True, exist_ok=True)
    notices.mkdir(parents=True, exist_ok=True)
    package_records = {}
    for name, source in sorted(libraries.items()):
        shutil.copy2(source, libdir / name, follow_symlinks=True)
        package = package_for(source)
        metadata = run(
            "dpkg-query", "-W",
            "-f=${binary:Package}\t${Version}\t${source:Package}\t${source:Version}",
            package,
        ).split("\t")
        if len(metadata) != 4 or not all(metadata):
            raise RuntimeError(f"incomplete Debian package metadata for {package}")
        package_records[name] = metadata
        copyright_file = Path("/usr/share/doc") / package.split(":", 1)[0] / "copyright"
        if not copyright_file.is_file():
            raise RuntimeError(f"missing license notice for {package}: {copyright_file}")
        shutil.copy2(copyright_file, notices / f"{package.replace(':', '_')}.copyright")
    manifest = [
        "library\tdebian package\tdebian package version\t"
        "debian source package\tdebian source version\n"
    ]
    manifest.extend(
        f"{name}\t{package}\t{package_version}\t{source_package}\t{source_version}\n"
        for name, (package, package_version, source_package, source_version)
        in sorted(package_records.items())
    )
    (bundle / "share/sgi/debian-libraries.tsv").write_text("".join(manifest))
    for binary in binaries:
        relative = os.path.relpath(libdir, binary.parent)
        subprocess.run(["patchelf", "--set-rpath", f"$ORIGIN/{relative}", str(binary)], check=True)
    for library in libdir.iterdir():
        subprocess.run(["patchelf", "--set-rpath", "$ORIGIN", str(library)], check=True)
    for binary, entries in needed.items():
        resolved = dependencies(binary)
        for name in entries:
            if resolved.get(name, Path()).resolve() != (libdir / name).resolve():
                raise RuntimeError(f"{name} did not resolve inside bundle for {binary}")
    print(f"bundled {len(libraries)} shared libraries")


if __name__ == "__main__":
    main()
