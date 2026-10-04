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
TARGET_SYSROOT = BUNDLER.SYSROOT
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
        return BUNDLER.runtime_dependencies([executable])

    def test_collects_transitive_dlls_and_sdl3_runtime_dependency(self):
        self.dll("second.dll", "int second(void) { return 2; }")
        self.dll("first.dll", "extern int second(void); int first(void) { return second(); }",
                 libraries=("second",))
        self.dll("SDL2.dll", "int sdl2(void) { return 2; }")
        self.dll("SDL3.dll", "extern int second(void); int sdl3(void) { return second(); }",
                 libraries=("second",))
        executable = self.executable(
            "app.exe",
            "extern int first(void); extern int sdl2(void); "
            "int main(void) { return first() + sdl2(); }",
            libraries=("first", "SDL2"),
        )

        dependencies = {path.name.lower() for path in self.scan(executable)}
        self.assertTrue({"first.dll", "second.dll", "sdl2.dll", "sdl3.dll"} <= dependencies)

    def test_packages_frontend_and_firmware_helper_target_zlib(self):
        roots = []
        for name in ("origami.exe", "instigator.exe", "qemu-sgi-firmware.exe",
                     "qemu-system-mips64.exe", "qemu-img.exe"):
            binary = self.executable(name,
                                     '#include <zlib.h>\nint main(void) { return zlibVersion()[0] ? 0 : 1; }\n',
                                     libraries=("z",))
            if name in ("qemu-system-mips64.exe", "qemu-img.exe"):
                destination = self.root / "libexec/sgi" / name
                destination.parent.mkdir(parents=True, exist_ok=True)
                binary.rename(destination)
            else:
                roots.append(binary)
        with patch.object(BUNDLER, "SYSROOT", TARGET_SYSROOT), \
                patch.object(BUNDLER.sys, "argv", [str(SCRIPT), str(self.root)]):
            BUNDLER.main()
        manifest = (self.root / "share/sgi/windows-dlls.tsv").read_text()
        self.assertIn("mingw64-zlib", manifest)
        self.assertTrue((self.bin / "zlib1.dll").is_file())
        self.assertTrue((self.root / "share/sgi/licenses/fedora/mingw64-zlib/zlib.LICENSE.txt").is_file())
        with patch.object(BUNDLER, "SYSROOT", self.bin):
            dependencies = BUNDLER.runtime_dependencies(roots)
        self.assertIn("zlib1.dll", {path.name.lower() for path in dependencies})

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

        dependencies = {path.name.lower() for path in self.scan(executable)}
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

        real_run = subprocess.run

        def capture_cmake(command, check):
            return real_run(command, check=check, capture_output=True, text=True)

        with patch.object(BUNDLER.subprocess, "run", side_effect=capture_cmake):
            with self.assertRaises(subprocess.CalledProcessError) as error:
                self.scan(executable)

        diagnostic = next(
            line.strip() for line in error.exception.stderr.splitlines()
            if "unresolved Windows DLL dependencies:" in line
        )
        self.assertEqual(diagnostic, "unresolved Windows DLL dependencies: missing.dll")


if __name__ == "__main__":
    unittest.main()
