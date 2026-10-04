"""Exercise the product build graph without compiling external projects."""
import json
import os
from pathlib import Path
import shutil
import sys
import subprocess
import tempfile
import time
import unittest

ROOT = Path(__file__).resolve().parents[2]


class ProductBuild(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="cmake product ")
        self.addCleanup(self.temp.cleanup)
        self.source = Path(self.temp.name) / "source tree"
        self.binary = Path(self.temp.name) / "build tree"
        self.source.mkdir()
        if (ROOT / "CMakeLists.txt").exists():
            shutil.copy(ROOT / "CMakeLists.txt", self.source)
            shutil.copy(ROOT / "CMakePresets.json", self.source)
            shutil.copytree(ROOT / "cmake", self.source / "cmake")
        (self.source / "build/tests").mkdir(parents=True)
        (self.source / "qemu/pc-bios/keymaps").mkdir(parents=True)
        (self.source / "qemu/pc-bios/keymaps/en-us").write_text("keymap")
        (self.source / "qemu/pc-bios/keymaps/meson.build").write_text("keymap build")
        (self.source / "qemu/util").mkdir(parents=True)
        (self.source / "qemu/include/sgi").mkdir(parents=True)
        (self.source / "qemu/include/sgi/flash-image.h").write_text("#define FIXTURE_VALUE 2\n")
        (self.source / "qemu/include/sgi/prom-image.h").write_text("int fixture_prom(void);\n")
        (self.source / "qemu/util/sgi-prom-image.c").write_text(
            '#include <zlib.h>\nint fixture_prom(void) { return zlibVersion()[0] != 0; }\n')
        (self.source / "qemu/util/sgi-flash-image.c").write_text(
            '#include "sgi/flash-image.h"\n#include "sgi/prom-image.h"\n'
            'int fixture_table(void);\nint firmware_fixture(void) { return fixture_prom() + FIXTURE_VALUE + fixture_table(); }\n')
        (self.source / "qemu/util/sgi-flash-layouts.c").write_text(
            'int fixture_table(void) { return 4; }\n')
        (self.source / "instigator").mkdir()
        (self.source / "src").mkdir()
        (self.source / "src/main.rs").write_text("first rust")
        (self.source / "instigator/main.go").write_text("first go")
        (self.source / "instigator/go.mod").write_text("module fixture")
        (self.source / "qemu/source.c").write_text("first qemu")
        (self.source / "qemu/VERSION").write_text("first version")
        (self.source / "qemu/meson.build").write_text("first configuration")
        (self.source / "qemu/subprojects/packagefiles").mkdir(parents=True)
        self.slirp_wrap = self.source / "qemu/subprojects/libslirp.wrap"
        self.slirp_wrap.write_text("[wrap-file]\ndiff_files = libslirp-test.patch\n")
        self.slirp_patch = self.source / "qemu/subprojects/packagefiles/libslirp-test.patch"
        self.slirp_patch.write_text("first slirp")
        self.log = Path(self.temp.name) / "commands.jsonl"
        self.env = dict(os.environ, FIXTURE_LOG=str(self.log), FIXTURE_BUILD_DIR=str(self.binary))
        self.tool = self.source / "tool"
        self.tool.write_text('''#!/usr/bin/env python3
import json, os, pathlib, subprocess, sys
args = sys.argv[1:]
with open(os.environ["FIXTURE_LOG"], "a") as f:
    f.write(json.dumps([pathlib.Path(sys.argv[0]).name, args]) + "\\n")
name = pathlib.Path(sys.argv[0]).name
if name == "configure":
    pathlib.Path("pyvenv/bin").mkdir(parents=True, exist_ok=True)
    if not pathlib.Path("pyvenv/bin/meson").exists():
        pathlib.Path("pyvenv/bin/meson").symlink_to(pathlib.Path(sys.argv[0]).parent.parent / "tool")
    root = pathlib.Path(sys.argv[0]).parent
    if not pathlib.Path("slirp-source").exists():
        pathlib.Path("slirp-source").write_text((root / "subprojects/packagefiles/libslirp-test.patch").read_text())
    prefix = next((arg.split("=", 1)[1] for arg in args if arg.startswith("--cross-prefix=")), "")
    pathlib.Path("cross-prefix").write_text(prefix)
    pathlib.Path("configuration-version").write_text((root / "VERSION").read_text())
    pathlib.Path("build.ninja").write_text("configured")
    pathlib.Path("configuration-source").write_text(str(pathlib.Path(sys.argv[0]).parent))
elif name == "meson":
    import configparser
    root = pathlib.Path(args[args.index("--sourcedir") + 1])
    wrap = configparser.ConfigParser()
    wrap.read(root / "subprojects/libslirp.wrap")
    contents = [(root / "subprojects/packagefiles" / patch.strip()).read_text()
                for patch in wrap["wrap-file"]["diff_files"].split(",")]
    if "INVALID" in contents:
        sys.exit("libslirp patch failed")
    pathlib.Path("slirp-source").write_text("\\n".join(contents))
elif name == "qemu-make":
    import subprocess
    root = pathlib.Path(pathlib.Path("configuration-source").read_text())
    if pathlib.Path("configuration-version").read_text() != (root / "VERSION").read_text():
        subprocess.run([str(root / "configure")], check=True)
    subprocess.run([str(root.parent / "ninja"), *args], check=True)
elif name == "ninja":
    root = pathlib.Path(pathlib.Path("configuration-source").read_text())
    pathlib.Path("configuration-meson").write_text((root / "meson.build").read_text())
    if "libsgi-firmware-core.a" in args:
        prefix = pathlib.Path("cross-prefix").read_text()
        objects = []
        for name in ("sgi-prom-image", "sgi-flash-image", "sgi-flash-layouts"):
            obj = pathlib.Path(name + ".o")
            subprocess.check_call([prefix + "cc" if not prefix else prefix + "gcc", "-I", str(root / "include"),
                                   "-c", str(root / "util" / (name + ".c")), "-o", str(obj)])
            objects.append(str(obj))
        subprocess.check_call([prefix + "ar", "rcs", "libsgi-firmware-core.a", *objects])
    for target in args:
        if target.startswith("qemu-"):
            content = (root / "source.c").read_text()
            if os.environ.get("FIXTURE_SLIRP_TEST"):
                content += "\\n" + pathlib.Path("slirp-source").read_text()
            pathlib.Path(target).write_text(content)
elif name == "cargo" and ("build" in args or "test" in args):
    archive = pathlib.Path(os.environ["SGI_FIRMWARE_ARCHIVE"])
    assert archive.is_file(), "Cargo started before the C archive was compiled"
    compiler = "x86_64-w64-mingw32-gcc" if "--target" in args else "cc"
    probe = pathlib.Path(os.environ["FIXTURE_BUILD_DIR"]) / ("firmware-probe.exe" if "--target" in args else "firmware-probe")
    source = probe.with_suffix(".c")
    source.write_text('#include <stdio.h>\\nint firmware_fixture(void);\\nint main(void) { printf("%d", firmware_fixture()); return 0; }\\n')
    subprocess.check_call([compiler, str(source), str(archive), "-lz", "-o", str(probe)])
    if "--target" in args:
        assert "--no-run" in args or "build" in args
        assert probe.read_bytes()[:2] == b"MZ"
    else:
        with (probe.parent / "firmware-results").open("a") as results:
            results.write(subprocess.check_output([str(probe)], text=True) + "\\n")
    if "test" in args: sys.exit(0)
    root = pathlib.Path(os.environ["CARGO_TARGET_DIR"])
    if "--target" in args: root /= args[args.index("--target") + 1]
    root /= "release" if "--release" in args else "debug"
    root.mkdir(parents=True, exist_ok=True)
    (root / "origami").write_text(pathlib.Path("src/main.rs").read_text())
elif name == "go" and "build" in args:
    pathlib.Path(args[args.index("-o") + 1]).write_text(pathlib.Path("main.go").read_text())
    (pathlib.Path(os.environ["FIXTURE_BUILD_DIR"]) / "go-environment.json").write_text(json.dumps({key: os.environ[key] for key in ("GOPATH", "GOMODCACHE", "GOCACHE", "CGO_ENABLED", "XDG_CONFIG_HOME") if key in os.environ}))
''')
        self.tool.chmod(0o755)
        for name in ("cargo", "rustc", "go", "ninja", "qemu-make"):
            (self.source / name).symlink_to(self.tool)
        (self.source / "qemu/configure").symlink_to(self.tool)
        (self.source / "build/stage-product.py").write_text('''import json, pathlib, shutil, subprocess, sys
m = json.loads(pathlib.Path(sys.argv[1]).read_text())
if "--prepare-qemu" in sys.argv:
    sys.exit(0)
out = pathlib.Path(sys.argv[sys.argv.index("--output")+1] if "--output" in sys.argv else m["output_dir"])
out.mkdir(parents=True, exist_ok=True)
for name, origin in [("origami", pathlib.Path(m["files"]["bin/origami"])), ("instigator", pathlib.Path(m["files"]["bin/instigator"])), ("qemu", pathlib.Path(m["files"]["libexec/sgi/qemu-system-mips64"]))]:
    shutil.copy(origin, out / name)
(out / "policy").write_text("release" if "--release" in sys.argv else "development")
''')

        self.run_command("git", "-C", str(self.source), "init", "-q")
        self.run_command("git", "-C", str(self.source), "-c", "user.name=Build Test",
                         "-c", "user.email=build@example.invalid", "add", ".")
        self.run_command("git", "-C", str(self.source), "-c", "user.name=Build Test",
                         "-c", "user.email=build@example.invalid", "commit", "-qm", "test: initialize source")

    def run_command(self, *args, success=True):
        result = subprocess.run(args, env=self.env, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
        if success:
            self.assertEqual(result.returncode, 0, result.stdout)
        else:
            self.assertNotEqual(result.returncode, 0, result.stdout)
        return result.stdout

    def configure(self, *flags):
        return self.run_command("cmake", "-S", str(self.source), "-B", str(self.binary), "-G", self.env.get("PRODUCT_TEST_GENERATOR", "Unix Makefiles"), *[f"-D{name}={self.source / tool}" for name, tool in [("PRODUCT_CARGO", "cargo"), ("PRODUCT_RUSTC", "rustc"), ("PRODUCT_GO", "go"), ("PRODUCT_MAKE", "qemu-make")]], *flags)

    def build(self):
        self.run_command("cmake", "--build", str(self.binary), "--parallel", "2")

    def test_build_rechecks_components_and_configuration(self):
        self.configure()
        self.build()
        commands = [json.loads(line) for line in self.log.read_text().splitlines()]
        self.assertEqual((self.binary / "firmware-results").read_text().splitlines()[-1], "7")
        cargo_commands = [args for name, args in commands if name == "cargo"]
        self.assertEqual(cargo_commands, [["build", "--locked", "--release"]])
        self.run_command("ctest", "--test-dir", str(self.binary), "-R", "^(frontend|instigator)$", "--output-on-failure")
        manifest = json.loads((self.binary / "product-build.json").read_text())
        self.assertEqual(manifest["qemu_source"], str(self.source / "qemu"))
        self.assertEqual((self.binary / "run/qemu").read_text(), "first qemu")
        self.build()
        commands = [json.loads(line) for line in self.log.read_text().splitlines()]
        self.assertEqual(sum(name == "configure" for name, _ in commands), 1)
        for path, content in [("src/main.rs", "edited rust"), ("instigator/main.go", "edited go"), ("qemu/source.c", "edited qemu")]:
            (self.source / path).write_text(content)
        self.build()
        for name in ("origami", "instigator", "qemu"):
            self.assertTrue((self.binary / "run" / name).read_text().startswith("edited"))
        # Makefile timestamp checks have one-second resolution.
        time.sleep(1.1)
        (self.source / "qemu/meson.build").write_text("changed configuration")
        self.build()
        commands = [json.loads(line) for line in self.log.read_text().splitlines()]
        self.assertEqual(sum(name == "configure" for name, _ in commands), 1)
        self.assertEqual((self.binary / "qemu-build/configuration-meson").read_text(), "changed configuration")
        self.assertTrue(any(name == "cargo" and "--locked" in args for name, args in commands))
        self.assertTrue(any(name == "go" and "-mod=readonly" in args for name, args in commands))
        self.run_command("cpack", "--config", str(self.binary / "CPackConfig.cmake"), "-B", str(self.binary / "archives"))
        commands = [json.loads(line) for line in self.log.read_text().splitlines()]
        self.assertEqual((self.binary / "firmware-results").read_text().splitlines()[-1], "7")
        cargo_commands = [args for name, args in commands if name == "cargo"]
        self.assertEqual(cargo_commands[-1], ["fetch", "--locked"])
        import tarfile
        archive_name, archive_root = (("origami-macos-arm64-preview.tar.gz", "macos-arm64-dev")
                                      if sys.platform == "darwin" else
                                      ("origami-linux-x86_64-preview.tar.gz", "linux-dev"))
        with tarfile.open(self.binary / "archives" / archive_name) as archive:
            self.assertEqual(archive.extractfile(f"{archive_root}/policy").read(), b"release")
            self.assertNotIn("product-build.json", archive.getnames())

    def test_package_rebuilds_changed_sources_before_collection(self):
        self.env["PRODUCT_TEST_GENERATOR"] = "Ninja"
        self.configure()
        self.assertIn("CMAKE_GENERATOR:INTERNAL=Ninja",
                      (self.binary / "CMakeCache.txt").read_text())
        self.build()
        (self.source / "src/main.rs").write_text("updated frontend for package")
        (self.source / "instigator/main.go").write_text("updated installer for package")
        (self.source / "qemu/source.c").write_text("updated emulator for package")
        self.run_command("cpack", "--config", str(self.binary / "CPackConfig.cmake"),
                         "-B", str(self.binary / "archives"))
        import tarfile
        archive_name, archive_root = (("origami-macos-arm64-preview.tar.gz", "macos-arm64-dev")
                                      if sys.platform == "darwin" else
                                      ("origami-linux-x86_64-preview.tar.gz", "linux-dev"))
        with tarfile.open(self.binary / "archives" / archive_name) as archive:
            self.assertEqual(archive.extractfile(f"{archive_root}/origami").read(),
                             b"updated frontend for package")
            self.assertEqual(archive.extractfile(f"{archive_root}/instigator").read(),
                             b"updated installer for package")
            self.assertEqual(archive.extractfile(f"{archive_root}/qemu").read(),
                             b"updated emulator for package")

    def test_packaging_consumes_the_build_environment_and_artifacts(self):
        self.configure()
        self.build()
        manifest = json.loads((self.binary / "product-build.json").read_text())
        environment = json.loads((self.binary / "go-environment.json").read_text())
        configured = dict(entry.split("=", 1) for entry in manifest.get("go_environment", []))
        self.assertEqual(configured, environment)
        self.assertEqual(configured.get("GOPATH"), str(self.binary / "go"))
        files = manifest.get("files", {})
        self.assertEqual(files.get("bin/origami"), str(self.binary / "cargo-target/release/origami"))
        self.assertEqual(files.get("libexec/sgi/qemu-img"), str(self.binary / "qemu-build/qemu-img"))

    def test_windows_configuration_supplies_target_paths_and_environment(self):
        self.run_command("cmake", "-S", str(self.source), "-B", str(self.binary),
                         "-DPRODUCT_PLATFORM=windows", "-DPRODUCT_RUST_TARGET=x86_64-pc-windows-gnu",
                         *[f"-D{name}={self.source / tool}" for name, tool in
                           [("PRODUCT_CARGO", "cargo"), ("PRODUCT_RUSTC", "rustc"),
                            ("PRODUCT_GO", "go"), ("PRODUCT_MAKE", "qemu-make")]], *flags)
        manifest = json.loads((self.binary / "product-build.json").read_text())
        self.assertEqual(manifest['files']['bin/origami.exe'],
                         str(self.binary / "cargo-target/x86_64-pc-windows-gnu/release/origami.exe"))
        environment = dict(entry.split("=", 1) for entry in manifest['go_environment'])
        self.assertEqual(environment['GOOS'], 'windows')
        self.assertEqual(environment['GOARCH'], 'amd64')

    def test_package_version_reads_tags_after_configure(self):
        self.configure()
        self.run_command("git", "-C", str(self.source), "-c", "user.name=Build Test",
                         "-c", "user.email=build@example.invalid", "tag", "-a", "v0.2.0", "-m", "test release")
        script = self.binary / "read-package-version.cmake"
        script.write_text('include("' + str(self.binary / "CPackConfig.cmake") + '")\n'
                          'if(CPACK_PROJECT_CONFIG_FILE)\n'
                          '  include("${CPACK_PROJECT_CONFIG_FILE}")\n'
                          'endif()\nmessage(STATUS "package-version=${CPACK_PACKAGE_VERSION}")\n')
        self.assertIn("package-version=0.2.0", self.run_command("cmake", "-P", str(script)))

    def test_qemu_make_owns_configuration_updates(self):
        self.configure()
        self.build()
        (self.source / "qemu/VERSION").write_text("changed version")
        self.build()
        self.assertEqual((self.binary / "qemu-build/configuration-version").read_text(), "changed version")
        commands = [json.loads(line) for line in self.log.read_text().splitlines()]
        self.assertEqual(sum(name == "qemu-make" and any(arg.startswith("qemu-system-") for arg in args) for name, args in commands), 2)
        self.assertEqual(sum(name == "configure" for name, _ in commands), 2)

    def test_libslirp_patch_updates_existing_build(self):
        self.env["FIXTURE_SLIRP_TEST"] = "1"
        self.configure()
        self.build()
        self.assertEqual((self.binary / "run/qemu").read_text(), "first qemu\nfirst slirp")
        self.build()
        commands = [json.loads(line) for line in self.log.read_text().splitlines()]
        self.assertEqual(sum(name == "meson" for name, _ in commands), 1)
        time.sleep(1.1)
        self.slirp_patch.write_text("changed slirp")
        self.build()
        self.assertEqual((self.binary / "run/qemu").read_text(), "first qemu\nchanged slirp")
        time.sleep(1.1)
        (self.slirp_patch.parent / "libslirp-new.patch").write_text("new patch")
        self.slirp_wrap.write_text("[wrap-file]\ndiff_files = libslirp-test.patch, libslirp-new.patch\n")
        self.build()
        self.assertEqual((self.binary / "run/qemu").read_text(), "first qemu\nchanged slirp\nnew patch")
        commands = [json.loads(line) for line in self.log.read_text().splitlines()]
        self.assertEqual(sum(name == "configure" for name, _ in commands), 1)
        self.assertEqual(sum(name == "meson" for name, _ in commands), 3)

    def test_libslirp_patch_failure_stops_qemu_build(self):
        self.configure()
        self.build()
        commands_before = [json.loads(line) for line in self.log.read_text().splitlines()]
        time.sleep(1.1)
        self.slirp_patch.write_text("INVALID")
        output = self.run_command("cmake", "--build", str(self.binary), success=False)
        self.assertIn("libslirp patch failed", output)
        commands_after = [json.loads(line) for line in self.log.read_text().splitlines()]
        self.assertEqual(sum(name == "ninja" for name, _ in commands_after),
                         sum(name == "ninja" for name, _ in commands_before))

    def test_core_sources_and_headers_rebuild_before_cargo(self):
        self.configure()
        self.build()
        for path, text, expected in [
            ("qemu/util/sgi-flash-layouts.c", "int fixture_table(void) { return 6; }\n", "9"),
            ("qemu/include/sgi/flash-image.h", "#define FIXTURE_VALUE 8\n", "15"),
        ]:
            time.sleep(1.1)
            (self.source / path).write_text(text)
            if path.endswith(".h"):
                self.run_command("cmake", "--build", str(self.binary), "--target", "frontend")
                self.run_command("ctest", "--test-dir", str(self.binary), "-R", "^frontend$", "--output-on-failure")
            else:
                self.build()
            self.assertEqual((self.binary / "firmware-results").read_text().splitlines()[-1], expected)

    @unittest.skipUnless(shutil.which("x86_64-w64-mingw32-gcc"), "MinGW cross compiler required")
    def test_windows_frontend_tests_compile_the_target_archive(self):
        self.configure("-DPRODUCT_PLATFORM=windows", "-DPRODUCT_RUST_TARGET=x86_64-pc-windows-gnu",
                       "-DPRODUCT_CROSS_PREFIX=x86_64-w64-mingw32-")
        self.run_command("cmake", "--build", str(self.binary), "--target", "frontend")
        self.run_command("ctest", "--test-dir", str(self.binary), "-R", "^frontend$", "--output-on-failure")
        commands = [json.loads(line) for line in self.log.read_text().splitlines()]
        self.assertIn(["cargo", ["test", "--locked", "--target", "x86_64-pc-windows-gnu", "--no-run"]], commands)
        self.assertFalse((self.binary / "firmware-results").exists())

    def test_preset_uses_ignored_source_output_by_default(self):
        self.env.pop("SGI_BUILD_ROOT", None)
        preset = "macos" if sys.platform == "darwin" else "linux"
        self.run_command("cmake", "--preset", preset, "-S", str(self.source),
                         "-G", "Unix Makefiles", *[f"-D{name}={self.source / tool}" for name, tool in
                         [("PRODUCT_CARGO", "cargo"), ("PRODUCT_RUSTC", "rustc"),
                          ("PRODUCT_GO", "go"), ("PRODUCT_MAKE", "qemu-make")]], *flags)
        manifest_path = self.source / f"out/{preset}/product-build.json"
        self.assertTrue(manifest_path.is_file())
        manifest = json.loads(manifest_path.read_text())
        self.assertEqual(manifest["output_dir"], str(self.source / f"out/{preset}/run"))

    def test_missing_submodules_allow_initialization_target(self):
        (self.source / "qemu/configure").unlink()
        self.configure()
        output = self.run_command("cmake", "--build", str(self.binary), "--target", "help")
        self.assertIn("submodules", output)
        output = self.run_command("cmake", "--build", str(self.binary), success=False)
        self.assertIn("submodules", output)


if __name__ == "__main__":
    unittest.main()
