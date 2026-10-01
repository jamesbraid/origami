import importlib.util
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

SCRIPT = Path(__file__).resolve().parents[1] / "collect-homebrew-notices.py"
SPEC = importlib.util.spec_from_file_location("collect_homebrew_notices", SCRIPT)
COLLECTOR = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(COLLECTOR)
PACKAGER_PATH = Path(__file__).resolve().parents[1] / "bundle-macos.py"
PACKAGER_SPEC = importlib.util.spec_from_file_location("bundle_macos", PACKAGER_PATH)
PACKAGER = importlib.util.module_from_spec(PACKAGER_SPEC)
PACKAGER_SPEC.loader.exec_module(PACKAGER)


class HomebrewNoticeTests(unittest.TestCase):
    def test_qemu_roots_use_the_shared_lib_directory(self):
        bundle = Path("/tmp/preview")
        groups = PACKAGER.bundle_groups(bundle)
        self.assertEqual(groups[0][2], "@executable_path/../lib/sgi/bin/")
        self.assertEqual(groups[1][2], "@executable_path/../../lib/sgi/qemu/")

    def test_bundles_each_root_group_without_erasing_the_other_group(self):
        with tempfile.TemporaryDirectory() as temporary:
            bundle = Path(temporary) / "preview with spaces"
            groups = PACKAGER.bundle_groups(bundle)
            for roots, _, _ in groups:
                for root in roots:
                    root.parent.mkdir(parents=True, exist_ok=True)
                    root.write_bytes(b"executable")
            calls = []

            def dylibbundler(command, check):
                calls.append(command)
                destination = Path(command[command.index("-d") + 1])
                shutil.rmtree(destination, ignore_errors=True)
                destination.mkdir(parents=True)
                roots = [Path(command[index + 1]) for index, arg in enumerate(command[:-1])
                         if arg == "-x"]
                for root in roots:
                    (destination / f"lib{root.name}.dylib").write_bytes(b"library")

            _, _, libraries = PACKAGER.bundle_dependencies(bundle, dylibbundler)
            self.assertEqual(len(calls), 2)
            self.assertEqual([command.count("-x") for command in calls], [2, 2])
            self.assertEqual(len(libraries), 4)
            self.assertEqual({path.parent for path in libraries},
                             {bundle / "lib/sgi/bin", bundle / "lib/sgi/qemu"})
            self.assertTrue(all(any("preview with spaces" in arg for arg in call)
                                for call in calls))

    def test_rejects_a_missing_executable_relative_dependency(self):
        bundle = Path("/tmp/preview")
        executable = bundle / "bin/origami"
        target = executable
        libdir = bundle / "lib/sgi"
        output = (f"{target}:\n\t@executable_path/../lib/sgi/bin/missing.dylib "
                  "(compatibility version 1.0.0, current version 1.0.0)\n")
        with patch.object(PACKAGER.subprocess, "check_output", return_value=output):
            with self.assertRaisesRegex(RuntimeError, "missing or external bundled load path"):
                PACKAGER.validate_load_paths(target, executable, libdir)

    def test_rejects_a_library_present_in_multiple_formula_file_lists(self):
        listings = {
            "glib": [("libintl.8.dylib", "/opt/homebrew/Cellar/glib/2.80/lib/libintl.8.dylib")],
            "gettext": [("libintl.8.dylib", "/opt/homebrew/Cellar/gettext/0.22/lib/libintl.8.dylib")],
        }
        with self.assertRaisesRegex(RuntimeError, "ambiguous Homebrew owner"):
            COLLECTOR.formula_owners({"libintl.8.dylib"}, listings)

    def test_requires_one_installed_version_for_a_bundled_formula(self):
        listings = {
            "glib": [("libglib-2.0.0.dylib", "/opt/homebrew/Cellar/glib/2.80/lib/libglib-2.0.0.dylib")],
        }
        with self.assertRaisesRegex(RuntimeError, "multiple installed versions"):
            COLLECTOR.formula_owners(
                {"libglib-2.0.0.dylib"}, listings,
                {"glib": ["2.80", "2.78"]},
            )

    def test_matches_a_homebrew_symlink_alias_but_checks_resolved_keg_owner(self):
        with tempfile.TemporaryDirectory() as temporary:
            keg = Path(temporary) / "Cellar/glib/2.80"
            versioned = keg / "lib/libglib-2.0.0.8000.0.dylib"
            versioned.parent.mkdir(parents=True)
            versioned.write_bytes(b"library")
            alias = versioned.parent / "libglib-2.0.0.dylib"
            alias.symlink_to(versioned.name)
            owners = COLLECTOR.formula_owners(
                {alias.name},
                {"glib": [(alias.name, str(alias.resolve()))]},
                {"glib": ["2.80"]}, {"glib": keg},
            )
            self.assertEqual(owners, {"glib": {alias.name}})
            other_keg = Path(temporary) / "Cellar/gettext/0.22"
            with self.assertRaisesRegex(RuntimeError, "escapes its installed keg"):
                COLLECTOR.formula_owners(
                    {alias.name},
                    {"glib": [(alias.name, str(alias.resolve()))]},
                    {"glib": ["2.80"]}, {"glib": other_keg},
                )

    def test_uses_installed_keg_when_formula_name_now_resolves_to_a_replacement(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            cellar = root / "Cellar"
            keg = cellar / "sdl2/2.32.10"
            library = keg / "lib/libSDL2-2.0.0.dylib"
            library.parent.mkdir(parents=True)
            library.write_bytes(b"library")
            (keg / "LICENSE.txt").write_text("installed SDL2 notice")
            bundled = root / "bundle/lib/sgi" / library.name
            bundled.parent.mkdir(parents=True)
            bundled.write_bytes(b"library")
            replies = {
                ("brew", "--cellar"): str(cellar),
                ("brew", "list", "--formula"): "sdl2",
                ("brew", "list", "--versions", "sdl2"): "sdl2 2.32.10",
                ("brew", "--prefix", "sdl2"): str(root / "opt/sdl2-compat"),
                ("brew", "list", "--verbose", "sdl2"): str(library),
            }
            with patch.object(COLLECTOR, "output", side_effect=lambda *args: replies[args]), \
                 patch.object(sys, "argv", [str(SCRIPT), str(root / "bundle"), str(root / "scratch")]):
                COLLECTOR.main()
            copied = root / "bundle/share/sgi/licenses/homebrew/sdl2/LICENSE.txt"
            self.assertEqual(copied.read_text(), "installed SDL2 notice")

    def test_uses_listed_library_keg_when_an_older_version_is_also_installed(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            cellar = root / "Cellar"
            keg = cellar / "glib/2.88.2"
            library = keg / "lib/libglib-2.0.0.dylib"
            library.parent.mkdir(parents=True)
            library.write_bytes(b"library")
            (keg / "COPYING").write_text("current GLib notice")
            bundled = root / "bundle/lib/sgi" / library.name
            bundled.parent.mkdir(parents=True)
            bundled.write_bytes(b"library")
            replies = {
                ("brew", "--cellar"): str(cellar),
                ("brew", "list", "--formula"): "glib",
                ("brew", "list", "--versions", "glib"): "glib 2.88.0 2.88.2",
                ("brew", "list", "--verbose", "glib"): str(library),
            }
            with patch.object(COLLECTOR, "output", side_effect=lambda *args: replies[args]), \
                 patch.object(sys, "argv", [str(SCRIPT), str(root / "bundle"), str(root / "scratch")]):
                COLLECTOR.main()
            self.assertIn("glib\t2.88.2", (root / "bundle/share/sgi/macos-libraries.tsv").read_text())
            copied = root / "bundle/share/sgi/licenses/homebrew/glib/COPYING"
            self.assertEqual(copied.read_text(), "current GLib notice")

    def test_uses_installed_notices_before_source_unpack(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            keg = root / "Cellar" / "glib" / "2.80"
            notice = keg / "share/doc/glib/COPYING"
            notice.parent.mkdir(parents=True)
            notice.write_text("license text")
            bundled = root / "bundle/lib/sgi/libglib-2.0.0.dylib"
            bundled.parent.mkdir(parents=True)
            bundled.write_bytes(b"dylib")
            manifest = COLLECTOR.collect_notices(
                root / "bundle", root / "scratch",
                {"glib": keg}, {"glib": ["2.80"]},
                {"glib": [("libglib-2.0.0.dylib", str(keg / "lib/libglib-2.0.0.dylib"))]},
            )
            self.assertIn("glib\t2.80", manifest.read_text())
            copied = root / "bundle/share/sgi/licenses/homebrew/glib/COPYING"
            self.assertEqual(copied.read_text(), "license text")

    def test_unpacks_exact_installed_formula_when_bottle_has_no_notice(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            keg = root / "Cellar/glib/2.80"
            formula = keg / ".brew/glib.rb"
            formula.parent.mkdir(parents=True)
            formula.write_text("class Glib < Formula; end")
            bundle = root / "bundle"
            library = bundle / "lib/sgi/libglib-2.0.0.dylib"
            library.parent.mkdir(parents=True)
            library.write_bytes(b"dylib")
            unpacked = root / "scratch/unpacked/glib/glib-2.80"

            def unpack(command, check):
                self.assertEqual(command[0:2], ["brew", "unpack"])
                self.assertNotIn("--patch", command)
                self.assertEqual(command[2], f"--destdir={root / 'scratch/unpacked/glib'}")
                self.assertEqual(command[-1], str(formula))
                unpacked.mkdir(parents=True)
                (unpacked / "COPYING").write_text("source license")

            with patch.object(COLLECTOR.subprocess, "run", side_effect=unpack):
                COLLECTOR.collect_notices(
                    bundle, root / "scratch", {"glib": keg}, {"glib": ["2.80"]},
                    {"glib": [("libglib-2.0.0.dylib", str(keg / "lib/libglib-2.0.0.dylib"))]},
                )
            copied = bundle / "share/sgi/licenses/homebrew/glib/COPYING"
            self.assertEqual(copied.read_text(), "source license")

    @unittest.skipUnless(
        sys.platform == "darwin"
        and all(shutil.which(tool) for tool in ("clang", "dylibbundler", "otool", "codesign")),
        "native macOS bundler tools are unavailable",
    )
    def test_native_bundle_runs_after_source_libraries_are_removed(self):
        with tempfile.TemporaryDirectory(prefix="macos bundle smoke ") as temporary:
            root = Path(temporary)
            source = root / "fixture source"
            libraries = source / "lib"
            libraries.mkdir(parents=True)
            bundle = root / "relocated bundle"
            roots = PACKAGER.bundle_groups(bundle)
            executables = [path for group, _, _ in roots for path in group]
            for executable in executables:
                executable.parent.mkdir(parents=True, exist_ok=True)

            indirect_source = source / "indirect.c"
            indirect_source.write_text("int indirect_value(void) { return 42; }\n")
            direct_source = source / "direct.c"
            direct_source.write_text(
                "extern int indirect_value(void);\n"
                "int direct_value(void) { return indirect_value(); }\n"
            )
            main_source = source / "main.c"
            main_source.write_text(
                "extern int direct_value(void);\n"
                "int main(void) { return direct_value() == 42 ? 0 : 1; }\n"
            )
            indirect = libraries / "libindirect.dylib"
            direct = libraries / "libdirect.dylib"
            subprocess.run([
                "clang", "-dynamiclib", str(indirect_source),
                f"-Wl,-install_name,{indirect}", "-o", str(indirect),
            ], check=True)
            subprocess.run([
                "clang", "-dynamiclib", str(direct_source), "-L", str(libraries),
                "-lindirect", f"-Wl,-install_name,{direct}", "-o", str(direct),
            ], check=True)
            for executable in executables:
                subprocess.run([
                    "clang", str(main_source), "-L", str(libraries), "-ldirect",
                    "-o", str(executable),
                ], check=True)

            with patch.object(sys, "argv", [str(PACKAGER_PATH), str(bundle)]):
                PACKAGER.main()
            shutil.rmtree(source)
            for executable in executables:
                subprocess.run([str(executable)], check=True, timeout=10)


if __name__ == "__main__":
    unittest.main()
