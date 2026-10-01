# Developing Origami

[Contributing](../.github/CONTRIBUTING.md) covers changes and bug reports.
The [user guide](usage.md) covers running downloaded archives.

## Build identity

`origami version` prints the compiled catalogue SHA-256 and the product, QEMU, and
Instigator revisions recorded in an extracted package. A source-tree build
reports itself as unpackaged and prints the pinned QEMU and Instigator revisions.

## Linux preview archive

Builds use rootless Podman. Initialize the pinned QEMU and Instigator submodules, then give the script a scratch directory with several gigabytes free:

```sh
git submodule update --init -- qemu instigator
SGI_CONTAINER_ENGINE=podman sh build/linux-dev.sh "$PWD/qemu" "$PWD/instigator" /path/to/scratch /path/to/archive-dir
```

The script builds the Rust CLI, Instigator, and QEMU with SDL, VNC, and user networking enabled. It checks that SDL and VNC are present.

The frontend pins both sources as Git submodules at exact commits. Their
relative URLs resolve to sibling repositories under the same owner on each
host. GitHub Actions resolves them against the GitHub product repository, so
its sibling QEMU and Instigator repositories must contain the pinned commits.

The build writes `origami-linux-x86_64-preview.tar.gz` and its SHA-256 hash to
`archive-dir`. Omit that final argument to write them to the scratch directory.
Keep the same scratch directory to reuse its pinned QEMU build and dependency
caches across product revisions. Extract the archive anywhere and run
`linux-dev/bin/origami`.

The archive contains `origami` and `instigator` under `bin/`, QEMU and
`qemu-img` under `libexec/sgi/`, shared libraries under `lib/sgi/`, and QEMU
keymaps under `share/sgi/qemu/keymaps/` for VNC keyboard input.
`SHA256SUMS` covers every packaged file. `share/sgi/source-revisions.txt`
records the source revisions and checkout state. The library manifest at
`share/sgi/debian-libraries.tsv` maps each library to its Debian notice.
`share/sgi/rust-dependencies.tsv` and `share/sgi/go-dependencies.tsv`
list the Rust crates and Go modules with their bundled license files.

The archive needs Linux x86-64 with glibc 2.39 or newer. SDL uses the host's
display and input services. The archive built at product `e4f283b` and QEMU
`2b1e1cc` passed CLI, QEMU and qemu-img startup checks on Ubuntu 24.04.
Its QMP catalogue matched the pinned source. Guest and display checks below
used earlier archives and have not been repeated with this QEMU pin.
A blank-disk install reached graphical login and desktop through packaged
SDL under Xvfb at 1280×1024 after the generated RAD4 helper ran at the first
installer handoff. XTest keyboard input logged in and typed into NEdit. XTest
pointer motion and clicks selected and opened its desktop icon. An earlier
installed disk opened an X terminal and displayed the root directory in the
file manager. Physical host input remains untested.

## Windows x86-64 preview archive

Use the initialized QEMU and Instigator submodules to build the Windows archive:

```sh
SGI_CONTAINER_ENGINE=podman sh build/windows-dev.sh "$PWD/qemu" "$PWD/instigator" /path/to/scratch /path/to/archive-dir
```

The script writes `origami-windows-x86_64-preview.zip` and its SHA-256 hash to
`archive-dir`. Omit that final argument to write them to the scratch directory.
Keep the scratch directory to reuse its QEMU and dependency caches across
product revisions. Extract the ZIP and run `windows-dev/bin/origami.exe`. It includes
QEMU with SDL and VNC, `qemu-img.exe`, Instigator, QEMU keymaps, the required
MinGW DLLs, license notices, source revisions, and per-file checksums. The CLI uses a
loopback TCP endpoint for Instigator's private install network on Windows.

An earlier archive passed every per-file checksum. Under Wine, its packaged
CLI and `qemu-img.exe` created a machine and disk. The eight-CPU Origin 2000
reached its firmware menu with clean diagnostics. That earlier ZIP booted an
installed IRIX 6.5.30 disk to the 1280×1024 RAD4 desktop in its SDL window.
XTest keyboard input logged in. A synthetic pointer click produced no visible
guest response, so SDL pointer input needs a native check. Another earlier
archive reached graphical login over VNC and accepted VNC pointer and keyboard input. Native Windows graphics and input remain untested.

## macOS arm64 preview build

A native Apple Silicon Mac can build the same CLI, QEMU SDL display and
keymaps, and Instigator private network into a relocatable archive. Install the
build tools and Homebrew libraries checked by `build/macos-dev.sh`, including
Python 3.11 or newer, then use clean submodule checkouts:

```sh
sh build/macos-dev.sh "$PWD/qemu" "$PWD/instigator" /path/to/scratch
```

The script stops if a non-system library cannot be bundled, a Homebrew license
notice is missing, or a library still resolves outside the archive. It writes
`origami-macos-arm64-preview.tar.gz`, a SHA-256 hash, and per-file checksums.
The archive passed native startup and relocation checks on macOS 15.8. Guest
graphics and input remain untested there.

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

## Licenses and dependency sources

The Origami CLI and Instigator use BSD-3-Clause. QEMU as a whole uses GPLv2.
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
