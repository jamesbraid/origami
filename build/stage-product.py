#!/usr/bin/env python3
# SPDX-License-Identifier: BSD-3-Clause
"""Stage the compiled product; CPack owns archive creation."""

import argparse
import configparser
import hashlib
import json
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path


def output(*command, **kwargs):
    return subprocess.check_output(command, text=True, **kwargs).strip()


def git(source, *args):
    return output('git', '--no-optional-locks', '-C', str(source), *args)


def revisions(manifest, release):
    records = {}
    product = Path(manifest['product_source']).resolve()
    for component in ('product', 'qemu', 'instigator'):
        source = Path(manifest[component + '_source']).resolve()
        if Path(git(source, 'rev-parse', '--show-toplevel')).resolve() != source:
            raise RuntimeError(f'{component} source is not a Git checkout root: {source}')
        revision = git(source, 'rev-parse', 'HEAD')
        dirty = bool(git(source, 'status', '--porcelain', '--untracked-files=all',
                         '--ignore-submodules=all'))
        if release and dirty:
            raise RuntimeError(f'{component} source is dirty: {source}')
        if release and component != 'product':
            entry = git(product, 'ls-tree', 'HEAD', '--', component).split()
            if len(entry) < 4 or entry[:2] != ['160000', 'commit']:
                raise RuntimeError(f'{component} has no committed gitlink pin')
            if revision != entry[2]:
                raise RuntimeError(f'{component} revision {revision} differs from gitlink pin {entry[2]}')
        records[component] = (revision, 'dirty' if dirty else 'clean')
    return records



def file_hash(path):
    with path.open('rb') as source:
        return hashlib.file_digest(source, 'sha256').hexdigest()


def source_identity(manifest):
    identity = {}
    for component in ('product', 'qemu', 'instigator'):
        source = Path(manifest[component + '_source']).resolve()
        command = ['git', '--no-optional-locks', '-c', 'diff.autoRefreshIndex=false',
                   '-C', str(source)]
        diff = subprocess.check_output(command + ['diff', '--binary',
                                                 '--ignore-submodules=all', 'HEAD'])
        status = subprocess.check_output(command + ['status', '--porcelain=v1', '-z',
                                                    '--untracked-files=all',
                                                    '--ignore-submodules=all'])
        untracked = subprocess.check_output(command + ['ls-files', '-z', '--others',
                                                        '--exclude-standard'])
        content = {}
        for encoded in sorted(name for name in untracked.split(b'\0') if name):
            name = os.fsdecode(encoded)
            path = source / name
            if path.is_symlink():
                content[name] = hashlib.sha256(os.fsencode(os.readlink(path))).hexdigest()
            elif path.is_file():
                content[name] = file_hash(path)
            else:
                raise RuntimeError(f'cannot record untracked source: {path}')
        identity[component] = {
            'revision': git(source, 'rev-parse', 'HEAD'),
            'diff_sha256': hashlib.sha256(diff).hexdigest(),
            'status_sha256': hashlib.sha256(status).hexdigest(),
            'untracked': content,
        }
    return {'sources': identity, 'configuration': manifest}


def state_path(manifest, name):
    return Path(manifest['qemu_build']).resolve().parent / name


def save_state(path, state):
    path.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(mode='w', prefix=path.name + '-',
                                     dir=path.parent, delete=False) as stream:
        temporary = Path(stream.name)
        json.dump(state, stream, sort_keys=True, indent=2)
        stream.write('\n')
    try:
        temporary.replace(path)
    finally:
        temporary.unlink(missing_ok=True)


def read_state(path, description):
    if not path.is_file():
        raise RuntimeError(f'missing {description}; rebuild the product through CMake')
    return json.loads(path.read_text())


def record_inputs(manifest):
    revisions(manifest, release=False)
    # Packaging must not consume an old completion record while a new build runs.
    state_path(manifest, 'build-provenance.json').unlink(missing_ok=True)
    save_state(state_path(manifest, 'build-inputs.json'), source_identity(manifest))


def artifact_identity(manifest):
    return {name: file_hash(path) for name, path in layout(manifest).items()}


def record_build(manifest):
    started = read_state(state_path(manifest, 'build-inputs.json'), 'build input record')
    finished = source_identity(manifest)
    if started != finished:
        raise RuntimeError('source or configuration changed during the build; rebuild the product')
    artifacts = artifact_identity(manifest)
    if source_identity(manifest) != finished:
        raise RuntimeError('source changed while recording build artifacts; rebuild the product')
    save_state(state_path(manifest, 'build-provenance.json'),
               {'inputs': finished, 'artifacts': artifacts})


def verify_build(manifest):
    completed = read_state(state_path(manifest, 'build-provenance.json'), 'completed build record')
    if completed['inputs'] != source_identity(manifest):
        raise RuntimeError('current source or configuration differs from the completed build; rebuild the product')
    if completed['artifacts'] != artifact_identity(manifest):
        raise RuntimeError('built artifacts differ from the completed build record; rebuild the product')


def layout(manifest):
    if manifest['platform'] not in ('linux', 'macos', 'windows'):
        raise RuntimeError(f"unsupported platform: {manifest['platform']}")
    if manifest['build_type'] not in ('debug', 'release'):
        raise RuntimeError(f"unsupported build type: {manifest['build_type']}")
    suffix = '.exe' if manifest['platform'] == 'windows' else ''
    target = Path(manifest['cargo_target_dir'])
    if manifest['rust_target']:
        target /= manifest['rust_target']
    target /= manifest['build_type']
    qemu = Path(manifest['qemu_build'])
    files = {
        'bin/origami' + suffix: target / ('origami' + suffix),
        'bin/instigator' + suffix: Path(manifest['instigator_binary']),
        'libexec/sgi/qemu-system-mips64' + suffix: qemu / ('qemu-system-mips64' + suffix),
        'libexec/sgi/qemu-img' + suffix: qemu / ('qemu-img' + suffix),
    }
    keymaps = Path(manifest['qemu_source']) / 'pc-bios/keymaps'
    if not keymaps.is_dir():
        raise RuntimeError(f'missing QEMU keymaps: {keymaps}')
    for path in sorted(keymaps.iterdir()):
        if path.name != 'meson.build' and path.is_file():
            files['share/sgi/qemu/keymaps/' + path.name] = path
    for source in files.values():
        if not source.is_file() or not source.stat().st_size:
            raise RuntimeError(f'missing or empty staging input: {source}')
    return files


def validate_destination(manifest, destination, files):
    if destination.is_symlink():
        raise RuntimeError(f'output must not be a symlink: {destination}')
    resolved = destination.resolve()
    protected = [Path(manifest[name]).resolve() for name in
                 ('product_source', 'qemu_source', 'instigator_source', 'qemu_build',
                  'cargo_target_dir', 'cargo_home', 'go_mod_cache', 'go_build_cache')]
    protected.extend(path.resolve() for path in files.values())
    if any(path.is_relative_to(resolved) for path in protected):
        raise RuntimeError(f'output would replace a source or build input: {destination}')
    for name in ('product_source', 'qemu_source', 'instigator_source'):
        source = Path(manifest[name]).resolve()
        if resolved.is_relative_to(source):
            ignored = subprocess.run(['git', '-C', str(source), 'check-ignore', '-q',
                                      str(resolved)], check=False)
            if ignored.returncode != 0:
                raise RuntimeError(f'output inside source must be Git ignored: {destination}')
    if destination.exists():
        if not destination.is_dir():
            raise RuntimeError(f'output is not a directory: {destination}')
        if any(destination.iterdir()) and not (destination / 'share/sgi/source-revisions.txt').is_file():
            raise RuntimeError(f'output contains files not owned by product staging: {destination}')


def helper(manifest, name, *args, **kwargs):
    script = Path(manifest['product_source']) / 'build' / name
    subprocess.run([manifest['python'], str(script), *(str(arg) for arg in args)],
                   check=True, **kwargs)


def bundle_runtime(manifest, bundle):
    if manifest['platform'] == 'windows' and not Path('/usr/x86_64-w64-mingw32/sys-root/mingw/bin').is_dir():
        raise RuntimeError('Windows staging requires the Fedora MinGW cross toolchain; '
                           'native MSYS2 packaging is not yet supported')
    helper(manifest, 'bundle-' + manifest['platform'] + '.py', bundle)


def copy_notice(source, destination):
    if not source.is_file() or not source.stat().st_size:
        raise RuntimeError(f'missing or empty license notice: {source}')
    destination.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(source, destination)


def package_notices(paths, destination):
    """Keep installed compiler and standard-library package notices together."""
    packages = set()
    records = []
    for path in paths:
        path = path.resolve()
        if shutil.which('dpkg-query'):
            owner = output('dpkg-query', '-S', str(path)).split(': ', 1)[0].split(',', 1)[0]
            if owner in packages:
                continue
            packages.add(owner)
            records.append(output('dpkg-query', '-W', '-f=${binary:Package} ${Version} ${Architecture}', owner))
            notice = Path('/usr/share/doc') / owner.split(':', 1)[0] / 'copyright'
            copy_notice(notice, destination / (owner.replace(':', '_') + '.copyright'))
        elif shutil.which('rpm'):
            owner = output('rpm', '-qf', '--qf', '%{NAME}', str(path))
            if owner in packages:
                continue
            packages.add(owner)
            records.append(output('rpm', '-q', '--qf', '%{NAME} %{VERSION}-%{RELEASE}.%{ARCH}', owner))
            notices = output('rpm', '-q', '--licensefiles', owner).splitlines()
            if not notices:
                raise RuntimeError(f'no installed RPM license files for {owner}')
            for name in notices:
                source = Path(name)
                if not source.is_relative_to('/usr/share'):
                    raise RuntimeError(f'unexpected RPM license path: {source}')
                copy_notice(source, destination / owner / source.relative_to('/usr/share'))
        else:
            raise RuntimeError(f'no installed toolchain notices or package owner for {path}')
    (destination / 'packages.txt').write_text('\n'.join(records) + '\n')


def toolchain_notices(manifest, bundle, env):
    root = bundle / 'share/sgi/licenses/toolchains'
    rust = root / 'rust'
    rust.mkdir(parents=True)
    rustc = manifest['rustc']
    rust_version = output(rustc, '-Vv')
    (rust / 'toolchain.txt').write_text(rust_version + '\n')
    sysroot = Path(output(rustc, '--print', 'sysroot'))
    docs = next((sysroot / 'share/doc' / name for name in ('rust', 'rustc')
                 if (sysroot / 'share/doc' / name / 'COPYRIGHT-library.html').is_file()), None)
    if docs:
        copy_notice(docs / 'COPYRIGHT-library.html', rust / 'COPYRIGHT-library.html')
        if not (docs / 'licenses/MIT.txt').is_file():
            raise RuntimeError(f'missing Rust standard-library notices: {docs}')
        shutil.copytree(docs / 'licenses', rust / 'licenses')
    else:
        host = next(line.split(': ', 1)[1] for line in rust_version.splitlines() if line.startswith('host: '))
        paths = [Path(shutil.which(rustc) or rustc)]
        for target in sorted({host, manifest['rust_target'] or host}):
            standard = sorted((sysroot / 'lib/rustlib' / target / 'lib').glob('libstd-*.rlib'))
            if not standard:
                raise RuntimeError(f'missing Rust standard library for {target} in {sysroot}')
            paths.append(standard[0])
        package_notices(paths, rust)
    go = root / 'go'
    go.mkdir()
    goroot = Path(output(manifest['go'], 'env', 'GOROOT', env=env))
    (go / 'toolchain.txt').write_text(output(manifest['go'], 'version', env=env) + '\n'
                                     + f'GOROOT={goroot}\n')
    license_file = next((path for path in (goroot / 'LICENSE', goroot.parent / 'LICENSE')
                         if path.is_file()), None)
    if license_file:
        copy_notice(license_file, go / 'LICENSE')
        for path in sorted((goroot / 'src').rglob('*')):
            name = path.name.upper()
            if path.is_file() and (name.startswith(('LICENSE', 'COPYING', 'NOTICE'))
                                   or name in ('COPYRIGHT', 'PATENTS')):
                copy_notice(path, go / path.relative_to(goroot))
    else:
        package_notices([Path(shutil.which(manifest['go']) or manifest['go']),
                         goroot / 'src/runtime/proc.go'], go)


def collect_release_notices(manifest, bundle):
    product = Path(manifest['product_source'])
    qemu = Path(manifest['qemu_source'])
    notices = bundle / 'share/sgi/licenses'
    sources = {
        'origami.LICENSE': product / 'LICENSE',
        'instigator.LICENSE': Path(manifest['instigator_source']) / 'LICENSE',
        'qemu.LICENSE': qemu / 'LICENSE',
        'qemu.origami.LICENSE': qemu / 'LICENSE.origami',
        'qemu.origami.paths': qemu / 'LICENSE.origami.paths',
        'qemu.COPYING': qemu / 'COPYING',
        'qemu.COPYING.LIB': qemu / 'COPYING.LIB',
        'qemu.sgi-models.LICENSE': qemu / 'hw/mips/sgi/models/LICENSE',
    }
    wrap = configparser.ConfigParser()
    wrap.read(qemu / 'subprojects/libslirp.wrap')
    directory = wrap.get('wrap-file', 'directory')
    if Path(directory).name != directory or directory in ('.', '..'):
        raise RuntimeError(f'invalid libslirp source directory: {directory}')
    slirp = next((root / 'subprojects' / directory / 'COPYRIGHT'
                  for root in (qemu, Path(manifest['qemu_build']))
                  if (root / 'subprojects' / directory / 'COPYRIGHT').is_file()), None)
    if slirp is None:
        raise RuntimeError(f'missing libslirp source notice for {directory}')
    sources['libslirp.COPYRIGHT'] = slirp
    sources['libslirp.NOTICES'] = product / 'build/licenses' / (directory + '.copyright')
    for name, source in sources.items():
        copy_notice(source, notices / name)
    env = os.environ.copy()
    env.update(GOMODCACHE=manifest['go_mod_cache'], GOCACHE=manifest['go_build_cache'],
               GOTOOLCHAIN='local', CGO_ENABLED='0')
    env['GOOS'] = {'linux': 'linux', 'macos': 'darwin', 'windows': 'windows'}[manifest['platform']]
    env['GOARCH'] = 'arm64' if manifest['platform'] == 'macos' else 'amd64'
    toolchain_notices(manifest, bundle, env)
    dependencies = output(manifest['go'], 'list', '-mod=readonly', '-deps',
                          '-f', '{{if .Module}}{{.Module.Path}}|{{.Module.Version}}|{{.Module.Dir}}{{end}}',
                          './cmd/instigator', cwd=manifest['instigator_source'], env=env)
    # This file is an input to the existing collector, not product payload.
    with tempfile.TemporaryDirectory(prefix='go-notices-', dir=bundle.parent) as scratch:
        deps = Path(scratch) / 'dependencies.txt'
        deps.write_text('\n'.join(sorted(set(dependencies.splitlines()))) + '\n')
        helper(manifest, 'bundle-go-licenses.py', deps, bundle, manifest['go_mod_cache'])
    helper(manifest, 'bundle-rust-licenses.py', manifest['cargo_home'], bundle)
    if manifest['platform'] == 'macos':
        with tempfile.TemporaryDirectory(prefix='homebrew-notices-', dir=bundle.parent) as scratch:
            helper(manifest, 'collect-homebrew-notices.py', bundle, scratch)
    if shutil.which('rpm'):
        (bundle / 'share/sgi/build-packages.txt').write_text('\n'.join(sorted(output('rpm', '-qa').splitlines())) + '\n')


def checksums(bundle):
    lines = []
    for directory in ('bin', 'lib', 'libexec', 'share'):
        for path in sorted((bundle / directory).rglob('*')):
            if path.is_file():
                with path.open('rb') as source:
                    digest = hashlib.file_digest(source, 'sha256').hexdigest()
                lines.append(f'{digest}  {path.relative_to(bundle).as_posix()}\n')
    (bundle / 'SHA256SUMS').write_text(''.join(lines))


def stage_product(manifest, release=False, output_dir=None):
    destination = Path(output_dir or manifest['output_dir'])
    files = layout(manifest)
    validate_destination(manifest, destination, files)
    records = revisions(manifest, release)
    verify_build(manifest)
    destination.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='product-stage-', dir=destination.parent) as scratch:
        bundle = Path(scratch) / 'payload'
        for name, source in files.items():
            target = bundle / name
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(source, target)
        revision_file = bundle / 'share/sgi/source-revisions.txt'
        revision_file.write_text(''.join(f'{name}={revision} ({state})\n'
                                        for name, (revision, state) in records.items()))
        bundle_runtime(manifest, bundle)
        if release:
            collect_release_notices(manifest, bundle)
        # ZIP timestamps begin in 1980, including notices copied from crate archives.
        if manifest['platform'] == 'windows':
            for path in bundle.rglob('*'):
                if path.is_file() and path.stat().st_mtime < 315619200:
                    os.utime(path, (315619200, 315619200))
        checksums(bundle)
        verify_build(manifest)
        previous = Path(scratch) / 'previous'
        if destination.exists():
            destination.rename(previous)
        try:
            bundle.rename(destination)
        except OSError:
            if previous.exists():
                previous.rename(destination)
            raise
    return destination


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('manifest', type=Path)
    modes = parser.add_mutually_exclusive_group()
    modes.add_argument('--release', action='store_true')
    modes.add_argument('--record-inputs', action='store_true')
    modes.add_argument('--record-build', action='store_true')
    parser.add_argument('--output', type=Path)
    args = parser.parse_args()
    try:
        manifest = json.loads(args.manifest.read_text())
        if args.record_inputs:
            record_inputs(manifest)
            print('recorded product build inputs')
            return
        if args.record_build:
            record_build(manifest)
        destination = stage_product(manifest, release=args.release, output_dir=args.output)
    except (OSError, RuntimeError, ValueError, KeyError, configparser.Error, subprocess.CalledProcessError) as error:
        parser.exit(1, f'product staging failed: {error}\n')
    print(f'staged product: {destination}')


if __name__ == '__main__':
    main()
