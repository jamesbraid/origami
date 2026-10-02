# SPDX-License-Identifier: BSD-3-Clause
import hashlib
import importlib.util
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

SCRIPT = Path(__file__).resolve().parents[1] / 'stage-product.py'


def git(root, *args):
    return subprocess.check_output(['git', '-C', str(root), *args], text=True).strip()


class ProductStageTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix='stage product ')
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.sources = {}
        for name in ('product', 'qemu', 'instigator'):
            root = self.root / (name + ' source')
            root.mkdir()
            git(root, 'init', '-q')
            git(root, 'config', 'user.name', 'Stage Test')
            git(root, 'config', 'user.email', 'stage@example.invalid')
            (root / 'LICENSE').write_text(name + ' license\n')
            if name == 'qemu':
                for path in ('LICENSE.origami', 'LICENSE.origami.paths', 'COPYING',
                             'COPYING.LIB', 'hw/mips/sgi/models/LICENSE',
                             'pc-bios/keymaps/en-us', 'pc-bios/keymaps/common',
                             'pc-bios/keymaps/meson.build'):
                    target = root / path
                    target.parent.mkdir(parents=True, exist_ok=True)
                    target.write_text(path + '\n')
            git(root, 'add', '.')
            git(root, 'commit', '-qm', 'test: initialize source')
            self.sources[name] = root
        for name in ('qemu', 'instigator'):
            git(self.sources['product'], 'update-index', '--add', '--cacheinfo',
                '160000', git(self.sources[name], 'rev-parse', 'HEAD'), name)
        git(self.sources['product'], 'commit', '-qm', 'test: pin dependencies')
        # The fixture gitlinks are external, like manifest sources; suppress missing-submodule noise.
        self.manifest = {name + '_source': str(root) for name, root in self.sources.items()}
        self.manifest.update(platform='linux', rust_target='', build_type='release',
                             output_dir=str(self.root / 'output tree'), archive_root_name='linux-dev',
                             qemu_build=str(self.root / 'qemu build'),
                             cargo_target_dir=str(self.root / 'cargo target'),
                             cargo_home=str(self.root / 'cargo'),
                             go_mod_cache=str(self.root / 'go mod'),
                             go_build_cache=str(self.root / 'go build'),
                             instigator_binary=str(self.root / 'instigator built'),
                             python='python3', rustc='rustc', go='go')
        for path in (Path(self.manifest['qemu_build']) / 'qemu-system-mips64',
                     Path(self.manifest['qemu_build']) / 'qemu-img',
                     Path(self.manifest['cargo_target_dir']) / 'release/origami',
                     Path(self.manifest['instigator_binary'])):
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(path.name + '\n')
        self.assertTrue(SCRIPT.is_file(), 'shared staging entry point is missing')
        spec = importlib.util.spec_from_file_location('stage_product', SCRIPT)
        self.stage = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(self.stage)

    def record_build(self):
        self.stage.record_inputs(self.manifest)
        self.stage.record_build(self.manifest)

    def run_stage(self, release=False):
        self.record_build()
        # Fixture files are text, so platform linkage is tested by the existing bundler tests.
        with patch.object(self.stage, 'bundle_runtime') as runtime, \
                patch.object(self.stage, 'collect_release_notices') as notices:
            result = self.stage.stage_product(self.manifest, release=release)
            self.assertEqual(notices.call_count, int(release))
            runtime.assert_called_once()
            return result

    def test_release_rejects_clean_commit_after_build(self):
        self.record_build()
        product = self.sources['product']
        (product / 'LICENSE').write_text('new clean commit')
        git(product, 'add', 'LICENSE')
        git(product, 'commit', '-qm', 'test: change product')
        with self.assertRaisesRegex(RuntimeError, 'source.*build|build.*source'):
            self.stage.stage_product(self.manifest, release=True)

    def test_release_rejects_changed_built_executable(self):
        self.record_build()
        Path(self.manifest['instigator_binary']).write_text('replaced executable')
        with self.assertRaisesRegex(RuntimeError, 'artifact|executable'):
            self.stage.stage_product(self.manifest, release=True)

    def test_build_record_rejects_source_change_during_compile(self):
        self.stage.record_inputs(self.manifest)
        (self.sources['qemu'] / 'LICENSE').write_text('source edit during compilation')
        with self.assertRaisesRegex(RuntimeError, 'source.*changed|changed.*source'):
            self.stage.record_build(self.manifest)

    def test_build_record_rejects_untracked_content_change_during_compile(self):
        edit = self.sources['qemu'] / 'local.c'
        edit.write_text('first edit')
        self.stage.record_inputs(self.manifest)
        edit.write_text('second edit')
        with self.assertRaisesRegex(RuntimeError, 'source.*changed|changed.*source'):
            self.stage.record_build(self.manifest)

    def test_release_requires_completed_build_record(self):
        with self.assertRaisesRegex(RuntimeError, 'completed build'):
            self.stage.stage_product(self.manifest, release=True)

    def test_dirty_development_layout_is_curated_and_checksums_match(self):
        (self.sources['qemu'] / 'untracked.txt').write_text('local edit')
        output = self.run_stage()
        expected = {'bin/origami', 'bin/instigator', 'libexec/sgi/qemu-system-mips64',
                    'libexec/sgi/qemu-img', 'share/sgi/qemu/keymaps/en-us',
                    'share/sgi/qemu/keymaps/common', 'share/sgi/source-revisions.txt',
                    'SHA256SUMS'}
        self.assertEqual({str(path.relative_to(output)) for path in output.rglob('*')
                          if path.is_file()}, expected)
        revisions = (output / 'share/sgi/source-revisions.txt').read_text()
        self.assertIn('qemu=' + git(self.sources['qemu'], 'rev-parse', 'HEAD') + ' (dirty)', revisions)
        for line in (output / 'SHA256SUMS').read_text().splitlines():
            digest, name = line.split('  ', 1)
            self.assertEqual(digest, hashlib.sha256((output / name).read_bytes()).hexdigest())

    def test_refresh_removes_stale_files(self):
        output = self.run_stage()
        (output / 'bin/stale').touch()
        self.run_stage()
        self.assertFalse((output / 'bin/stale').exists())

    def test_missing_input_preserves_existing_stage(self):
        output = self.run_stage()
        sentinel = output / 'sentinel'
        sentinel.write_text('keep')
        Path(self.manifest['instigator_binary']).unlink()
        with self.assertRaisesRegex(RuntimeError, 'missing|empty'):
            self.run_stage()
        self.assertEqual(sentinel.read_text(), 'keep')

    def test_runtime_failure_preserves_existing_stage(self):
        output = self.run_stage()
        sentinel = output / 'sentinel'
        sentinel.write_text('keep')
        with patch.object(self.stage, 'bundle_runtime', side_effect=RuntimeError('linkage failed')):
            with self.assertRaisesRegex(RuntimeError, 'linkage failed'):
                self.stage.stage_product(self.manifest)
        self.assertEqual(sentinel.read_text(), 'keep')

    def test_release_rejects_dirty_components(self):
        for name in ('product', 'qemu', 'instigator'):
            with self.subTest(component=name):
                edit = self.sources[name] / 'local.txt'
                edit.write_text('edit')
                with self.assertRaisesRegex(RuntimeError, name + '.*dirty'):
                    self.run_stage(release=True)
                edit.unlink()

    def test_release_rejects_revision_different_from_gitlink(self):
        qemu = self.sources['qemu']
        (qemu / 'LICENSE').write_text('new license')
        git(qemu, 'commit', '-qam', 'test: move dependency')
        with self.assertRaisesRegex(RuntimeError, 'qemu.*pin'):
            self.run_stage(release=True)

    def test_output_cannot_replace_source_or_built_inputs(self):
        for path in (self.sources['product'], self.sources['qemu'], self.root,
                     Path(self.manifest['qemu_build'])):
            with self.subTest(path=path):
                self.manifest['output_dir'] = str(path)
                with self.assertRaisesRegex(RuntimeError, 'output'):
                    self.run_stage()
        self.assertTrue((self.sources['product'] / 'LICENSE').exists())

    def test_refuses_unowned_nonempty_output(self):
        output = Path(self.manifest['output_dir'])
        output.mkdir()
        (output / 'keep').write_text('unrelated')
        with self.assertRaisesRegex(RuntimeError, 'output'):
            self.run_stage()
        self.assertEqual((output / 'keep').read_text(), 'unrelated')

    def test_release_notices_include_original_models_and_libslirp_build_source(self):
        qemu = self.sources['qemu']
        wrap = qemu / 'subprojects/libslirp.wrap'
        wrap.parent.mkdir()
        wrap.write_text('[wrap-file]\ndirectory = libslirp-4.9.1\n')
        slirp = Path(self.manifest['qemu_build']) / 'subprojects/libslirp-4.9.1/COPYRIGHT'
        slirp.parent.mkdir(parents=True)
        slirp.write_text('libslirp upstream notice')
        extra = self.sources['product'] / 'build/licenses/libslirp-4.9.1.copyright'
        extra.parent.mkdir(parents=True)
        extra.write_text('libslirp additional notices')
        bundle = self.root / 'notice output'
        bundle.mkdir()
        with patch.object(self.stage, 'toolchain_notices'), \
                patch.object(self.stage, 'helper'), \
                patch.object(self.stage, 'output', return_value=''), \
                patch.object(self.stage.shutil, 'which', return_value=None):
            self.stage.collect_release_notices(self.manifest, bundle)
        for name, original in {
            'qemu.origami.paths': qemu / 'LICENSE.origami.paths',
            'qemu.sgi-models.LICENSE': qemu / 'hw/mips/sgi/models/LICENSE',
            'libslirp.COPYRIGHT': slirp,
            'libslirp.NOTICES': extra,
        }.items():
            self.assertEqual((bundle / 'share/sgi/licenses' / name).read_bytes(),
                             original.read_bytes())

    def test_installed_toolchain_notices_include_go_standard_library(self):
        sysroot = self.root / 'rust sysroot'
        docs = sysroot / 'share/doc/rust'
        (docs / 'licenses').mkdir(parents=True)
        (docs / 'COPYRIGHT-library.html').write_text('Rust standard library copyrights')
        (docs / 'licenses/MIT.txt').write_text('MIT terms')
        goroot = self.root / 'go root'
        (goroot / 'src/vendor/dependency').mkdir(parents=True)
        (goroot / 'LICENSE').write_text('Go BSD terms')
        vendor_notice = goroot / 'src/vendor/dependency/LICENSE'
        vendor_notice.write_text('third party standard-library notice')

        def tool_output(*command, **kwargs):
            if command == ('rustc', '-Vv'):
                return 'rustc 1.92.0\nhost: x86_64-unknown-linux-gnu'
            if command == ('rustc', '--print', 'sysroot'):
                return str(sysroot)
            if command == ('go', 'env', 'GOROOT'):
                return str(goroot)
            if command == ('go', 'version'):
                return 'go version go1.26.3 linux/amd64'
            raise AssertionError(command)

        bundle = self.root / 'toolchain output'
        with patch.object(self.stage, 'output', side_effect=tool_output):
            self.stage.toolchain_notices(self.manifest, bundle, {})
        notices = bundle / 'share/sgi/licenses/toolchains'
        self.assertEqual((notices / 'go/src/vendor/dependency/LICENSE').read_bytes(),
                         vendor_notice.read_bytes())
        self.assertEqual((notices / 'rust/COPYRIGHT-library.html').read_text(),
                         'Rust standard library copyrights')

    def test_windows_layout_uses_exe_and_rust_target(self):
        self.manifest.update(platform='windows', rust_target='x86_64-pc-windows-gnu')
        for directory, names in ((Path(self.manifest['qemu_build']),
                                  ('qemu-system-mips64.exe', 'qemu-img.exe')),
                                 (Path(self.manifest['cargo_target_dir']) / 'x86_64-pc-windows-gnu/release',
                                  ('origami.exe',))):
            directory.mkdir(parents=True, exist_ok=True)
            for name in names:
                (directory / name).write_text(name)
        output = self.run_stage()
        self.assertTrue((output / 'bin/origami.exe').is_file())
        self.assertTrue((output / 'libexec/sgi/qemu-img.exe').is_file())


if __name__ == '__main__':
    unittest.main()
