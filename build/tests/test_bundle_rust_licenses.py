# SPDX-License-Identifier: BSD-3-Clause
import importlib.util
from pathlib import Path
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().parents[1] / 'bundle-rust-licenses.py'
SPEC = importlib.util.spec_from_file_location('bundle_rust_licenses', SCRIPT)
COLLECTOR = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(COLLECTOR)


class RustLicenseTests(unittest.TestCase):
    def test_lowercase_license_filenames_are_collected(self):
        with tempfile.TemporaryDirectory(prefix='rust crate licenses ') as temporary:
            root = Path(temporary)
            registry = root / 'index'
            crate = registry / 'windows-link-0.2.1'
            crate.mkdir(parents=True)
            for name in ('license-apache-2.0', 'license-mit'):
                (crate / name).write_text(name)

            self.assertEqual(
                [path.name for path in COLLECTOR.license_files(root, 'windows-link', '0.2.1')],
                ['license-apache-2.0', 'license-mit'],
            )

    def test_windows_import_libraries_use_winapi_license_files(self):
        with tempfile.TemporaryDirectory(prefix='rust crate licenses ') as temporary:
            root = Path(temporary)
            registry = root / 'index'
            registry.mkdir()
            parent = registry / 'winapi-0.3.9'
            parent.mkdir()
            for name in ('LICENSE-APACHE', 'LICENSE-MIT'):
                (parent / name).write_text(name)

            for name in ('winapi-i686-pc-windows-gnu', 'winapi-x86_64-pc-windows-gnu'):
                with self.subTest(crate=name):
                    crate = registry / f'{name}-0.4.0'
                    crate.mkdir()
                    self.assertEqual(
                        [path.name for path in COLLECTOR.license_files(root, name, '0.4.0')],
                        ['LICENSE-APACHE', 'LICENSE-MIT'],
                    )


if __name__ == '__main__':
    unittest.main()
