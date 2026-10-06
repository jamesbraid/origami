# Using Origami

[Back to the README](../README.md). Examples use an extracted preview archive.

## Downloaded previews

Download the archive for your host and its `.sha256` file from the
[release page](https://github.com/jamesbraid/origami/releases). Check the hash
before extracting. Keep the extracted directory together: the CLI needs the
bundled executables, libraries and data beside it.

On Linux:

```sh
sha256sum -c origami-linux-x86_64-preview.tar.gz.sha256
tar -xzf origami-linux-x86_64-preview.tar.gz
./linux-dev/bin/origami machines
```

On Windows, compare the displayed hash with the value in the `.sha256` file,
then extract and start the CLI from PowerShell:

```powershell
Get-FileHash -Algorithm SHA256 .\origami-windows-x86_64-preview.zip
Expand-Archive .\origami-windows-x86_64-preview.zip -DestinationPath .\origami
.\origami\windows-dev\bin\origami.exe machines
```

On Apple Silicon macOS:

```sh
shasum -a 256 -c origami-macos-arm64-preview.tar.gz.sha256
tar -xzf origami-macos-arm64-preview.tar.gz
./macos-arm64-dev/bin/origami machines
```

The archives are unsigned. Native Windows and macOS download warnings remain
unverified. There is no installer or automatic updater. To try a newer version,
extract its archive into a separate directory. Keep your machine directories,
firmware and disks outside the extracted package.

Use the full CLI path above in place of `origami` in the examples below, or
add the package's `bin` directory to your PATH. The first `create` example
downloads and verifies its preset's PROM. Use `--prom FILE` to supply a local
PROM instead. Presets with a BASEIO or GIGAchannel board also download the IO
PROM; `--io-prom FILE` supplies a local one.
To use an existing guest disk after `create`, attach a copy as the system disk:

```sh
origami drive-attach my-origin /path/to/copied-guest-disk.qcow2 --type disk --target 1
origami run my-origin
```

Use a disk prepared for that emulated machine. The guest can change it while
running. For a new disk, use `drive-create` and the installation instructions
below instead.

## Hosts and current limitations

The launch targets are Linux x86-64 with glibc 2.39 or newer, Windows 11
x86-64, and macOS 15 or newer on Apple Silicon.

Native Windows execution and the current macOS archive remain unverified.
Linux graphical checks used Xvfb, so physical keyboard and pointer input
remain untested there too.

## Current commands

Run `origami --help` to list commands and `origami COMMAND --help` for
arguments and examples, such as `origami create --help` or `origami run --help`.
Unknown options and invalid argument values are rejected before the command runs.

```text
origami machines
origami create my-origin --preset origin200-1 --memory-per-node 128
origami create my-o300 --preset origin300-2
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

`origami version` reports the bundled component revisions. Include its output
when reporting a problem.

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
origami install-init my-origin --mac 08:00:69:12:34:56 --profile desktop
origami install-addon my-origin --name tablet --source /path/to/tablet.tardist --install PRODUCT.SUBSYSTEM
origami install-check my-origin
origami install-serve my-origin
```

This configures the public IRIX 6.5.30 desktop media. Use `--profile base`
for a smaller installation or `--profile development` to include MIPSpro.
For local media, add `--media-root /path/to/media`, naming the directory
containing `6.5.30/`, `6.5-base/`, and `mipspro/` for the development profile.
`install-init` writes sources to `my-origin/install/media.toml`.

`install-addon` is optional. Stop the machine before changing add-ons.
Replace `PRODUCT.SUBSYSTEM` with the package selection named by the add-on.
A source can be a local SGI image, extracted tree, `.tar`, `.tar.gz`, `.tgz`,
`.tardist`, or `.tardist.gz`, or a public HTTPS URL. HTTPS sources use
Instigator's existing remote-media support. The add-on remains outside the
emulator archive and is configured in the machine's media manifest.

To add the optional RAD4 driver from its public archive, select package `rad4x`
from distribution `dist_6.5`:

```sh
origami install-addon my-origin \
  --name rad4 \
  --source https://origami-dist.irix.fans/irix/addons/rad4/rad4x_65_13k.tar.gz \
  --install rad4x \
  --dist dist_6.5
```

`install-check` uses the packaged Instigator to open every named source,
including selected development inputs and any add-on, and assemble the install tree without opening
network ports. It catches invalid media and missing collision winners before
`install-serve`. It also checks that each named script's selected products have
`.sw` or `.man` files in an enabled distribution. Local images stay in place.
Instigator extracts archive sources into `install/cache/`. A successful check
does not prove that IRIX will install from those sources.

`install-serve` prints the path to its log and records each launch in a fresh
`install/instigator-*/` directory inside the machine. `server.log` keeps
Instigator's stdout and stderr. Follow it with `tail -f PATH/server.log`.
Instigator's native capture records timestamped events, request timings and
build/media details in `events.jsonl` and `run.json`. A clean shutdown also
writes `summary.json`. If shutdown leaves that summary missing, reconstruct
it from the retained events with the packaged server:

```sh
/path/to/package/bin/instigator trace summary my-origin/install/instigator-TIMESTAMP
```

An incomplete capture covers only the events recorded before the server
stopped. Keep the same machine settings, package selection and media sources
when comparing installation timings.

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
finishing the installation. Omit `--addon` to install the selected profile
alone. Use `--addon tablet` or `--addon rad4` when that add-on is configured.
The script combines the selected profile with the add-on. MIPSpro is included
only with a development profile. The generated RAD4 helper below is required after package transfer to build the guest kernel
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
a different value. The RAD4 package's miniroot exit command reports a missing
`/usr/sbin/lboot`. The helper builds the kernel from the installed target.
The PROM boot, disk preparation and first-run questions remain interactive.

A forward binds only to the host loopback address. The example sends host TCP
port 2222 to guest port 22 while user networking is active. Use
`origami network-forward-remove my-origin ssh` to remove it. Forwards stay in
`machine.toml` while private installation networking or no networking is
selected, and become active again with `network-set --mode user`. Stop the
machine before changing its network or forwards.

`origami machines` lists 12 starter presets for Origin 200, Origin 2000,
Onyx2, Origin 300, Octane, Octane2 and Fuel. The IMPACT shortcuts select SI.
Onyx2 selects InfiniteReality. Origin 300 has direct V12 and V-brick choices.
Fuel currently has a serial console. Octane VPro and Fuel VPro are unavailable.

The console uses the serial line QEMU's catalogue marks as the machine's
console: the L1 on Origin 300 and Fuel, and IOC3 port A elsewhere. A Fuel
whose PROM environment is set to `console=d` talks on IOC3 port A instead.
Choose another line while the machine is stopped, or omit `--port` to return
to the default. Lines take their catalogue names, such as `l1`, `ioc3_a` and
`ioc3_b`; an unknown name lists the machine's choices.

```sh
origami console-set my-fuel --port ioc3_a
```

`create --memory-per-node MiB` selects one of the values accepted by the preset.
The full QEMU catalogue also validates additional topologies and processor
populations configured in `machine.toml`. Use `topology` and `population` to
identify those configurations. Support remains experimental: a selectable
configuration does not imply that firmware, installation or graphics work.
The [QEMU machine documentation](https://github.com/jamesbraid/qemu/blob/sgi-origami/docs/specs/sgi-sn.rst)
describes topology and board options. QEMU's catalogue gives each machine its
board and processor values, such as Fuel's `board-id-word` or Octane2's
`r12000-prid`. `create --set PROPERTY=VALUE` overrides one; QEMU checks the
value. An unknown property lists the ones the preset accepts.

A machine has one MAC, stored under `[identity]` in `machine.toml` and passed
to QEMU as the machine's `mac` property. QEMU reports it in the machine's
identity records and gives it to the onboard Ethernet. `create`,
`network-set` and `install-init` accept `--mac` to set it when the machine has
none; a machine keeps the MAC it has. Private networking and installation
need it. Without one, QEMU uses its own default address.

PROMs and media stay outside the repository. Local IP27 and IP35 PROM files
may be SGI's original images or raw PROM payloads; Octane takes a raw image.
QEMU checks them when it creates the machine.

## Machine state

A machine keeps its writable state under `state/`: one file per node boot
flash, IO PROM flash, Timekeeper NVRAM and clock record, named as QEMU's
machine catalogue names them. `create` runs QEMU's `qemu-sgi-machine-init` to
build these files from the PROM images, and keeps no copy of the images.
Later runs need neither the PROM files nor a download. PROM updates and
settings the guest saves persist in these files.

Origami never rebuilds a missing state file, because that would discard what
the guest saved. A missing file is an error naming its path. Restore it from a
backup, or create a new machine and attach the old machine's disks.
Other commands refuse a machine created by Origami 0.1 until it is upgraded
in place, which keeps its flash, NVRAM, drives, network and MAC. Stop the
machine first. A machine with an IO PROM downloads one as `create` does, or
takes a local file:

```sh
origami upgrade my-origin
origami upgrade my-onyx2 --io-prom io6prom.img
```

The CLI starts an existing disk or CD-ROM image and prepares the Instigator server. The PROM, disk formatter, and first-run questions remain interactive. `install-apply` runs the generated `inst` package-selection script, and `install-finish` automates the RAD4 installer handoff.

The development media profile includes MIPSpro and development packages.

## Firmware and remote installation media

Omit `--prom` and `--io-prom` to fetch the pinned PROM images the machine
reads over HTTPS. Origami checks their size and SHA-256 before creating the
machine. Verified files are cached in the
host's user cache directory, and creation builds the machine's flash from them.
A warm cache supports offline creation. `--prom FILE` keeps the local-file
workflow, including custom IP27 PROMs. Running an existing machine never fetches
firmware.

```sh
origami create my-origin --preset origin200-1
origami install-init my-origin --mac 08:00:69:12:34:56
```

Without `--media-root`, install initialization configures remote sources.
Instigator fetches disc ranges and archive entries as needed during installation.
If the server refuses range requests, it can download a whole disc instead.
Initialization itself downloads nothing.

Choose `--profile base` for the six IRIX 6.5.30 overlay, foundation and NFS discs,
`--profile desktop` (the default) for ten discs including applications,
complementary software and their development-library dependencies, or
`--profile development` to add the existing MIPSpro 7.4.4 update and C compiler
tarballs. Remote disc objects use `.iso` filenames
and retain the original SGI disc bytes. The RAD4 driver is a separate add-on
that can use a local path or public HTTPS source.

Use `--media-root /path/to/media` for local installation. Its disc paths retain
`.image` filenames. Existing media configurations keep their original full
recipe. `--profile legacy-development` is available for that local recipe.

PROM downloads have pinned checksums. Instigator's remote installation sources
do not verify a whole-image checksum before serving them. The public desktop
media with RAD4 completed a fresh Linux Origin 200 installation and reached
an IRIX 6.5.30 serial root shell. Other profiles and machines remain
experimental. Desktop responsiveness was not established by that check.
