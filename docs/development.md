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
That commit also selects the QEMU and Instigator pins, and its `CHANGELOG.md`
needs an `## X.Y.Z` section, which becomes the release notes. Build from the tag,
run the tests, then package those binaries. Rebuild after adding or changing
a tag so the embedded version matches the archive version.
Cargo's `0.0.0` is a package placeholder. The Git tag owns the release version.
`cargo xtask dist` reads the description when it runs and names the archive
`origami-<version>-<platform>` and its top-level directory `origami-<version>`,
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

Plain `cargo test --workspace` needs no download. `cargo xtask dist` writes
`origami-<version>-<platform>.tar.gz` (`.zip` on Windows) and its `.sha256`
for the platform it runs on: `linux-x86_64`, `macos-arm64` or
`windows-x86_64`. The archive holds `origami-<version>/` with:

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

### Before a QEMU release exists, or for a QEMU change

Build an archive in a QEMU worktree (`python3 packaging/sgi-package.py`; see
`packaging/README.md` there) and pass it in:

```sh
cargo xtask dist --qemu-archive /path/to/sgi-origami-<version>-linux-x86_64.tar.gz
```

`--qemu-archive` works with `runtime`, `test` and `dist`, and replaces the
QEMU pin for that run.

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

Each platform builds natively with Cargo: Apple Silicon on macOS 15 or newer,
x86-64 Linux, and x86-64 Windows with the MSVC toolchain. Native guest graphics
and input remain unqualified on macOS and Windows. The Linux release needs
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

## Licenses and dependency sources

The Origami CLI, Instigator and original Origami additions to QEMU use
BSD-3-Clause. QEMU as a whole uses GPLv2. Upstream and adapted code retain
their existing licenses. The QEMU release's `qemu.LICENSE.origami.paths` lists original files
covered by the BSD grant, and `qemu.LICENSE.origami` contains its terms.
Archives install the QEMU release's `licenses/` (which include each C
library's `copyright` file), Instigator's licence and `THIRD_PARTY_NOTICES`
under `share/origami/licenses/`. The notices file is
maintained by hand: update it when a Rust crate or Go module is added or
changes license.

QEMU statically links a patched libslirp. Its pinned source and patch are in
the QEMU repository's `subprojects/` at the release tag named in
`runtime.lock`.

## Release archive workflow

`.github/workflows/build.yml` is a reusable workflow. On each of
ubuntu-24.04, macos-15 and windows-latest it checks out the product, runs
`cargo xtask test` and `cargo xtask dist`, and uploads
`origami-<platform>` containing the archive and its `.sha256`.
`.github/workflows/release.yml` calls it for `v*` tags and publishes a GitHub
release with the three archives, `SHA256SUMS` and the `CHANGELOG.md`
section for that version. Pull requests and `main` pushes on GitHub do not
build archives. Forgejo's required check is `cli.yml`: formatting, Clippy and
`cargo test` on Linux.

Each job also extracts its archive to a path with spaces and checks that
`origami version` reaches QEMU and Instigator. Boot checks are the functional
tests below. Run artifacts contain archives and SHA-256 files. Firmware and
guest media are not included.

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
