# Developing Origami

[Contributing](../.github/CONTRIBUTING.md) covers changes and bug reports.
The [user guide](usage.md) covers running downloaded archives.

## Build identity

`origami --version` (also `origami version`) prints three lines: Origami,
QEMU and Instigator. Each executable carries its own build identity.
Origami uses Git tags: `v0.2.0` at a release, `v0.2.0-5-gabc1234` after it,
and a `-dirty` suffix for tracked local edits. Without a version tag it uses
the commit ID. Builds without Git metadata report `unknown`.

Create an annotated `vX.Y.Z` tag on the product commit to select a release.
That commit also selects the QEMU and Instigator pins. Build from the tag,
run the tests, then package those binaries. Rebuild after adding or changing
a tag so the embedded version matches the archive version.
Cargo's `0.0.0` is a package placeholder. The Git tag owns the release version,
including CPack's package version.

## Build the product

CMake 3.25 or newer coordinates QEMU, Cargo and Go. The presets use Ninja
for product orchestration. CMake invokes QEMU's GNU Make entry point, which
owns reconfiguration and its Meson/Ninja build. CTest runs the product checks
and CPack creates release archives. Submodules own the exact
QEMU, Instigator and vcpkg revisions. Builds never fetch or switch their commits.

CMake `install()` rules lay out the product, and `cmake/check-libraries.cmake`
stops the installation if an executable needs a shared library that is
neither in the archive nor part of the host platform. Cargo, Go and QEMU own
incremental rebuilds.

Instigator's `go.mod` owns its required Go version. The container images
install the distribution's Go as a bootstrap toolchain, and the build sets
`GOTOOLCHAIN=auto` so Go downloads the version the pinned Instigator checkout
requires. A Go version bump belongs in Instigator. The product picks it up
through its submodule pin. macOS CI also reads `instigator/go.mod` through
`setup-go`.

Initialize the submodules once; configuration stops with this command if
they are missing:

```sh
git submodule update --init
```

## C libraries

vcpkg builds GLib, pixman and SDL 2 during configuration, and QEMU links
them statically. `vcpkg.json` lists them; the `vcpkg` submodule pins their
versions, so update them by moving the submodule. Triplet overrides live in
`cmake/vcpkg/triplets/`. The first configuration takes several minutes, and
later ones reuse `~/.cache/vcpkg`. Host display, audio and system libraries
come from the user's system.

When libslirp's wrap or patch files change, the product build runs
`meson subprojects update --reset libslirp` through QEMU's build environment
before compilation. Unchanged builds reuse the extracted dependency.

The default build installs a runnable tree into `run/` in the build
directory. Release builds follow configure, build, test and package in that
order. CPack installs the existing binaries without invoking compilation. Run
the build again after changing source or release tags, then test and package.

## macOS

Use an Apple Silicon Mac running macOS 15 or newer, with Xcode command-line
tools, Rust, Go with automatic toolchain selection, and Python 3.12 or newer.
Install the build tools:

```sh
brew install cmake ninja meson pkg-config autoconf automake libtool
```

Then, from the product checkout:

```sh
cmake --preset macos
cmake --build --preset macos
./out/macos/run/bin/origami machines
```

CMake applies the preset's vcpkg toolchain only to a new build directory, so a
directory configured before vcpkg was added fails with "vcpkg's toolchain is
not loaded". Reconfigure it with `cmake --preset macos --fresh`.

Changes rebuild incrementally in the same directory. To create a release
archive from a clean checkout:

```sh
cmake --build --preset macos --target binaries
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
The release target requires glibc 2.39 or newer and the host's PulseAudio and
ALSA client libraries. The archive ships no shared libraries. Graphics
drivers, audio, X11, Wayland and udev come from the host,
so they match its drivers and services. SDL uses the host's display
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

The default build installs the runnable development tree. Release CI builds
`--target binaries` to skip that installation, runs CTest, then lets CPack
install the release tree once. Use the same target locally when only an
archive is needed.

The runnable tree contains `origami` and `instigator` under `bin/`, QEMU,
`qemu-img` and `qemu-sgi-machine-init` under `libexec/origami/` (with the
MinGW thread DLL on Windows) and QEMU keymaps under
`share/origami/qemu/`. `origami --version` (also `origami version`) prints
the version and Git identity carried by each executable. QEMU and Instigator
are queried at their launch paths, including `ORIGAMI_RUNTIME_DIR` for QEMU.
Replacing a binary changes its reported identity. Local edits are marked
dirty, and unavailable components are reported on their own lines.

The frontend embeds its Git description through `vergen-gitcl`. QEMU uses its
native package version option with `sgi-origami` and its Git description.
Every build checks the QEMU checkout and refreshes that option when its
identity changes, including local edits. Instigator uses Go's native module
version and VCS build information from its own checkout.

CPack writes an archive SHA-256 file alongside the archive. Archives are
assembled from the installed product tree, without build caches or source
directories.

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
git submodule update --init
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

## Machine catalogue and state

The CLI reads its machine offerings from the QEMU it launches. It starts that
QEMU with no machine and asks for `query-sgi-machines` over QMP each time a
command needs an offering. A QEMU pin change that alters the catalogue therefore
changes the offerings without a frontend edit, and a frontend change that
needs new catalogue data needs the matching QEMU pin.

The Rust tests read `tests/fixtures/sgi-machines.json`, a short hand-written
excerpt of that reply with one offering of each kind the tests exercise.
Anything that needs the full catalogue belongs in the product-state test
below, which queries the real QEMU.

`create` runs `qemu-sgi-machine-init` from beside QEMU to build a machine's
storage, and launches attach each catalogue storage item as a block node of
the same name. `cargo test` uses stand-ins for QEMU and the init tool, so it
needs neither a QEMU build nor the submodules. One ignored test checks that
every starter preset names an offering of the real catalogue, then creates
and reopens each with the real tools. Native product builds run
it in CTest as `product-state`. To run it against an existing build:

```sh
ORIGAMI_RUNTIME_DIR="$PWD/out/linux/run/libexec/origami" \
  cargo test --test product_state -- --ignored
```

## Licenses and dependency sources

The Origami CLI, Instigator and original Origami additions to QEMU use
BSD-3-Clause. QEMU as a whole uses GPLv2. Upstream and adapted code retain
their existing licenses. `qemu/LICENSE.origami.paths` lists original files
covered by the BSD grant, and `qemu/LICENSE.origami` contains its terms.
Archives install these licenses, each vcpkg library's `copyright` file and
`THIRD_PARTY_NOTICES` under `share/origami/licenses/`. The notices file is
maintained by hand: update it when a Rust crate or Go module is added or
changes license.

QEMU statically links a patched libslirp. Its pinned source URL and checksum
are in `qemu/subprojects/libslirp.wrap`. The patch and its tests are in
`qemu/subprojects/packagefiles/libslirp-sgi-prom.patch` in the initialized
QEMU checkout.

## Preview archive workflow

The `Build release archives` workflow runs for version tags or manually
requested builds. GitHub tag builds produce Linux, macOS and Windows
archives, then publish a prerelease with their checksums. Windows is
cross-compiled on Linux. Only assets produced in that workflow run are
uploaded. Pull requests and `main` pushes do not build archives. GitHub jobs
keep vcpkg's binary cache between runs.

Manual runs build the selected platform without publishing a release.
Routine development checks and additional archive builds can use the same
workflow on the project's build runners.

Each platform's archive is extracted into a path containing spaces and run
through the product smoke test, Windows on a Windows runner. It checks bundled executables, disk creation,
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
