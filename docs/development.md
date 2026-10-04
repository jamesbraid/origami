# Developing Origami

[Contributing](../.github/CONTRIBUTING.md) covers changes and bug reports.
The [user guide](usage.md) covers running downloaded archives.

## Build identity

`origami version` prints the compiled catalogue SHA-256 and the product, QEMU, and
Instigator revisions recorded in an extracted package. A source-tree build
reports itself as unpackaged and prints the pinned QEMU and Instigator revisions.

## Build the product

CMake 3.25 or newer coordinates QEMU, Cargo and Go. Ninja performs the
build, CTest runs the product checks and CPack creates release archives.
QEMU retains its own configure and Meson build. Submodules own the exact
QEMU and Instigator revisions. Builds never fetch or switch their commits.

Initialize them once:

```sh
git submodule update --init -- qemu instigator
```

Alternatively, configure the product and use its explicit initialization
target before building:

```sh
cmake --preset macos
cmake --build --preset macos --target submodules
```

After changing a QEMU pin that updates a Meson dependency patch, refresh the
extracted dependency with Meson before rebuilding. For libslirp, from `qemu/`:

```sh
meson subprojects update --reset libslirp
```

Ordinary builds accept local edits and produce a runnable directory.
Release packaging requires clean source and dependencies at their committed
pins. It also verifies that the binaries match the completed build.

## macOS

Use an Apple Silicon Mac running macOS 15 or newer, with Xcode command-line
tools, Rust, Go 1.26.3 or newer, and Python 3.11 or newer. Install the native
libraries and build tools:

```sh
brew install cmake ninja meson pkg-config glib pixman sdl2 dylibbundler
```

Then, from the product checkout:

```sh
cmake --preset macos
cmake --build --preset macos
./out/macos/run/bin/origami machines
```

Changes rebuild incrementally in the same directory. Tests and release
packaging are separate:

```sh
ctest --preset macos
cpack --preset macos
```

The archive is `origami-macos-arm64-preview.tar.gz`, containing
`macos-arm64-dev/`. Native guest graphics and input remain unqualified.

## Linux

The `linux` preset targets x86-64. Use the supplied rootless Podman build
image to obtain CMake, Ninja, Rust, Go and QEMU's native dependencies:

```sh
podman build -t origami-builder -f build/Product.Containerfile build
podman run --rm --userns=keep-id -v "$PWD:$PWD" -w "$PWD" origami-builder sh -ec '
  cmake --preset linux
  cmake --build --preset linux
  ctest --preset linux
  cpack --preset linux
'
```

The runnable CLI is `out/linux/run/bin/origami`. The archive is
`origami-linux-x86_64-preview.tar.gz`, containing `linux-dev/`.
The release target requires glibc 2.39 or newer. SDL uses the host's display
and input services. Prior Xvfb checks exercised desktop drawing and synthetic
input. They do not qualify physical host input.

## Windows

The `windows-cross` preset builds x86-64 Windows executables on Linux using
QEMU's MinGW toolchain. Native Windows building is not currently provided.

```sh
podman build -t localhost/origami-qemu-windows-builder:dev \
  -f qemu/tests/docker/dockerfiles/fedora-win64-cross.docker qemu/tests/docker/dockerfiles
podman build -t origami-windows-builder -f build/ProductWindows.Containerfile build
podman run --rm --userns=keep-id -v "$PWD:$PWD" -w "$PWD" origami-windows-builder sh -ec '
  cmake --preset windows-cross
  cmake --build --preset windows-cross
  ctest --preset windows-cross
  cpack --preset windows-cross
'
```

The archive is `origami-windows-x86_64-preview.zip`, containing
`windows-dev/`. The runnable Windows CLI is `bin/origami.exe` inside that
directory. Cross-build tests use native Linux test binaries. They do not
replace launch, graphics or input checks on Windows 11.

## Build outputs

The presets use ignored `out/<preset>/` directories. Override a configure
command with `-B /path/to/build` to select external storage, then use
`cmake --build /path/to/build`, `ctest --test-dir /path/to/build` and
`cpack --config /path/to/build/CPackConfig.cmake` for that directory.

The runnable tree contains `origami` and `instigator` under `bin/`, QEMU
and `qemu-img` under `libexec/sgi/`, runtime libraries and QEMU keymaps.
`share/sgi/source-revisions.txt` records actual revisions and checkout state.
`origami --version` (also `origami version`) prints the version and Git identity
carried by each executable. QEMU and Instigator are queried at their launch
paths, including `SGI_RUNTIME_DIR` for QEMU. Replacing a binary changes its
reported identity. Local edits are marked `-dirty`, and unavailable components
are reported on their own lines. Instigator 0.3.1 does not yet support
`--version`, so its line reports `unavailable` until a binary with version
support is installed. Its captures also omit the source revision, while
retaining the binary checksum and timing data.

The frontend uses `vergen-gitcl` with Cargo's package version. QEMU uses its
native package version option with `sgi-origami` and its Git description.
Every build checks the QEMU checkout and refreshes that option when its
identity changes, including local edits. The product build passes Instigator's
own checkout identity through Go linker variables for version-aware binaries.
Go 1.26's automatic VCS discovery skips submodule `.git` files, so automatic
stamping cannot reliably identify those builds.

Release archives add toolchain and dependency notices and manifests.
`SHA256SUMS` covers each packaged file, and CPack writes an archive SHA-256
file alongside the archive. Archives are assembled from the installed
product tree, without build caches or source directories.

GitHub release jobs and local builds use the same presets. Linux and Windows
jobs supply their toolchains through the images above. The Mac builds
natively. Routine checks run on the canonical service, and GitHub builds
and publishes only release artifacts from its own workflow run.

## Source and GitHub builds

Each product release tag pins QEMU and Instigator through Git submodules. To
build the release named on its download page, clone the product repository,
check out that tag and initialize its dependencies:

```sh
git clone https://github.com/jamesbraid/origami.git
cd origami
git checkout RELEASE_TAG
git submodule update --init -- qemu instigator
```

The pinned QEMU and Instigator commits must be available in sibling GitHub
repositories. GitHub's automatic source ZIP omits submodule contents. Use the
Git clone when building from source. The platform build commands above use
these exact checkouts.

CI build checkouts use `fetch-depth: 0` so native version descriptions can
reach release tags in the product and its submodules. A normal initial
`git submodule update --init` creates full dependency clones, but existing
shallow clones need their history fetched explicitly.

To refresh an existing macOS checkout, finish any active install first, then
run these commands from the product repository. Fetching history and tags
does not change the dependency commits selected by the product pin.

```sh
git pull --recurse-submodules
git submodule update --init -- qemu instigator
for component in qemu instigator; do
  if [ "$(git -C "$component" rev-parse --is-shallow-repository)" = true ]; then
    git -C "$component" fetch --unshallow --tags origin
  else
    git -C "$component" fetch --tags origin
  fi
done
cmake --build --preset macos
./out/macos/run/bin/origami --version
```

## Licenses and dependency sources

The Origami CLI, Instigator and original Origami additions to QEMU use
BSD-3-Clause. QEMU as a whole uses GPLv2. Upstream and adapted code retain
their existing licenses. `qemu/LICENSE.origami.paths` lists original files
covered by the BSD grant, and `qemu/LICENSE.origami` contains its terms.
Bundled libraries retain their own licenses, with notices under
`share/sgi/licenses/` in the archive.

`share/sgi/source-revisions.txt` identifies the product, QEMU and Instigator
commits. The Debian and Windows library manifests record the exact binary and
source package versions. Rust and Go dependency manifests identify their
modules and bundled notices.

QEMU statically links a patched libslirp. Its pinned source URL and checksum
are in `qemu/subprojects/libslirp.wrap`. The patch and its tests are in
`qemu/subprojects/packagefiles/libslirp-sgi-prom.patch` in the initialized
QEMU checkout.

## Preview archive workflow

The `Build release archives` workflow runs for version tags or manually
requested builds. Public GitHub tag builds produce Linux, macOS and Windows
archives, then publish a prerelease with their checksums. Windows is
cross-compiled on Linux. Only assets produced in that workflow run are
uploaded. Private GitHub repositories skip these jobs. Pull requests and
`main` pushes do not build archives.

Manual runs build the selected platform without publishing a release.
Routine development checks and additional archive builds can use the same
workflow on the project's build runners.

The Linux and macOS jobs extract their archives into paths containing spaces
and run the product smoke test. It checks bundled executables, disk creation,
QEMU start/status/stop, a VNC connection, concurrent-launch refusal and
drive-edit locks using
an original synthetic MIPS loop. No firmware or guest media is needed.
To run it against an extracted package:

```sh
python3 build/smoke-test.py /path/to/linux-dev --scratch /path/to/scratch
```

This checks product integration, not firmware or guest compatibility. Run
artifacts contain archives and SHA-256 files. Firmware and guest media are
not included.
