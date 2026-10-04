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
That commit also selects the QEMU and Instigator pins. Build from the tag.
Packaging invokes the native builds again so tag changes reach the archived
binaries.
Cargo's `0.0.0` is a package placeholder. The Git tag owns the release version,
including CPack's package version.

## Build the product

CMake 3.25 or newer coordinates QEMU, Cargo and Go. The presets use Ninja
for product orchestration. CMake invokes QEMU's GNU Make entry point, which
owns reconfiguration and its Meson/Ninja build. CTest runs the product checks
and CPack creates release archives. Submodules own the exact
QEMU and Instigator revisions. Builds never fetch or switch their commits.

CMake defines the compiler commands, target paths and Go environment once.
The staging helper consumes that generated configuration to assemble the
runnable tree, bundle libraries and collect notices. Cargo, Go and QEMU
own incremental rebuilds.

Instigator's `go.mod` owns its required Go version. The container images
install the distribution's Go as a bootstrap toolchain. Go automatically
selects and downloads the required toolchain from the pinned Instigator
checkout for builds, tests and license collection. A Go version bump belongs
in Instigator. The product picks it up through its submodule pin. macOS CI
also reads `instigator/go.mod` through `setup-go`.

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

When libslirp's wrap or patch files change, the product build runs
`meson subprojects update --reset libslirp` through QEMU's build environment
before compilation. Unchanged builds reuse the extracted dependency.

Ordinary builds accept local edits and produce a runnable directory.
The install script builds CMake's `binaries` target before release staging,
including when CPack uses Ninja. Release packaging requires clean
source and dependencies at their committed pins.

## Build or test the frontend directly

QEMU owns the portable firmware parser library and the `qemu-sgi-firmware`
utility. Cargo links the prebuilt `libsgi-firmware-core.a` and the target's
standard zlib. It does not invoke CMake or compile the parser itself.

With Rust, a C compiler, GNU Make, Ninja, pkg-config, Python with venv support,
GLib development files and zlib development files installed, initialize the
submodules and build only the native library:

```sh
qemu_source="$PWD/qemu"
qemu_build="$PWD/out/firmware"
mkdir -p "$qemu_build"
(cd "$qemu_build" && "$qemu_source/configure" --disable-system --disable-user \
  --enable-tools --disable-docs --without-default-features)
make -C "$qemu_build" -j2 libsgi-firmware-core.a
export SGI_FIRMWARE_ARCHIVE="$qemu_build/libsgi-firmware-core.a"
export SGI_FIRMWARE_SOURCE_DIR="$qemu_source"
cargo build --locked
cargo test --locked
```

Run the Make target again after changing QEMU sources or headers. Native
build tools refresh the archive before Cargo checks it, and Cargo watches the
archive and sources before linking. The product CMake build invokes this same
target before compiling the frontend. Product CTest checks consume the built
archive.

The helper is independently selectable with
`make -C "$qemu_build" -j2 qemu-sgi-firmware`. Both targets use the same archive.
A full emulator build is unnecessary for frontend tests.

For MinGW, configure a separate QEMU build with
`--cross-prefix=x86_64-w64-mingw32-`, use its archive, and set
`SGI_FIRMWARE_RUST_TARGET=x86_64-pc-windows-gnu` and
`CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=x86_64-w64-mingw32-gcc`.
Then use `cargo build --locked --target x86_64-pc-windows-gnu` and
`cargo test --locked --target x86_64-pc-windows-gnu --no-run`.
Native tests use a separate native archive. The selected linker must find
its target zlib. Use Cargo's standard target configuration or `RUSTFLAGS`
to add a library search path when the toolchain requires one.

## macOS

Use an Apple Silicon Mac running macOS 15 or newer, with Xcode command-line
tools, Rust, Go with automatic toolchain selection, and Python 3.12 or newer.
Install the native libraries and build tools:

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
directory. Cross-build tests compile Windows Rust test executables with
`--no-run`. Go and Python helper checks run natively on Linux. These checks do
not replace launch, graphics or input checks on Windows 11.

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
reported identity. Local edits are marked dirty, and unavailable components
are reported on their own lines.

The frontend embeds its Git description through `vergen-gitcl`. QEMU uses its
native package version option with `sgi-origami` and its Git description.
Every build checks the QEMU checkout and refreshes that option when its
identity changes, including local edits. Instigator uses Go's native module
version and VCS build information from its own checkout.

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

## Firmware registry

[`resources/proms.toml`](../resources/proms.toml) records each downloadable PROM's
ID, role, version, object path, size and SHA-256. Machine profiles in
[`src/profiles.rs`](../src/profiles.rs) select registry IDs through `boot_prom`
and optional `io_prom` references. Adding an image to the registry leaves the
profile selections unchanged. Select a different version by updating its profile
reference. Object names preserve the original SGI filename and extension, with
the PROM version inserted before the extension, such as `ip27prom-6.156.img`.

## Offline firmware preparation

The frontend statically links QEMU's portable firmware library. Cargo consumes
the prebuilt archive directly, and CMake coordinates product builds. `src/firmware.rs` borrows original input bytes, copies
the successful prepared output into Rust ownership and clears C allocations
on every return path. QEMU's generated layout table owns flash geometry and
reserved regions. Rust does not rebuild those structures or hardware records.

Original firmware creation, complete prepared backend import and existing
machine reopen use distinct paths. Creation calls preparation once per input
image. Import checks geometry and copies bytes. Reopen checks every persistent
backend before launch and never writes missing flash, NVRAM or clock files.
QEMU owns default identity record construction, SPD placement and processor
register validation. The frontend stores user selections and passes supplied
raw records through unchanged.

The remaining Octane and Octane2 models read an exact 2 MiB ROM backend.
The frontend marks that drive read-only. Prepared import preserves its bytes,
but the model implements neither flash commands nor persistence of changes
to the PROM's data area. Origin 200, Origin 2000, Onyx2, Origin 300 and Fuel
retain writable flash backends. All imported backend copies remain independent files.

New NVRAM and clock files contain zero bytes only. NVRAM capacity and saved
clock record size come from generated catalogue resources. QEMU owns clock
serialization. The frontend
writes no clock fields or checksums. The `nvramN.raw.clock` filename follows
QEMU's existing clock backend interface.

Fuel's optional `[machine]` console selection names `l1` or `ioc3-a`. An omitted
selection uses L1 for fresh PROMs. The `console-set` configuration
command validates the selection under the machine edit lock. Runtime console
logging and background control use that same persisted selection.
