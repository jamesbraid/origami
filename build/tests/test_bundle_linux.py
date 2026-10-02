# SPDX-License-Identifier: BSD-3-Clause
import importlib.util
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

    def test_reports_unresolved_dependencies(self):
        with patch.object(BUNDLER, 'run', return_value='libmissing.so.0 => not found\n'):
            with self.assertRaisesRegex(RuntimeError, 'unresolved dependency.*libmissing'):
                BUNDLER.dependencies(Path('/run/bin/origami'))


if __name__ == '__main__':
    unittest.main()
