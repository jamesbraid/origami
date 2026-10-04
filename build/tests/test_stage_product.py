# SPDX-License-Identifier: BSD-3-Clause
import hashlib
import importlib.util
import os
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
        self.manifest['files'] = {
            'bin/origami': str(Path(self.manifest['cargo_target_dir']) / 'release/origami'),
            'bin/instigator': self.manifest['instigator_binary'],
            'libexec/sgi/qemu-system-mips64': str(Path(self.manifest['qemu_build']) / 'qemu-system-mips64'),
            'libexec/sgi/qemu-img': str(Path(self.manifest['qemu_build']) / 'qemu-img'),
            'share/sgi/qemu/keymaps/en-us': str(self.sources['qemu'] / 'pc-bios/keymaps/en-us'),
            'share/sgi/qemu/keymaps/common': str(self.sources['qemu'] / 'pc-bios/keymaps/common'),
        }
        self.manifest['go_environment'] = [
            'GOMODCACHE=' + self.manifest['go_mod_cache'],
            'GOCACHE=' + self.manifest['go_build_cache'], 'CGO_ENABLED=0',
            'XDG_CONFIG_HOME=' + str(Path(self.manifest['qemu_build']).parent / 'tool-config'),
        ]
        self.assertTrue(SCRIPT.is_file(), 'shared staging entry point is missing')
        spec = importlib.util.spec_from_file_location('stage_product', SCRIPT)
        self.stage = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(self.stage)

    def run_stage(self, release=False):
        # Fixture files are text, so platform linkage is tested by the existing bundler tests.
        with patch.object(self.stage, 'bundle_runtime') as runtime, \
                patch.object(self.stage, 'collect_release_notices') as notices:
            result = self.stage.stage_product(self.manifest, release=release)
            self.assertEqual(notices.call_count, int(release))
            runtime.assert_called_once()
            return result

    def test_qemu_preparation_refreshes_the_package_identity(self):
        source = self.sources['qemu']
        git(source, 'tag', '-a', 'v11.1.0', '-m', 'test release')
        build = Path(self.manifest['qemu_build'])
        meson = build / 'pyvenv/bin/meson'
        meson.parent.mkdir(parents=True)
        tool = self.root / 'qemu-tool'
        tool.write_text("""#!/usr/bin/env python3
import json, pathlib, sys
build = pathlib.Path.cwd()
args = sys.argv[1:]
if pathlib.Path(sys.argv[0]).name == 'meson':
    build = pathlib.Path(args[-1])
    state = build / 'package-option.json'
    if args[0] == 'introspect':
        print(json.dumps([{'name': 'pkgversion', 'value': state.read_text() if state.exists() else ''}]))
    else:
        value = next(arg.split('=', 1)[1] for arg in args if arg.startswith('-Dpkgversion='))
        state.write_text(value)
        with (build / 'configurations').open('a') as log: log.write(value + '\\n')
""")
        tool.chmod(0o755)
        meson.symlink_to(tool)
        artifact = build / 'package-option.json'
        self.stage.prepare_qemu(self.manifest)
        self.assertEqual(artifact.read_text(), 'sgi-origami v11.1.0')
        self.stage.prepare_qemu(self.manifest)
        self.assertEqual(len((build / 'configurations').read_text().splitlines()), 1)
        index = source / '.git/index'
        original_index = index.read_bytes()
        (source / 'LICENSE').write_text('local QEMU changes')
        self.stage.prepare_qemu(self.manifest)
        self.assertEqual(artifact.read_text(), 'sgi-origami v11.1.0-dirty')
        self.assertEqual(index.read_bytes(), original_index)
        git(source, 'add', 'LICENSE')
        git(source, 'commit', '-qm', 'test: advance QEMU checkout')
        self.stage.prepare_qemu(self.manifest)
        self.assertEqual(artifact.read_text(), 'sgi-origami ' + git(source, 'describe', '--match', 'v*'))
        self.assertEqual(len((build / 'configurations').read_text().splitlines()), 3)

    def test_source_checks_preserve_git_index(self):
        product = self.sources['product']
        index = product / '.git/index'
        before = index.read_bytes()
        tracked = product / 'LICENSE'
        stat = tracked.stat()
        os.utime(tracked, ns=(stat.st_atime_ns, stat.st_mtime_ns + 2_000_000_000))
        self.stage.revisions(self.manifest, release=False)
        self.assertEqual(index.read_bytes(), before)

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

    def test_stage_uses_configured_artifact_paths(self):
        configured = self.root / 'custom frontend output'
        configured.write_text("configured frontend")
        self.manifest['files'] = {
            'bin/origami': str(configured),
            'bin/instigator': self.manifest['instigator_binary'],
        }
        output = self.run_stage()
        self.assertEqual((output / 'bin/origami').read_bytes(), configured.read_bytes())

    def test_configured_destination_cannot_escape_bundle(self):
        for name in ('../outside', '/outside'):
            with self.subTest(destination=name):
                self.manifest['files'] = {name: self.manifest['instigator_binary']}
                with self.assertRaisesRegex(RuntimeError, 'invalid staging path'):
                    self.run_stage()

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
        with patch.object(self.stage, 'toolchain_notices') as toolchain, \
                patch.object(self.stage, 'helper'), \
                patch.object(self.stage, 'output', return_value=''), \
                patch.object(self.stage.shutil, 'which', return_value=None):
            self.stage.collect_release_notices(self.manifest, bundle)
        config = Path(toolchain.call_args.args[2]['XDG_CONFIG_HOME'])
        self.assertEqual(config, Path(self.manifest['qemu_build']).parent / 'tool-config')
        self.assertFalse(config.is_relative_to(self.sources['product']))
        for name, original in {
            'qemu.origami.paths': qemu / 'LICENSE.origami.paths',
            'qemu.sgi-models.LICENSE': qemu / 'hw/mips/sgi/models/LICENSE',
            'libslirp.COPYRIGHT': slirp,
            'libslirp.NOTICES': extra,
        }.items():
            self.assertEqual((bundle / 'share/sgi/licenses' / name).read_bytes(),
                             original.read_bytes())

    def test_go_notices_use_configured_build_environment(self):
        qemu = self.sources['qemu']
        wrap = qemu / 'subprojects/libslirp.wrap'
        wrap.parent.mkdir()
        wrap.write_text('[wrap-file]\ndirectory = libslirp-test\n')
        notice = Path(self.manifest['qemu_build']) / 'subprojects/libslirp-test/COPYRIGHT'
        notice.parent.mkdir(parents=True)
        notice.write_text('notice')
        extra = self.sources['product'] / 'build/licenses/libslirp-test.copyright'
        extra.parent.mkdir(parents=True)
        extra.write_text('notice')
        self.manifest['go_environment'] = [
            'GOMODCACHE=' + str(self.root / 'configured modules'),
            'GOCACHE=' + str(self.root / 'configured compilation'),
            'CGO_ENABLED=0', 'GOOS=windows', 'GOARCH=amd64',
            'XDG_CONFIG_HOME=' + str(self.root / 'configured settings'),
        ]
        bundle = self.root / 'notice output'
        bundle.mkdir()
        with patch.object(self.stage, 'toolchain_notices') as toolchain, \
                patch.object(self.stage, 'helper'), \
                patch.object(self.stage, 'output', return_value=''), \
                patch.object(self.stage.shutil, 'which', return_value=None):
            self.stage.collect_release_notices(self.manifest, bundle)
        environment = toolchain.call_args.args[2]
        self.assertEqual(environment['GOMODCACHE'], str(self.root / 'configured modules'))
        self.assertEqual(environment['GOCACHE'], str(self.root / 'configured compilation'))
        self.assertEqual(environment['GOOS'], 'windows')
        self.assertEqual(environment['XDG_CONFIG_HOME'], str(self.root / 'configured settings'))

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
                self.assertEqual(kwargs.get('cwd'), self.manifest['instigator_source'])
                return str(goroot)
            if command == ('go', 'version'):
                return 'go version go1.27.1 linux/amd64'
            raise AssertionError(command)

        bundle = self.root / 'toolchain output'
        with patch.object(self.stage, 'output', side_effect=tool_output):
            self.stage.toolchain_notices(self.manifest, bundle, {})
        notices = bundle / 'share/sgi/licenses/toolchains'
        self.assertEqual((notices / 'go/src/vendor/dependency/LICENSE').read_bytes(),
                         vendor_notice.read_bytes())
        self.assertEqual((notices / 'rust/COPYRIGHT-library.html').read_text(),
                         'Rust standard library copyrights')

    def test_stage_uses_configured_windows_artifacts(self):
        self.manifest.update(platform='windows', rust_target='x86_64-pc-windows-gnu')
        for directory, names in ((Path(self.manifest['qemu_build']),
                                  ('qemu-system-mips64.exe', 'qemu-img.exe')),
                                 (Path(self.manifest['cargo_target_dir']) / 'x86_64-pc-windows-gnu/release',
                                  ('origami.exe',))):
            directory.mkdir(parents=True, exist_ok=True)
            for name in names:
                (directory / name).write_text(name)
        self.manifest['files'] = {
            'bin/origami.exe': str(Path(self.manifest['cargo_target_dir']) / 'x86_64-pc-windows-gnu/release/origami.exe'),
            'bin/instigator.exe': self.manifest['instigator_binary'],
            'libexec/sgi/qemu-system-mips64.exe': str(Path(self.manifest['qemu_build']) / 'qemu-system-mips64.exe'),
            'libexec/sgi/qemu-img.exe': str(Path(self.manifest['qemu_build']) / 'qemu-img.exe'),
        }
        output = self.run_stage()
        self.assertTrue((output / 'bin/origami.exe').is_file())
        self.assertTrue((output / 'libexec/sgi/qemu-img.exe').is_file())


if __name__ == '__main__':
    unittest.main()
