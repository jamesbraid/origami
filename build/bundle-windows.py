#!/usr/bin/env python3
"""Collect MinGW DLLs and their license notices for the Windows archive."""

import re
import shutil
import subprocess
import sys
from pathlib import Path


SYSROOT = Path("/usr/x86_64-w64-mingw32/sys-root/mingw/bin")
SYSTEM_DLLS = {
    "advapi32.dll", "bcrypt.dll", "bcryptprimitives.dll", "cfgmgr32.dll",
    "comctl32.dll", "comdlg32.dll", "crypt32.dll", "d3d11.dll", "d3d9.dll",
    "dnsapi.dll", "dwmapi.dll", "dxgi.dll", "gdi32.dll", "imm32.dll", "iphlpapi.dll",
    "kernel32.dll", "msvcrt.dll", "ntdll.dll", "ole32.dll", "oleaut32.dll",
    "setupapi.dll", "shell32.dll", "shlwapi.dll", "user32.dll", "userenv.dll",
    "uxtheme.dll", "version.dll", "winmm.dll", "ws2_32.dll",
}
# Fedora's SDL2 compatibility DLL loads SDL3 at runtime, outside the PE import table.
RUNTIME_DLLS = {"sdl2.dll": ("SDL3.dll",)}


def output(*args):
    return subprocess.check_output(args, text=True)


def imports(path):
    return re.findall(r"^\s*DLL Name:\s*(\S+)", output("x86_64-w64-mingw32-objdump", "-p", str(path)), re.M)


def main():
    if len(sys.argv) != 2:
        raise SystemExit(f"usage: {sys.argv[0]} BUNDLE")
    bundle = Path(sys.argv[1]).resolve()
    available = {path.name.lower(): path for path in SYSROOT.glob("*.dll")}
    notices = bundle / "share/sgi/licenses/fedora"
    shutil.rmtree(notices, ignore_errors=True)
    notices.mkdir(parents=True)
    manifest = ["directory\tdll\tfedora package\tlicense\n"]
    for directory, names in (("bin", ("sgi.exe", "instigator.exe")),
                             ("libexec/sgi", ("qemu-system-mips64.exe", "qemu-img.exe"))):
        destination = bundle / directory
        for stale in destination.iterdir():
            if stale.suffix.lower() == ".dll":
                stale.unlink()
        queue = [destination / name for name in names]
        copied = set()
        while queue:
            module = queue.pop()
            for name in (*imports(module), *RUNTIME_DLLS.get(module.name.lower(), ())):
                key = name.lower()
                if key in copied:
                    continue
                source = available.get(key)
                if source is None:
                    if key in SYSTEM_DLLS or key.startswith(("api-ms-win-", "ext-ms-win-")):
                        continue
                    raise RuntimeError(f"unresolved Windows DLL import: {name}")
                target = destination / source.name
                shutil.copy2(source, target)
                copied.add(key)
                queue.append(target)
                package = output("rpm", "-qf", "--qf", "%{NAME}", str(source))
                license_name = output("rpm", "-q", "--qf", "%{LICENSE}", package)
                licenses = [Path(path) for path in output("rpm", "-ql", package).splitlines()
                            if Path(path).is_file() and (
                                path.startswith(f"/usr/share/licenses/{package}/")
                                or (path.startswith(f"/usr/share/doc/{package}/")
                                    and Path(path).name.lower().startswith(
                                        ("license", "licence", "copying", "notice", "copyright"))))]
                license_dir = notices / package
                license_dir.mkdir(exist_ok=True)
                for path in licenses:
                    shutil.copy2(path, license_dir / path.name)
                if package == "mingw64-zlib" and not licenses:
                    header = Path("/usr/x86_64-w64-mingw32/sys-root/mingw/include/zlib.h").read_text()
                    (license_dir / "zlib.LICENSE.txt").write_text(header.split("*/", 1)[0] + "*/\n")
                elif "Public-Domain" in license_name and not licenses:
                    (license_dir / "FEDORA-LICENSE.txt").write_text(
                        f"Fedora declares {package} as {license_name}.\n")
                elif not licenses:
                    raise RuntimeError(f"no license files installed for {package} ({source.name})")
                manifest.append(f"{directory}\t{source.name}\t{package}\t{license_name}\n")
    (bundle / "share/sgi/windows-dlls.tsv").write_text("".join(manifest))
    print(f"bundled {len(manifest) - 1} Windows DLL copies")


if __name__ == "__main__":
    main()
