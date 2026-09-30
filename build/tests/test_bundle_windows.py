import importlib.util
import shutil
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

SCRIPT = Path(__file__).resolve().parents[1] / "bundle-windows.py"
SPEC = importlib.util.spec_from_file_location("bundle_windows", SCRIPT)
BUNDLER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(BUNDLER)


class WindowsRuntimeDependencyTests(unittest.TestCase):
    def test_uses_cmake_runtime_dependency_helper_for_pe_binaries(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            executable = root / "origami.exe"
            sysroot = root / "sysroot"
            sysroot.mkdir()
            dependency = sysroot / "SDL2.dll"
            dependency.touch()
            def run(command, check):
                self.assertEqual(command[0], "cmake")
                self.assertIn(f"-DROOTS={executable}", command)
                search_arg = next(
                    argument for argument in command
                    if argument.startswith("-DSEARCH_DIRECTORIES=")
                )
                search_directory = Path(search_arg.split("=", 1)[1])
                self.assertEqual(search_directory.name, "dlls")
                self.assertEqual(
                    (search_directory / dependency.name.lower()).resolve(), dependency,
                )
                self.assertEqual(
                    (search_directory / dependency.name).resolve(), dependency,
                )
                self.assertEqual(command[-2:], [
                    "-P", str(BUNDLER.RUNTIME_DEPENDENCIES_SCRIPT),
                ])
                output_arg = next(
                    argument for argument in command if argument.startswith("-DOUTPUT=")
                )
                Path(output_arg[len("-DOUTPUT="):]).write_text(str(dependency) + "\n")

            with patch.object(BUNDLER, "SYSROOT", sysroot), \
                    patch.object(BUNDLER.subprocess, "run", side_effect=run):
                found = BUNDLER.runtime_dependencies([executable])

            self.assertEqual(found, [dependency])

    def test_copies_scanned_dll_and_exact_fedora_notice(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            bundle = root / "bundle"
            for directory, names in (("bin", ("origami.exe", "instigator.exe")),
                                     ("libexec/sgi", ("qemu-system-mips64.exe", "qemu-img.exe"))):
                destination = bundle / directory
                destination.mkdir(parents=True)
                for name in names:
                    (destination / name).touch()
            (bundle / "share/sgi/licenses").mkdir(parents=True)
            sysroot = root / "sysroot"
            sysroot.mkdir()
            dll = sysroot / "SDL2.dll"
            dll.write_text("PE fixture")
            stale = bundle / "bin/stale.dll"
            stale.touch()
            keep = bundle / "bin/keep.txt"
            keep.touch()
            rpm_notice = "/usr/share/licenses/test-package/COPYING"

            def rpm_output(*command):
                if command[:2] == ("rpm", "-qf"):
                    return "test-package"
                if command[:3] == ("rpm", "-q", "--qf"):
                    if command[3] == "%{LICENSE}":
                        return "BSD-3-Clause"
                    if command[3] == "%{VERSION}-%{RELEASE}\t%{SOURCERPM}":
                        return "2.4.6-1.fc43\ttest-source-2.4.6-1.fc43.src.rpm"
                if command[:2] == ("rpm", "-ql"):
                    return rpm_notice
                raise AssertionError(f"unexpected command: {command}")

            def copy(source, target):
                if str(source) == rpm_notice:
                    Path(target).write_text("Fedora copyright notice")
                else:
                    shutil.copyfile(source, target)

            with patch.object(BUNDLER, "SYSROOT", sysroot), \
                    patch.object(BUNDLER, "runtime_dependencies", side_effect=([dll], [])), \
                    patch.object(BUNDLER, "output", side_effect=rpm_output), \
                    patch.object(BUNDLER.Path, "is_file", autospec=True,
                                 side_effect=lambda path: str(path) == rpm_notice), \
                    patch.object(BUNDLER.shutil, "copy2", side_effect=copy), \
                    patch.object(BUNDLER.sys, "argv", [str(SCRIPT), str(bundle)]):
                BUNDLER.main()

            self.assertEqual((bundle / "bin/SDL2.dll").read_text(), "PE fixture")
            self.assertFalse(stale.exists())
            self.assertTrue(keep.exists())
            notice = bundle / "share/sgi/licenses/fedora/test-package/COPYING"
            self.assertEqual(notice.read_text(), "Fedora copyright notice")
            manifest = (bundle / "share/sgi/windows-dlls.tsv").read_text()
            self.assertEqual(
                manifest,
                "directory\tdll\tfedora package\tlicense\tfedora package version\tsource RPM\n"
                "bin\tSDL2.dll\ttest-package\tBSD-3-Clause\t2.4.6-1.fc43\t"
                "test-source-2.4.6-1.fc43.src.rpm\n",
            )


if __name__ == "__main__":
    unittest.main()
