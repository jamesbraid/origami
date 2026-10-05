#!/usr/bin/env python3
"""Check an extracted package without firmware or guest media."""
from __future__ import annotations

import argparse
from contextlib import contextmanager
import hashlib
import json
import os
import shutil
import signal
import socket
from pathlib import Path
import struct
import sys
import subprocess
import tempfile
import time
import tomllib
import unittest


@contextmanager
def machine_directory(scratch):
    root = Path(tempfile.mkdtemp(prefix="origami smoke ", dir=scratch))
    try:
        yield root
    except BaseException:
        print(f"Smoke test files retained at {root}", file=sys.stderr)
        raise
    else:
        shutil.rmtree(root)


class ProductSmoke(unittest.TestCase):
    package: Path
    scratch: Path
    commands: list[dict] = []

    def run_binary(self, relative, *args, success=True):
        binary = self.package / relative
        if os.name == "nt":
            binary = binary.with_suffix(".exe")
        env = os.environ.copy()
        for name in ("ORIGAMI_RUNTIME_DIR", "LD_LIBRARY_PATH", "DYLD_LIBRARY_PATH"):
            env.pop(name, None)
        background = relative == "bin/origami" and args[0] == "run" and "--background" in args
        with subprocess.Popen([str(binary), *map(str, args)], stdout=subprocess.PIPE,
                              stderr=subprocess.PIPE, text=True, env=env,
                              start_new_session=background and os.name == "posix") as process:
            if background and success and os.name == "posix":
                self.process_group = process.pid
            try:
                stdout, stderr = process.communicate(timeout=30)
            except subprocess.TimeoutExpired:
                if background and os.name == "posix":
                    os.killpg(process.pid, signal.SIGKILL)
                else:
                    process.kill()
                process.communicate()
                raise
            result = subprocess.CompletedProcess(process.args, process.returncode, stdout, stderr)
        self.commands.append({"binary": relative, "args": list(map(str, args)),
                              "exit": result.returncode, "stdout": result.stdout,
                              "stderr": result.stderr})
        self.assertEqual(result.returncode == 0, success,
                         f"{binary} {args}: {result.stdout}\n{result.stderr}")
        return result.stdout

    def cli(self, *args, **kwargs):
        return self.run_binary("bin/origami", *args, **kwargs)

    def test_packaged_executables(self):
        self.assertIn("\nqemu ", self.cli("version"))
        self.assertIn("origin200-1", self.cli("machines"))
        self.run_binary("bin/instigator", "--help")
        self.run_binary("libexec/origami/qemu-system-mips64", "--version")
        self.assertIn("sdl", self.run_binary("libexec/origami/qemu-system-mips64", "-display", "help"))
        self.run_binary("libexec/origami/qemu-img", "--version")
        self.run_binary("libexec/origami/qemu-sgi-machine-init", "--machine", "help")

    def test_remote_install_configuration(self):
        with machine_directory(self.scratch) as root:
            rejected = root / "invalid machine"
            self.cli("create", rejected, "--preset", "origin200-1",
                     "--memory-per-node", "96", success=False)
            self.assertIn("not offered", self.commands[-1]["stderr"])
            self.assertFalse(rejected.exists())
            prom = root / "synthetic prom.bin"
            prom.write_bytes(struct.pack(">II", 0x1000FFFF, 0) + bytes(1024 * 1024 - 8))
            machine = root / "remote install machine"
            self.cli("create", machine, "--preset", "origin200-1", "--prom", prom)
            self.cli("install-init", machine, "--mac", "08:00:69:12:34:56")
            media = tomllib.loads((machine / "install/media.toml").read_text())
            self.assertEqual(len(media["media"]), 10)
            self.assertTrue({"devlibs", "devfoundation"}.issubset(media["media"]))
            self.assertTrue(all(source.endswith(".iso") for source in media["media"].values()))
            self.assertEqual(media["media"]["overlays1"],
                             "https://origami-dist.irix.fans/irix/6.5.30/overlays1.iso")
            self.assertTrue(all(source.startswith("https://origami-dist.irix.fans/irix/")
                                for source in media["media"].values()))
            self.assertFalse((machine / "install/cache").exists())

    def test_machine_lifecycle_and_drive_lock(self):
        with machine_directory(self.scratch) as root:
            prom = root / "synthetic prom.bin"
            # MIPS branch-to-self and its delay slot. This is not SGI firmware.
            prom.write_bytes(struct.pack(">II", 0x1000FFFF, 0) + bytes(1024 * 1024 - 8))
            machine = root / "machine with spaces"
            self.cli("create", machine, "--preset", "origin200-1", "--prom", prom,
                     "--memory-per-node", "128")
            self.cli("create", machine, "--preset", "origin200-1", "--prom", prom, success=False)
            self.cli("drive-create", machine, "16")
            disk = machine / "drives/system.qcow2"
            before = hashlib.sha256(disk.read_bytes()).hexdigest()
            self.cli("drive-create", machine, "16", success=False)
            self.assertEqual(hashlib.sha256(disk.read_bytes()).hexdigest(), before)
            info = json.loads(self.run_binary("libexec/origami/qemu-img", "info", "--output=json", disk))
            self.assertEqual(info["format"], "qcow2")
            self.assertEqual(info["virtual-size"], 16 * 1024 * 1024)
            self.cli("validate", machine)
            self.assertEqual(self.cli("status", machine).strip(), "stopped")
            try:
                with socket.socket() as listener:
                    listener.bind(("127.0.0.1", 0))
                    port = listener.getsockname()[1]
                self.assertGreaterEqual(port, 5900)
                self.cli("run", machine, "--display", "vnc", "--vnc-port", port, "--background")
                with socket.create_connection(("127.0.0.1", port), timeout=5) as vnc:
                    self.assertTrue(vnc.recv(12).startswith(b"RFB "))
                self.assertTrue(self.cli("status", machine).startswith("running:"))
                self.cli("run", machine, "--display", "none", "--background", success=False)
                configuration = (machine / "machine.toml").read_bytes()
                self.cli("drive-detach", machine, "system", success=False)
                self.assertEqual((machine / "machine.toml").read_bytes(), configuration)
            finally:
                try:
                    if self.cli("status", machine).startswith("running:"):
                        self.cli("stop", machine)
                        deadline = time.monotonic() + 10
                        while self.cli("status", machine).strip() != "stopped":
                            self.assertLess(time.monotonic(), deadline, "QEMU did not stop")
                            time.sleep(0.1)
                finally:
                    # The launcher and its children inherit this test's new session.
                    # Kill only that group if QMP or startup failed to clean it up.
                    group = getattr(self, "process_group", None)
                    if group is not None:
                        try:
                            os.killpg(group, signal.SIGKILL)
                        except ProcessLookupError:
                            pass
            self.cli("drive-detach", machine, "system")
            self.assertTrue(disk.exists(), "detaching deleted the disk")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("package", type=Path)
    parser.add_argument("--scratch", required=True, type=Path)
    parser.add_argument("--report", type=Path)
    args = parser.parse_args()
    ProductSmoke.package = args.package.resolve(strict=True)
    ProductSmoke.scratch = args.scratch.resolve(strict=True)
    suite = unittest.defaultTestLoader.loadTestsFromTestCase(ProductSmoke)
    result = unittest.TextTestRunner(verbosity=2).run(suite)
    if args.report:
        args.report.write_text(json.dumps({"passed": result.wasSuccessful(),
                                         "tests": result.testsRun,
                                         "commands": ProductSmoke.commands}, indent=2) + "\n")
    raise SystemExit(not result.wasSuccessful())


if __name__ == "__main__":
    main()
