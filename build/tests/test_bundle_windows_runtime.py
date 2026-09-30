import importlib.util
import shutil
import subprocess
import tempfile
import unittest
from unittest.mock import patch
from pathlib import Path


SCRIPT = Path(__file__).resolve().parents[1] / "bundle-windows.py"
SPEC = importlib.util.spec_from_file_location("bundle_windows", SCRIPT)
BUNDLER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(BUNDLER)

COMPILER = shutil.which("x86_64-w64-mingw32-gcc")
OBJDUMP = shutil.which("x86_64-w64-mingw32-objdump")
CMAKE = shutil.which("cmake")
AVAILABLE = all((COMPILER, OBJDUMP, CMAKE)) and BUNDLER.SYSROOT.is_dir()


@unittest.skipUnless(
    AVAILABLE,
    "requires CMake, the MinGW compiler and objdump, and the Fedora MinGW sysroot",
)
class WindowsRuntimeDependencyTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.bin = self.root / "bin"
        self.bin.mkdir()
        self.sysroot = self.root / "sysroot"
        self.sysroot.mkdir()
        self.search = self.root / "search"
        self.search.mkdir()
        self._sysroot_patch = patch.object(BUNDLER, "SYSROOT", self.sysroot)
        self._sysroot_patch.start()
        self.addCleanup(self._sysroot_patch.stop)

    def compile(self, *arguments):
        subprocess.run(
            [COMPILER, *map(str, arguments)], check=True, capture_output=True, text=True,
        )

    def dll(self, name, source, libraries=()):
        stem = Path(name).stem
        source_path = self.root / f"{stem}.c"
        output_path = self.sysroot / name
        import_library = self.sysroot / f"lib{stem}.a"
        source_path.write_text(source)
        self.compile(
            "-shared", source_path, *(
                [f"-L{self.sysroot}", *(f"-l{library}" for library in libraries)]
                if libraries else []
            ),
            "-o", output_path, "-Wl,--out-implib," + str(import_library),
        )

    def executable(self, name, source, libraries=(), library_dirs=()):
        source_path = self.root / f"{Path(name).stem}.c"
        output_path = self.bin / name
        source_path.write_text(source)
        self.compile(
            source_path,
            *(f"-L{directory}" for directory in library_dirs),
            *(f"-L{directory}" for directory in ({self.sysroot} if not library_dirs else ())),
            *(f"-l{library}" for library in libraries),
            "-o", output_path,
        )
        return output_path

    def scan(self, executable):
        for source in self.sysroot.glob("*.dll"):
            alias = self.search / source.name.lower()
            if not alias.exists():
                alias.symlink_to(source)
        output = self.root / f"{executable.stem}-dependencies.txt"
        result = subprocess.run(
            [
                CMAKE,
                f"-DROOTS={executable}",
                f"-DSEARCH_DIRECTORIES={self.search}",
                f"-DSYSTEM_DLLS={';'.join(sorted(BUNDLER.SYSTEM_DLLS))}",
                f"-DOUTPUT={output}",
                "-P", str(BUNDLER.RUNTIME_DEPENDENCIES_SCRIPT),
            ],
            capture_output=True, text=True,
        )
        return result, output

    def test_collects_transitive_dlls_and_sdl3_runtime_dependency(self):
        self.dll("second.dll", "int second(void) { return 2; }")
        self.dll("first.dll", "extern int second(void); int first(void) { return second(); }",
                 libraries=("second",))
        self.dll("sdl2.dll", "int sdl2(void) { return 2; }")
        self.dll("sdl3.dll", "extern int second(void); int sdl3(void) { return second(); }",
                 libraries=("second",))
        executable = self.executable(
            "app.exe",
            "extern int first(void); extern int sdl2(void); "
            "int main(void) { return first() + sdl2(); }",
            libraries=("first", "sdl2"),
        )

        result, output = self.scan(executable)

        self.assertEqual(result.returncode, 0, result.stderr)
        dependencies = {Path(line).name.lower() for line in output.read_text().splitlines()}
        self.assertTrue({"first.dll", "second.dll", "sdl2.dll", "sdl3.dll"} <= dependencies)

    def test_excludes_unavailable_api_set_import(self):
        api_source = self.root / "api.c"
        api_dir = self.root / "api"
        api_dir.mkdir()
        api_dll = api_dir / "api-ms-win-test.dll"
        api_import = api_dir / "libapi.a"
        api_source.write_text("int api(void) { return 5; }")
        self.compile(
            "-shared", api_source, "-o", api_dll,
            "-Wl,--out-implib," + str(api_import),
        )
        executable = self.executable(
            "api-app.exe", "int api(void); int main(void) { return api(); }",
            libraries=("api",), library_dirs=(api_dir,),
        )

        result, output = self.scan(executable)

        self.assertEqual(result.returncode, 0, result.stderr)
        dependencies = {Path(line).name.lower() for line in output.read_text().splitlines()}
        self.assertNotIn("api-ms-win-test.dll", dependencies)

    def test_reports_missing_non_system_dll_import(self):
        missing_dir = self.root / "missing"
        missing_dir.mkdir()
        missing_dll = missing_dir / "missing.dll"
        missing_import = missing_dir / "libmissing.a"
        source = self.root / "missing-library.c"
        source.write_text("int missing(void) { return 9; }")
        self.compile(
            "-shared", source, "-o", missing_dll,
            "-Wl,--out-implib," + str(missing_import),
        )
        executable = self.executable(
            "missing-app.exe", "int missing(void); int main(void) { return missing(); }",
            libraries=("missing",), library_dirs=(missing_dir,),
        )
        missing_dll.unlink()

        result, _ = self.scan(executable)

        self.assertNotEqual(result.returncode, 0)
        diagnostic = next(
            line.strip() for line in result.stderr.splitlines()
            if "unresolved Windows DLL dependencies:" in line
        )
        self.assertEqual(diagnostic, "unresolved Windows DLL dependencies: missing.dll")


if __name__ == "__main__":
    unittest.main()
