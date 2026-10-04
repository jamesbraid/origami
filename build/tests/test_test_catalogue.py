# SPDX-License-Identifier: BSD-3-Clause
import importlib.util
import json
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parents[1] / 'update-test-catalogue.py'
SPEC = importlib.util.spec_from_file_location('update_test_catalogue', SCRIPT)
UPDATER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(UPDATER)
QEMU = UPDATER.ROOT / 'qemu'


class TestCatalogueTests(unittest.TestCase):
    @unittest.skipUnless((QEMU / 'VERSION').is_file(), 'qemu submodule is not checked out')
    def test_fixture_matches_the_pinned_qemu_catalogue(self):
        # A checked-out QEMU must carry the catalogue; a missing one is a stale pin.
        catalogue = json.loads(UPDATER.SUBMODULE_CATALOGUE.read_text())
        self.assertEqual(UPDATER.FIXTURE.read_text(), UPDATER.render(catalogue),
                         'tests/fixtures/sgi-machines.json is stale; '
                         'run build/update-test-catalogue.py')

    def test_trim_keeps_only_the_fields_the_frontend_reads(self):
        offering = {field: field for field in UPDATER.OFFERING_FIELDS}
        offering['display-name'] = 'dropped'
        text = UPDATER.render({'schema': 'sgi-machines', 'version': 1,
                               'topologies': [], 'offerings': [offering]})
        self.assertEqual(json.loads(text)['offerings'], [
            {field: field for field in UPDATER.OFFERING_FIELDS}])
        with self.assertRaises(SystemExit):
            UPDATER.render({'schema': 'sgi-sn', 'version': 1, 'offerings': []})
        del offering['consoles']
        with self.assertRaises(SystemExit):
            UPDATER.render({'schema': 'sgi-machines', 'version': 1, 'offerings': [offering]})

    def test_fixture_is_a_trimmed_catalogue(self):
        fixture = json.loads(UPDATER.FIXTURE.read_text())
        self.assertEqual(UPDATER.FIXTURE.read_text(), UPDATER.render(fixture))


if __name__ == '__main__':
    unittest.main()
