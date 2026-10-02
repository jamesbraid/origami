"""Exercise the product build graph without compiling external projects."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
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
        (self.source / "qemu").mkdir()
        (self.source / "instigator").mkdir()
        (self.source / "src").mkdir()
        (self.source / "src/main.rs").write_text("first rust")
        (self.source / "instigator/main.go").write_text("first go")
        (self.source / "instigator/go.mod").write_text("module fixture")
        (self.source / "qemu/source.c").write_text("first qemu")
        (self.source / "qemu/meson.build").write_text("first configuration")
        self.log = Path(self.temp.name) / "commands.jsonl"
        self.env = dict(os.environ, FIXTURE_LOG=str(self.log), FIXTURE_BUILD_DIR=str(self.binary))
        self.tool = self.source / "tool"
        self.tool.write_text('''#!/usr/bin/env python3
import json, os, pathlib, sys
args = sys.argv[1:]
with open(os.environ["FIXTURE_LOG"], "a") as f:
    f.write(json.dumps([pathlib.Path(sys.argv[0]).name, args]) + "\\n")
name = pathlib.Path(sys.argv[0]).name
if name == "configure" or (name in ("cargo", "go") and "build" in args):
    assert (pathlib.Path(os.environ["FIXTURE_BUILD_DIR"]) / "inputs-recorded").is_file(), "compilation started before inputs were recorded"
if name == "configure":
    pathlib.Path("build.ninja").write_text("configured")
    pathlib.Path("configuration-source").write_text(str(pathlib.Path(sys.argv[0]).parent))
elif name == "ninja":
    root = pathlib.Path(pathlib.Path("configuration-source").read_text())
    for target in args:
        if target.startswith("qemu-"):
            pathlib.Path(target).write_text((root / "source.c").read_text())
elif name == "cargo" and "build" in args:
    root = pathlib.Path(os.environ["CARGO_TARGET_DIR"])
    if "--target" in args: root /= args[args.index("--target") + 1]
    root /= "release" if "--release" in args else "debug"
    root.mkdir(parents=True, exist_ok=True)
    (root / "origami").write_text(pathlib.Path("src/main.rs").read_text())
elif name == "go" and "build" in args:
    pathlib.Path(args[args.index("-o") + 1]).write_text(pathlib.Path("main.go").read_text())
''')
        self.tool.chmod(0o755)
        for name in ("cargo", "rustc", "go", "ninja"):
            (self.source / name).symlink_to(self.tool)
        (self.source / "qemu/configure").symlink_to(self.tool)
        (self.source / "build/stage-product.py").write_text('''import json, pathlib, shutil, sys
m = json.loads(pathlib.Path(sys.argv[1]).read_text())
record = pathlib.Path(m["output_dir"]).parent
if "--record-inputs" in sys.argv:
    (record / "inputs-recorded").write_text("inputs")
    sys.exit(0)
if "--record-build" in sys.argv:
    assert (record / "inputs-recorded").is_file()
    (record / "build-recorded").write_text("outputs")
if "--release" in sys.argv:
    assert (record / "build-recorded").is_file(), "missing build provenance"
out = pathlib.Path(sys.argv[sys.argv.index("--output")+1] if "--output" in sys.argv else m["output_dir"])
out.mkdir(parents=True, exist_ok=True)
for name, origin in [("origami", pathlib.Path(m["cargo_target_dir"]) / m["build_type"] / "origami"), ("instigator", pathlib.Path(m["instigator_binary"])), ("qemu", pathlib.Path(m["qemu_build"]) / "qemu-system-mips64")]:
    shutil.copy(origin, out / name)
(out / "policy").write_text("release" if "--release" in sys.argv else "development")
''')

    def run_command(self, *args, success=True):
        result = subprocess.run(args, env=self.env, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
        if success:
            self.assertEqual(result.returncode, 0, result.stdout)
        else:
            self.assertNotEqual(result.returncode, 0, result.stdout)
        return result.stdout

    def configure(self):
        return self.run_command("cmake", "-S", str(self.source), "-B", str(self.binary), "-G", os.environ.get("PRODUCT_TEST_GENERATOR", "Unix Makefiles"), *[f"-D{name}={self.source / tool}" for name, tool in [("PRODUCT_CARGO", "cargo"), ("PRODUCT_RUSTC", "rustc"), ("PRODUCT_GO", "go"), ("PRODUCT_NINJA", "ninja")]])

    def build(self):
        self.run_command("cmake", "--build", str(self.binary), "--parallel", "2")

    def test_build_rechecks_components_and_configuration(self):
        self.configure()
        self.build()
        self.run_command("ctest", "--test-dir", str(self.binary), "-R", "^(frontend|instigator)$", "--output-on-failure")
        manifest = json.loads((self.binary / "product-build.json").read_text())
        self.assertEqual(manifest["qemu_source"], str(self.source / "qemu"))
        self.assertEqual((self.binary / "run/qemu").read_text(), "first qemu")
        self.assertTrue((self.binary / "build-recorded").is_file())
        self.build()
        commands = [json.loads(line) for line in self.log.read_text().splitlines()]
        self.assertEqual(sum(name == "configure" for name, _ in commands), 1)
        for path, content in [("src/main.rs", "edited rust"), ("instigator/main.go", "edited go"), ("qemu/source.c", "edited qemu")]:
            (self.source / path).write_text(content)
        self.build()
        for name in ("origami", "instigator", "qemu"):
            self.assertTrue((self.binary / "run" / name).read_text().startswith("edited"))
        (self.source / "qemu/meson.build").write_text("changed configuration")
        self.build()
        commands = [json.loads(line) for line in self.log.read_text().splitlines()]
        self.assertEqual(sum(name == "configure" for name, _ in commands), 2)
        self.assertTrue(any(name == "cargo" and "--locked" in args for name, args in commands))
        self.assertTrue(any(name == "go" and "-mod=readonly" in args for name, args in commands))
        self.run_command("cpack", "--config", str(self.binary / "CPackConfig.cmake"), "-B", str(self.binary / "archives"))
        import tarfile
        with tarfile.open(self.binary / "archives/origami-linux-x86_64-preview.tar.gz") as archive:
            self.assertEqual(archive.extractfile("linux-dev/policy").read(), b"release")
            self.assertNotIn("product-build.json", archive.getnames())

    def test_preset_uses_ignored_source_output_by_default(self):
        self.env.pop("SGI_BUILD_ROOT", None)
        self.run_command("cmake", "--preset", "linux", "-S", str(self.source),
                         "-G", "Unix Makefiles", *[f"-D{name}={self.source / tool}" for name, tool in
                         [("PRODUCT_CARGO", "cargo"), ("PRODUCT_RUSTC", "rustc"),
                          ("PRODUCT_GO", "go"), ("PRODUCT_NINJA", "ninja")]])
        manifest_path = self.source / "out/linux/product-build.json"
        self.assertTrue(manifest_path.is_file())
        manifest = json.loads(manifest_path.read_text())
        self.assertEqual(manifest["output_dir"], str(self.source / "out/linux/run"))

    def test_missing_submodules_allow_initialization_target(self):
        (self.source / "qemu/configure").unlink()
        self.configure()
        output = self.run_command("cmake", "--build", str(self.binary), "--target", "help")
        self.assertIn("submodules", output)
        output = self.run_command("cmake", "--build", str(self.binary), success=False)
        self.assertIn("submodules", output)


if __name__ == "__main__":
    unittest.main()
