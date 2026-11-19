# Developing Origami

[Contributing](../.github/CONTRIBUTING.md) covers changes and bug reports.
The [user guide](../docs/usage.md) covers running downloaded archives.

## Build identity

`origami --version` (also `origami version`) prints three lines: Origami,
QEMU and Instigator. Each executable carries its own build identity.
Origami uses Git tags: `v0.2.0` at a release, `v0.2.0-5-gabc1234` after it,
and a `-dirty` suffix for tracked local edits. Without a version tag it uses
the commit ID. Builds without Git metadata report `unknown`.

Create an annotated `vX.Y.Z` tag on the product commit to select a release.
That commit also selects the QEMU and Instigator pins, and its `CHANGELOG.md`
needs an `## X.Y.Z - YYYY-MM-DD` section, which becomes the release notes.
Rename `## Unreleased` and open a fresh one above it. The section opens with
a one-sentence summary, then Breaking, Features, Changed and Fixes, leaving
out empty ones. Build from the tag,
run the tests, then package those binaries. Rebuild after adding or changing
a tag so the embedded version matches the archive version.
Cargo's `0.0.0` is a package placeholder. The Git tag owns the release version.
`cargo xtask dist` reads the description when it runs and names the archive
`origami-cli-<version>-<platform>` and its top-level directory
`origami-cli-<version>`,
where `<version>` is the description without the `v` (`0.4.1`, or
`0.4.0-5-gabc1234` between releases, with `-dirty` after tracked edits).

## Build the product

The product is a Cargo workspace. It builds neither QEMU nor Instigator: it
pins their published releases in `runtime.lock` (a QEMU release from
[qemu-sgi-origami](https://github.com/jamesbraid/qemu-sgi-origami/releases)
and an Instigator release from
[instigator](https://github.com/jamesbraid/instigator/releases)), and a
small task runner, `xtask`, downloads, verifies and lays them out. Install a
Rust toolchain (1.95 or newer) and nothing else.

```sh
cargo xtask runtime   # download and verify the pinned releases; print the tree
cargo xtask test      # cargo test, then the runtime contract tests on that runtime
cargo xtask dist      # release build plus archive under target/dist/
cargo xtask bump qemu sgi-origami-11.1.50-2    # move a pin
cargo xtask bump instigator v0.5.1
```

Plain `cargo test` needs no download. `origami-gui` is not a default member (it builds Slint and wgpu); select it with `-p origami-gui`. `cargo xtask dist` writes
`origami-cli-<version>-<platform>.tar.gz` (`.zip` on Windows)
for the platform it runs on: `linux-x86_64`, `linux-arm64`, `macos-arm64`,
`macos-x86_64`, `windows-x86_64` or `windows-arm64`. The archive holds
`origami-cli-<version>/` with:

- `bin/origami` and `bin/instigator`;
- `libexec/origami/` with `qemu-system-mips64`, `qemu-img`,
  `qemu-sgi-machine-init` and, on Windows, QEMU's DLLs;
- `share/origami/qemu/keymaps/`;
- `share/origami/licenses/`, which gathers the licences from both archives
  and from this repository.

A QEMU release archive has a fixed layout: `bin/{qemu-system-mips64,
qemu-img,qemu-sgi-machine-init}` (plus DLLs on Windows), `share/qemu/keymaps/`
(`share/keymaps/` in releases before 11.1.90) and `licenses/`. `xtask` moves
them to the paths above. It ships no firmware and no shared libraries: the
host provides its display, audio and system libraries, as the QEMU
repository's packaging notes describe.

### iris

`cargo build --release --features iris` links the iris emulator into the `origami` executable, for the machines the `indy` preset makes. iris is a git dependency of `crates/origami-iris` at an exact commit of `jamesbraid/iris` (the `origami/zero-copy` branch, which carries patches we do not send upstream), and the two `[patch.crates-io]` entries in the workspace `Cargo.toml` repeat iris's own for `winit` and `block`. Move the pin in its own change. Only the runner process (`origami _serve`) links iris; the crate's engine half, which front ends use to start a machine, does not. The `iris` test runs the engine contract suite and the core against a real iris machine, and takes a picture from its display socket: `cargo test --features iris --test iris`. `cargo test -p origami-iris` runs the display conformance suite against iris's push host with synthetic boards. `cargo run --release -p origami-iris --example v4_shot -- SOCKET OUT.ppm` is a bare viewer for a running machine.

### Before a QEMU release exists, or for a QEMU change

Build an archive in a QEMU worktree (`python3 packaging/sgi-package.py`; see
`packaging/README.md` there) and pass it in:

```sh
cargo xtask dist --qemu-archive /path/to/sgi-origami-<version>-linux-x86_64.tar.gz
```

`--qemu-archive` works with `runtime`, `test` and `dist`, and replaces the
QEMU pin for that run.

### App packages

`cargo xtask dist --app` adds the native app to the same tree and writes
`origami-<version>-<platform>.tar.gz` (`.zip` on Windows and macOS). It needs the app in the checkout: `crates/origami-gtk` on Linux
(GTK 4.14 and libadwaita 1.6 come from the host), `apps/macos` on macOS (the
archive holds `Origami.app` with the tools beside its executable, ad hoc
signed unless [signing](#code-signing) is on) and `apps/windows` on Windows.
The Windows app is `Origami.exe`,
which cannot share a directory with `origami.exe`, so it lives in `app/` and
finds the tools in the sibling `bin`. Release packages carry no Slint.
`cargo test -p xtask -- --ignored` checks the notices; it fetches every
dependency, including git ones, so the default test run skips it.

The Windows runner has no compiler. Build the Rust parts elsewhere with
`cargo xtask dist --app --platform windows-x86_64 --target x86_64-pc-windows-gnu
--prebuild DIR` (needs `uniffi-bindgen-cs`, pinned in
`crates/origami-ffi/bindings/csharp/gen.sh`), then run the cross-built
`DIR/xtask.exe dist --app --prebuilt DIR` on Windows, where `dotnet publish`
builds the app. `.github/workflows/package-windows.yml` does both.

Windows arm64 builds natively on a Windows on Arm host, so there is no
prebuild step: `cargo xtask dist --app` does it all. No Instigator release has
a Windows arm64 archive, so `runtime.lock` pins that platform to a source
commit instead, and `cargo xtask runtime` builds it with Go (`git` and `go`
must be on PATH).

### Instigator's install configurations

The install recipes embed the five YAML files in `resources/instigator/configs`
and the NFS and frame tests replay transcripts under
`crates/*/tests/data`, so `cargo test` needs nothing from Instigator's
repository. They are copies from the pinned Instigator release.
`cargo xtask runtime` fails if the configs differ from the ones in the pinned
archive, and `cargo xtask bump instigator TAG` refreshes them. Refresh the
transcripts by hand when Instigator changes them.

## Build identity of the pieces

`origami --version` queries QEMU and Instigator at their launch paths,
including `ORIGAMI_RUNTIME_DIR` for QEMU. QEMU reports
`(sgi-origami <release>)`, taken from the release tag; Instigator reports its
release tag. The frontend embeds its Git description through `vergen-gitcl`.

## macOS, Linux and Windows

Each platform builds natively with Cargo: Apple Silicon or Intel on macOS 15
or newer, x86-64 or arm64 Linux, and x86-64 or arm64 Windows with the MSVC
toolchain.
Native guest graphics and input remain unqualified on macOS and Windows. The Linux release needs
the host's PulseAudio and ALSA client libraries, X11 and udev; the QEMU
release is built on an older glibc so it runs on current distributions. SDL
uses the host's display and input services.

## Machine list and state

The CLI reads its machine offerings from the QEMU it launches. It starts that
QEMU with no machine and asks for `query-sgi-machines` over QMP each time a
command needs an offering. A QEMU pin change that alters the machine list therefore
changes the offerings without a frontend edit, and a frontend change that
needs new machine list data needs the matching QEMU pin.

The CLI reads machine list version 3 only and refuses a QEMU that reports
another. The Rust tests read `tests/fixtures/sgi-machines.json`, an excerpt of
the pinned QEMU's own reply: the offerings the tests use, with the record
fields the CLI reads. Regenerate it whenever the pin moves:

```sh
tests/fixtures/generate-machines.py "$(cargo xtask runtime)/libexec/origami/qemu-system-mips64" \
  > tests/fixtures/sgi-machines.json
```

Anything that needs the full machine list belongs in the runtime contract
tests below, which query the real QEMU.

`create` runs `qemu-sgi-machine-init` from beside QEMU to build a machine's
storage, and launches attach each storage item in the machine list as a block node of
the same name. `cargo test` runs the component tests in `tests/cli` against
`origami-fake` (`tests/bin/fake.rs`), a test-only binary that acts as QEMU,
qemu-img, the init tool and Instigator under their names, so it needs neither
a QEMU download. `ORIGAMI_FAKE` makes it misbehave: see the
header of `tests/bin/fake.rs`. `cargo build` builds it too; the release archive
does not carry it.

The ignored tests in `tests/runtime` are contract tests. They check that every
starter preset names an offering of the real machine list, then create, reopen
and start each paused (`-S`) with the real tools, start each network mode that
needs no host device, check that the fake's machine list is cut from the real
one, and upgrade and start older machines. To run them against an existing
runtime directory holding `qemu-system-mips64`, `qemu-img` and
`qemu-sgi-machine-init`:

```sh
ORIGAMI_RUNTIME_DIR="$(cargo xtask runtime)/libexec/origami" \
  cargo test --test runtime -- --include-ignored
```

## Integration and acceptance tests

Two test targets boot real IRIX guests with a built product, and are ignored
by a plain `cargo test`. `tests/integration` runs guests on networks with real
peers: on Linux a bridge with a tap device per guest and dnsmasq and unfs3 in a
network namespace, on macOS socket_vmnet with dnsmasq, on Windows user-mode
networking and a TAP-Windows6 probe. `tests/acceptance` installs IRIX from the
public media on an Origin 200. Both need `ORIGAMI_PRODUCT` (an extracted
archive) and `ORIGAMI_WORK`; [the integration README](../tests/integration/README.md)
has the variables, the tests and the Linux lab. GitHub runs them from
`integration-network.yml` daily and `acceptance-install.yml` weekly, each only
when `main` has moved:

```sh
ORIGAMI_PRODUCT=/path/to/extracted/origami-<version> ORIGAMI_WORK=/scratch/it \
  cargo test --test integration -- --ignored --test-threads=1 lab_probe lan_tap
```

## Display protocol version 4

`origami-display` holds the version 4 viewer library (`v4`),
`origami-display-producer` the producer library and a reference producer, and
`display-conformance` the suite any producer must pass. `cargo test` runs the
suite against the reference producer in both host styles (it pulls lines, or
its host pushes pictures). To run it against another producer started with the
test extension on, for QEMU `-device display-shm,path=SOCK,x-test=on,heads=16`:

```sh
cargo run --release -p display-conformance -- --socket SOCK
cargo bench -p display-conformance --bench viewer_frame
```

Restart the producer for each run: head numbers are never reused, so a run
leaves fewer heads for the next. The queue-overflow check needs the producer to
read about 260,000 reports; `CONF_OVERFLOW_SECS` raises its 45 second budget,
and a skipped check fails the run unless `--allow-skips` is given.

The frame plane speaks version 4 to a QEMU that has the `display-shm` device
and version 3 to an older one: `qom-list-types` on a machine-less QEMU decides,
once per QEMU binary. For version 4 the runner listens on `state/display.sock`
and passes the socket down (`-device display-shm,fd=N`), so a viewer can connect
before QEMU is ready and the runner removes the socket when the machine ends.
The viewer's GPU imports the shared-memory slots where it can and lends each
back when it has read it: Vulkan host memory on Linux (asked for as `RW_PIXELS`,
because the driver pins host memory for writing) and Metal no-copy on macOS
(not yet run). Otherwise the changed lines are copied. Windows stays on version
3 until the version 4 session library and QEMU's producer support it.
`crates/origami-ffi/tests/qemu_boot.rs` boots an Octane2 and an IRIX desktop
through the C frame plane on either; its header lists the variables.

With the `iris` feature the ffi crate also hosts iris machines: the runner
(an `origami` built with `iris`) serves protocol 4, and the frame plane attaches
over the socket like a viewer, so a machine's pictures and input reach
`origami_view_present` as QEMU's do. iris's producer publishes only on a real
change; the login caret's blink still wakes a viewer about twice a second. iris
composes the whole board whenever a viewer waits, which costs iris's own thread
and is not measured here.

`crates/origami-ffi/tests/iris_boot.rs` boots an Indy from the published IRIX 5.3
disk (`irix-images/find --machine indy --release 5.3`). It is ignored by default
and needs a Vulkan device, an X server (Xvfb is enough), ImageMagick's `import`
and these variables: `ORIGAMI_BIN`, an `origami` built with `iris`;
`ORIGAMI_GUI_TEST_DIR`, a directory for the machine; `ORIGAMI_TEST_DISK`, the
disk, which is copied so its writes stay in a diff beside the copy; and
optionally `ORIGAMI_TEST_DISK_SHA256`. Run it with `cargo test -p origami-ffi
--release --features iris --test iris_boot -- --ignored --nocapture`; the
numbers it prints start with `MEASURE`.

`crates/origami-ffi/tests/cube_view.rs` draws the cube in an X11 window through
the same functions with a fake machine: it reads back each scene in both
appearances, drives the pointer, steps scenes, reflects the fake machine's
screen, and measures CPU while turning, flung, at rest and hidden. It is
ignored by default and needs a Vulkan device and an X server. Run it with
`cargo test -p origami-ffi --release --features fake --test cube_view --
--ignored --nocapture`; `ORIGAMI_CUBE_SHOTS` names a directory for PNG
screenshots and `ORIGAMI_CUBE_SECS` the length of the turning measurement. The
GPU time of the cube pass is `cargo test -p origami-render --release gpu_time --
--ignored --nocapture`. Both print lines starting with `MEASURE`.

## Licenses and dependency sources

The Origami CLI, Instigator and original Origami additions to QEMU use
BSD-3-Clause. QEMU as a whole uses GPLv2. Upstream and adapted code retain
their existing licenses. The QEMU release's `qemu.LICENSE.origami.paths` lists original files
covered by the BSD grant, and `qemu.LICENSE.origami` contains its terms.
Archives install the QEMU release's `licenses/` (which include each C
library's `copyright` file), Instigator's licence, `THIRD_PARTY_NOTICES` and
`QEMU_SOURCE.txt` (the exact source tag of the bundled QEMU and its link,
from `runtime.lock`) under `share/origami/licenses/`. The notices file is
maintained by hand: update it when a Rust crate or Go module is added or
changes license.

The apps' Acknowledgements and Licences sheets come from one document,
`origami_credits_json`, built by `crates/origami-credits` from
`resources/credits/credits.toml` (the people and projects), `scenes.json`,
`runtime.lock` and the notices. An app build reads the notices that
`cargo xtask dist --app` generates for that package (`ORIGAMI_NOTICES`), so the
Licences sheet lists the crates the package links; a plain `cargo build` reads
the repository's file. A new bundled component needs a group in
`THIRD_PARTY_NOTICES`, or an entry in `credits.toml` if it has no group there.

QEMU statically links a patched libslirp. Its pinned source and patch are in
the QEMU repository's `subprojects/` at the release tag named in
`runtime.lock`.

## Release archive workflow

`.github/workflows/build.yml` is a reusable workflow. On each of
ubuntu-24.04, ubuntu-24.04-arm, macos-15, macos-15-intel, windows-latest and windows-11-arm it checks out the product, runs
`cargo xtask test` and `cargo xtask dist`, and uploads
`origami-<platform>` containing the archive. It also runs
`cargo xtask dist --app` and uploads `origami-app-<platform>`: the GTK tarball
on Linux, the `.app` zip on macOS (ad hoc signed, or Developer ID signed and notarized), and the WinUI app zip on
Windows, whose Rust parts are cross-built with MinGW on Linux first.
A last job joins the arm64 and x86_64 macOS packages with
`cargo xtask universal` (every Mach-O through `lipo`, then one ad hoc
signature) into `origami-macos-universal`, which is what a release publishes
for macOS; the thin ones stay artifacts. A release has `SHA256SUMS` only, no
per-file checksums. The signing step is `universal::sign`, between the join
and the zip.
`.github/workflows/release.yml` calls it for `v*` tags and publishes a GitHub
release with the ten archives and `SHA256SUMS`, and the `CHANGELOG.md`
section for that version. Pull requests and `main` pushes on GitHub do not
build archives. Forgejo's required check is `cli.yml`: formatting, Clippy and
`cargo test` on Linux.

Each job also extracts its archive to a path with spaces and checks that
`origami version` reaches QEMU and Instigator. Boot checks are the functional
tests below. Run artifacts contain archives and SHA-256 files. Firmware and
guest media are not included.

### Code signing

`v*` tag builds sign and notarize the macOS archives when signing is
configured. The `macos-universal` job runs in the `release` environment, which
holds the signing secrets and allows only `v*` tags. Other runs (manual runs on
a branch, forks, the nightly checks) never enter it, build unsigned and say so
in a notice. A tag build fails if signing is only partly configured or any
signing step fails, and so does a tag build with nothing configured, which
would publish unsigned macOS archives.
The scripts are in `build/sign/`. They unpack an archive, sign it and pack it
again before `SHA256SUMS` is computed at publish. macOS is signed once, in the
`macos-universal` job after `cargo xtask universal` joins the two builds, so the
thin per-architecture packages stay ad hoc signed.

Environment secrets: `MACOS_CERT_P12_BASE64` (Developer ID Application
certificate and key), `MACOS_CERT_PASSWORD`, `APPLE_TEAM_ID`, `ASC_KEY_ID`,
`ASC_ISSUER_ID` and `ASC_KEY_P8_BASE64` (App Store Connect key for
`notarytool`). Every Mach-O is signed inside out with the hardened runtime
and a timestamp; only `qemu-system-*` gets the `com.apple.security.cs.allow-jit`
entitlement, which QEMU's TCG needs for its `MAP_JIT` code buffer (split-wx is
off by default in a release build, so the `MAP_JIT` path is the one taken; a
`debug_tcg` build or `-accel tcg,split-wx=on` would also need
`allow-unsigned-executable-memory`). The app is notarized and stapled; the
command-line archive is notarized but cannot be stapled. After signing, the job
starts the signed `qemu-system-mips64` once (`build/sign/macos-smoke.sh`).

## App screenshots

`tools/screenshots` regenerates `docs/images/app/`, the pictures in the
[app guide](../docs/app.md) and the changelog. Run the `screenshots` workflow on GitHub
(Actions, Run workflow), then `tools/screenshots pull` to collect its Linux,
CLI, macOS and Windows sets; `tools/screenshots --help` lists the commands that
make one platform's set locally. The Linux set is the `docs` scenario of
`crates/origami-gtk/ci/driver.py`, run on lavapipe like the workflow, so About
shows the plain dialog. A new picture goes in the table in `tools/screenshots`
and in the page that shows it.

## Functional tests

A functional test is a full-machine boot. `tests/functional` boots a published
IRIX 6.5.30 disk on each system in `tests/data/systems.toml` and checks login,
`uname -m`, `hinv`, a network address, a ping, NFS reads and writes, a CD-ROM
read and a clean halt. A trial is an (engine, system) pair named
`<engine>::<family>::<system>`; QEMU is the only engine with a driver, and a
row that names another engine is listed as ignored. The `functional` workflow
runs them nightly on Linux, macOS and Windows against the built archive.
Adding a system is a row in `systems.toml`.

They test an extracted product archive, so without `ORIGAMI_PRODUCT` every
trial is listed as ignored. Locally, with a product tree that has `bin/origami`
and `libexec/origami/`:

```sh
ORIGAMI_PRODUCT=/path/to/origami-<version> \
ORIGAMI_FUNCTIONAL_DISKS=/path/to/disk-cache \
  cargo test --test functional -- qemu::ip27::
```

The disk is 1.6 GB: it is downloaded once into `ORIGAMI_FUNCTIONAL_DISKS` and
checked against the hash in `systems.toml`. `ORIGAMI_FUNCTIONAL_WORK` holds the
machines and the PROM cache and `ORIGAMI_FUNCTIONAL_OUT` receives each trial's
`results.json` and console log. Trials run one at a time. A system whose disk
is not published, or that names an engine without a driver, is listed with the
reason and never runs.
