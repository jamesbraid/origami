# SPDX-License-Identifier: BSD-3-Clause
import importlib.util
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

SCRIPT = Path(__file__).resolve().parents[1] / 'bundle-linux.py'
SPEC = importlib.util.spec_from_file_location('bundle_linux', SCRIPT)
BUNDLER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(BUNDLER)


class LinuxLibraryTests(unittest.TestCase):
    def test_parses_relocated_dependency_path_containing_spaces(self):
        listing = '''
    linux-vdso.so.1 (0x00007ffffffe1000)
    libSDL2-2.0.so.0 => /build with spaces/run/lib/sgi/libSDL2-2.0.so.0 (0x00007abc12340000)
    libc.so.6 => /lib/x86_64-linux-gnu/libc.so.6 (0x00007abc43210000)
    /lib64/ld-linux-x86-64.so.2 (0x00007abc00000000)
'''
        with patch.object(BUNDLER, 'run', return_value=listing):
            self.assertEqual(BUNDLER.dependencies(Path('/build with spaces/run/bin/origami')), {
                'libSDL2-2.0.so.0': Path('/build with spaces/run/lib/sgi/libSDL2-2.0.so.0'),
            })

    @unittest.skipUnless(sys.platform.startswith("linux") and
                         all(shutil.which(tool) for tool in ("cc", "ldd", "patchelf", "dpkg-query")),
                         "Debian ELF packaging tools required")
    def test_bundles_frontend_and_firmware_helper_zlib_dependencies(self):
        with tempfile.TemporaryDirectory(prefix="linux firmware bundle ") as temporary:
            bundle = Path(temporary) / "bundle"
            source = Path(temporary) / "main.c"
            source.write_text('#include <zlib.h>\nint main(void) { return zlibVersion()[0] ? 0 : 1; }\n')
            roots = [bundle / name for name in ("bin/origami", "bin/qemu-sgi-firmware",
                                                "libexec/sgi/qemu-system-mips64", "libexec/sgi/qemu-img")]
            for binary in roots:
                binary.parent.mkdir(parents=True, exist_ok=True)
                subprocess.run(["cc", str(source), "-lz", "-o", str(binary)], check=True)
            with patch.object(BUNDLER.sys, "argv", [str(SCRIPT), str(bundle)]):
                BUNDLER.main()
            for binary in roots:
                zlib = BUNDLER.dependencies(binary)["libz.so.1"]
                self.assertEqual(zlib.resolve(), (bundle / "lib/sgi/libz.so.1").resolve())
                subprocess.run([str(binary)], check=True, timeout=10)
            self.assertIn("libz.so.1\tzlib1g", (bundle / "share/sgi/debian-libraries.tsv").read_text())
            self.assertTrue(any((bundle / "share/sgi/licenses/debian").glob("zlib*.copyright")))

    def test_reports_unresolved_dependencies(self):
        with patch.object(BUNDLER, 'run', return_value='libmissing.so.0 => not found\n'):
            with self.assertRaisesRegex(RuntimeError, 'unresolved dependency.*libmissing'):
                BUNDLER.dependencies(Path('/run/bin/origami'))


if __name__ == '__main__':
    unittest.main()
