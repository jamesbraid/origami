# Origami

`origami` is an experimental SGI emulator built around a pinned QEMU branch.
It is a hobby project developed largely with AI assistance. Hardware behavior
and guest compatibility are incomplete. It keeps each managed machine's
firmware, disks, and writable state in one directory.

The first graphical configuration is an Origin 200 with RAD4 output through QEMU's SDL display. VNC is optional and needs a viewer supplied by the user.

The current Linux and Windows preview archives include the repaired Instigator
network. Both passed packaged checks against real IRIX 6.5.30, MIPSpro
7.4.4, and RAD4 1.3k media. Each archive also booted a disposable copy of an
installed Origin 200 disk to the 1280×1024 IRIX desktop through SDL. The
Windows run was under Wine. XTest keyboard input logged in on both. Native
Windows runtime and physical host input remain untested. These are
pre-release checks, not a promise of full guest compatibility.

The current Linux archive completed a blank-disk Origin 200 install with
IRIX 6.5.30, MIPSpro 7.4.4m, and RAD4 1.3k. It then booted the installed disk
to the SDL desktop. Keyboard login and mouse opening of the UnixRoot file
manager worked under Xvfb. Native host input remains to be checked.

VNC listens on `127.0.0.1:5900` by default. Set `--vnc-port PORT` on `run` or `show-command` to use another local TCP port from 5900 through 65535. The option requires `--display vnc` and also works with `run --background`.

## Current commands

```text
origami machines
origami create my-origin --preset origin200-1 --prom /path/to/ip27-prom.bin --memory-per-node 128
origami create my-o300 --preset origin300-2 --prom /path/to/ip35-prom.bin --spd-dimm2 /path/to/dimm2-spd.bin --spd-dimm3 /path/to/dimm3-spd.bin
origami drive-create my-origin 4096
origami drive-attach my-origin /path/to/irix-install.iso --type cdrom --target 4
origami drive-attach my-origin /path/to/tape.image --type tape --target 5
origami drive-detach my-origin cdrom4
origami network-forward-add my-origin ssh --protocol tcp --host-port 2222 --guest-port 22
origami validate my-origin
origami show-command my-origin
origami run my-origin
origami run my-origin --display vnc --vnc-port 5991
origami run my-origin --background
origami install-apply my-origin --addon rad4
origami install-finish my-origin
origami status my-origin
origami console my-origin
origami stop my-origin
```

`origami version` prints the compiled catalogue SHA-256 and the product, QEMU, and
Instigator revisions recorded in an extracted package. A source-tree build
reports itself as unpackaged and prints the pinned QEMU and Instigator revisions.

Stop a machine before creating, attaching, or detaching drives. `drive-detach`
removes the named attachment from `machine.toml` and leaves the image file in
place. A tape attachment uses an existing writable image. Stop the machine
before changing it.

`run --background` returns after QEMU opens its control connection. It keeps the
machine lock until QEMU exits. `status`, `console`, and `stop` operate on that
background run. `stop` asks QEMU to quit. The primary serial output is also
appended to `logs/serial.log`. A second launch of the same machine is refused.
The default foreground `run` still uses the terminal for serial input and
output and appends the raw primary serial output to `logs/serial.log`.
`status` reports its lock, but `console` and `stop` need a background run.

For an IRIX 6.5.30 network install, prepare the media manifest and private network:

```sh
origami install-init my-origin --media-root /path/to/media --mac 08:00:69:12:34:56
origami install-addon my-origin --name tablet --source /path/to/tablet.tardist --install PRODUCT.SUBSYSTEM
origami install-check my-origin
origami install-serve my-origin
```

`--media-root` names the directory containing `6.5.30/`, `6.5-base/`, and
`mipspro/`. `install-init` writes their twelve source paths to
`my-origin/install/media.toml`. Edit those paths if your images live elsewhere.

`install-addon` is optional. Stop the machine before changing add-ons.
Replace `PRODUCT.SUBSYSTEM` with the package selection named by the add-on.
It accepts a local SGI image, extracted tree,
`.tar`, `.tar.gz`, `.tgz`, `.tardist`, or `.tardist.gz`. The add-on remains
outside the emulator archive and is read from its configured path.
`install-check` uses the packaged Instigator to open every named source,
including MIPSpro and any add-on, and assemble the install tree without opening
network ports. It catches invalid media and missing collision winners before
`install-serve`. It also checks that each named script's selected products have
`.sw` or `.man` files in an enabled distribution. Local images stay in place.
Instigator extracts archive sources into `install/cache/`. A successful check
does not prove that IRIX will install from those sources.

In another terminal, start the machine in background mode and attach its
serial console:

```sh
origami run my-origin --display none --background
origami console my-origin
```

The install commands use the background run's serial and control connections.
A foreground `origami run` cannot be controlled by `install-apply` or
`install-finish`.

The generated Instigator server listens at `10.98.0.2` and offers
`10.98.0.65` to the configured MAC. On a new Origin 200 disk, enter the PROM
command monitor from the System Maintenance Menu and boot the disk formatter:

```text
setenv netaddr 10.98.0.65
boot -f bootp():/6.5.30/stand/fx.64
```

In `fx`, accept the default `dksc(0,1,0)` drive, enter extended mode, then use
`label`, `sync`, `..`, and `exit` to write its default partitions. This changes
the selected disk. Back at the System Maintenance Menu, choose **Install System
Software**, then **Remote Directory**. Use `10.98.0.65` for the client address,
`10.98.0.2` for the server, and `/6.5.30/dist` for the remote directory. The
installer copies its miniroot to the disk. On a blank disk, confirm creation of
the root filesystem, choose 4096-byte blocks, and supply a hostname, client
address, and `255.255.255.0` netmask.

At the guest's `Inst>` prompt, disconnect `origami console` and load the
packaged install script from another host terminal:

```sh
origami install-apply my-origin --addon rad4
```

The command streams the installer output and waits through package transfer,
the dependency check, and the final `Inst>` prompt. It leaves the guest running
if those steps do not complete. Inspect the output for package errors before
finishing the installation. Omit `--addon` for the base release and MIPSpro.
Use `--addon tablet` when that add-on is configured. Its script selects MIPSpro
and the add-on together. `--addon rad4` selects MIPSpro and RAD4. The generated
RAD4 helper below is required after package transfer to build the guest kernel
and enable graphical login. The same scripts can be loaded manually with
`admin source 10.98.0.2:/mipspro.cmds` or
`admin source 10.98.0.2:/addon-NAME.cmds`.

For the RAD4 1.3k add-on, `install-check` also generates
`my-origin/install/generated/rad4/dist/finish-rad4.sh`. It builds the installed
kernel with the RAD4 driver, disables DMA, selects the working input devices,
and enables graphical login. When package installation returns to `Inst>`, keep
`install-serve` running and use `origami install-finish my-origin` from another host
terminal. The command fetches the helper over TFTP, checks its length and guest
checksum, checks shell syntax and exit status, then restarts the guest and waits
for serial login. The machine stays running if a check fails so the guest can
be inspected.

After the guest restarts, stop the machine before using
`origami network-set my-origin --mode user` to return it to QEMU user networking.
`--mode none` disconnects it.
The same sequence can be entered manually:

```text
Inst> shroot
IRIS# ifconfig ef0 10.98.0.65 netmask 255.255.255.0 up
IRIS# tftp 10.98.0.2
tftp> binary
tftp> get /addon-rad4/dist/finish-rad4.sh /tmp/finish-rad4.sh
tftp> quit
IRIS# cksum /tmp/finish-rad4.sh
IRIS# /bin/sh /tmp/finish-rad4.sh /
IRIS# exit
Inst> quit
Restart? y
```

The current script's `cksum` is `2973792095 2550`. Stop if the guest reports
a different value. This TFTP fetch, first kernel build, and repeat script run
passed at the first miniroot handoff on a blank-disk install. The RAD4 package's
own miniroot exit command reports a missing `/usr/sbin/lboot`. The helper
builds the kernel from the installed target. The PROM boot, disk preparation,
and first-run questions still use the interactive guest installer. Packaged
`install-finish` passed at the first `Inst>` handoff on a blank-disk installation
and reached serial login with RAD4 attached. A candidate CLI completed
`install-apply` on a blank-disk guest. The current clean archive has completed
`install-finish` and SDL desktop login on that guest, but has not repeated the
package stage from a blank disk.

A forward binds only to the host loopback address. The example sends host TCP
port 2222 to guest port 22 while user networking is active. Use
`origami network-forward-remove my-origin ssh` to remove it. Forwards stay in
`machine.toml` while private installation networking or no networking is
selected, and become active again with `network-set --mode user`. Stop the
machine before changing its network or forwards.

`origami machines` lists five managed presets. The Origin 200 one-processor preset enables RAD4 graphics. Origin 2000 uses four independent node PROM images under the machine's `state/` directory.
`create --memory-per-node MiB` selects one of the values accepted by the chosen preset. Without it, `create` uses that preset's default. `origami show` prints the selected amount.

The package also contains the full QEMU implementation, including Origin 200,
Origin 2000, Origin 300 and Onyx2 machines and the implemented RAD4, SI and
InfiniteReality graphics devices. Many configurations have no managed
`origami create` preset. To inspect the QEMU machine and device options in an
extracted archive, run `libexec/sgi/qemu-system-mips64 -machine help` and
`libexec/sgi/qemu-system-mips64 -device help` (use `.exe` on Windows). Invoke
that bundled QEMU binary directly for configurations outside the five presets.
The [QEMU machine documentation](qemu/docs/specs/sgi-sn.rst) describes its
topologies and board options. These options are experimental and do not imply
that firmware, installation or graphics work for every combination.

Origin 300 creation takes the two 128-byte SPD records for the reviewed 512 MiB
kit as separate inputs. The command checks their SHA-256 values, copies them
beside the user-supplied PROM, and creates a persistent 16 MiB boot flash on
first run. It also creates synthetic IO8 identity records using the configured
MAC address (`--mac`, default `08:00:69:12:34:56`). Use that same address for
`install-init --mac` or a private network attachment. The SPD records and PROM
are user supplied. Their redistribution rights have not been settled. Origin
300 reached the firmware System Maintenance Menu from an earlier Linux package
at the same QEMU revision.
An earlier Windows package reached it under Wine. IRIX boot on Origin 300 still
needs package-level qualification.

The firmware and IRIX media are supplied by the user. They are copied or
referenced only in the machine directory and never enter this repository. IP27
accepts a nonempty PROM payload up to 1 MiB. Origin 300 requires its exact
reviewed IP35 PROM size and hash.

The CLI starts an existing disk or CD-ROM image and prepares the Instigator server. The PROM, disk formatter, and first-run questions remain interactive. `install-apply` runs the generated `inst` package-selection script, and `install-finish` automates the RAD4 installer handoff.

The standard media manifest keeps MIPSpro and development packages in the install source.

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
display and input services. An earlier archive at the same QEMU revision passed
a runtime check in Ubuntu 24.04 with glibc 2.39 and SDL's dummy video driver.
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

The extracted archive passed every per-file checksum. Under Wine, the packaged
CLI and `qemu-img.exe` created a machine and disk. The eight-CPU Origin 2000
reached its firmware menu with clean diagnostics. The current ZIP booted an
installed IRIX 6.5.30 disk to the 1280×1024 RAD4 desktop in its SDL window.
XTest keyboard input logged in. A synthetic pointer click produced no visible
guest response, so SDL pointer input needs a native check. An earlier archive
at the same QEMU revision reached graphical login over VNC and accepted VNC
pointer and keyboard input. Native Windows graphics and input remain untested.

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
This build path has not run on macOS yet, so no macOS archive or host support is
claimed.

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

The `Preview archives` GitHub Actions workflow builds Linux, Windows and
macOS artifacts on pull requests, mirrored `main` commits and version tags.
It uploads archives and SHA-256 files as run artifacts. It does not upload
firmware or guest media. The workflow has not run against this candidate.

## License

The original frontend and build tooling are licensed under
[BSD-3-Clause](LICENSE). QEMU, Instigator, and other bundled dependencies retain
their own licenses. Packaged notices are in `share/sgi/licenses/`. Guest
firmware, operating systems, and third-party drivers are not covered by the
frontend license.

## Contributing

Open issues and pull requests on GitHub. The [contribution guide](.github/CONTRIBUTING.md)
explains what to include in reports and patches and where to submit dependency
changes.
