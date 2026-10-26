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
origami create my-origin --preset origin200-rad4 --memory-per-node 128
origami create my-o200 --preset origin200-2 --processor r12000-270mhz-4mb
origami create my-o300 --preset origin300-2
origami create my-onyx2 --preset onyx2-infinite-reality --graphics-setting rm-count=1
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
origami install-apply my-origin
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
origami install-init my-origin --mac 08:00:69:12:34:56 --profile standard
origami install-addon my-origin --name mipspro
origami install-addon my-origin --name tablet --source /path/to/tablet.tardist --install PRODUCT.SUBSYSTEM
origami install-check my-origin
origami install-serve my-origin
```

This configures the public IRIX 6.5.30 media for IRIX's standard selection.
Use `--profile minimal` for a smaller installation. For local media, add
`--media-root /path/to/media`, naming the directory containing `6.5.30/` and
`6.5-base/`. `install-init` writes sources to `my-origin/install/media.toml`.

`install-addon` is optional. Stop the machine before changing add-ons.
`--name mipspro` configures the MIPSpro 7.4.4 compilers from the public
archive. Add `--media-root /path/to/media` to use `mipspro/7.4.4/` from local
media instead. Use it with the standard profile: it upgrades compiler
packages that profile installs from the development discs. `cc` prints a
FLEXlm message that the compiler needs a license password, then compiles and
exits 0.
Replace `PRODUCT.SUBSYSTEM` with the package selection named by the add-on.
A source can be a local SGI image, extracted tree, `.tar`, `.tar.gz`, `.tgz`,
`.tardist`, or `.tardist.gz`, or a public HTTPS URL. HTTPS sources use
Instigator's existing remote-media support. The add-on remains outside the
emulator archive and is configured in the machine's media manifest.

On a machine with RAD4 graphics, such as one created from `origin200-rad4`,
`install-init` also configures the RAD4 driver as add-on `rad4`. It selects
the driver, X server and manual pages from distribution `dist_6.5` of the
public archive, as this command would for another machine:

```sh
origami install-addon my-origin \
  --name rad4 \
  --source https://origami-dist.irix.fans/irix/addons/rad4/rad4x_65_13k.tar.gz \
  --install rad4x.sw.driver \
  --install rad4x.sw.xserver \
  --install rad4x.man \
  --dist dist_6.5
```

Do not select all of `rad4x`. Its `rad4x.sw.kernel_patch` replaces two IRIX
6.5.30 kernel objects with 1998 copies, and `rad4x.sw.prom` only installs a
PROM image file.

`install-check` uses the packaged Instigator to open every named source,
including any add-on, and assemble the install tree without opening
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
A foreground `origami run` cannot be controlled by `install-apply`.

The generated Instigator server listens at `10.98.0.2` and offers
`10.98.0.65` to the configured MAC. On a new Origin 200 disk, enter the PROM
command monitor from the System Maintenance Menu, turn on automatic booting
for later starts, and boot the disk formatter:

```text
setenv netaddr 10.98.0.65
setenv AutoLoad Yes
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
origami install-apply my-origin
```

The command streams the installer output and waits through package transfer,
the dependency check and the return to `Inst>`. With add-ons configured, IRIX
installs first and every add-on installs in a second `go` from the same
script, so the command waits for `Inst>` twice. Use `--addon rad4` to install
only that add-on.

After installation the command leaves `inst` with `quit`, which relinks the
kernel with any driver the add-on installed, answers the restart prompt and
waits for the serial login prompt. If the first boot instead rebuilds the
kernel, the command logs in as `root`, which has no password on a new
install, and restarts the guest once more. It stops and leaves the guest
running if `inst` reports a conflict or a failed command, if the
post-install commands do not finish, if `quit` prints an `lboot` message, or
if a step does not finish. Inspect the serial output, then continue by hand.

The same scripts can be loaded manually with
`admin source 10.98.0.2:/irix.cmds` for the profile alone,
`admin source 10.98.0.2:/addons.cmds` or
`admin source 10.98.0.2:/addon-NAME.cmds`. After the last `go`, each script
runs `chkconfig -f autoconfig_ipaddress on` and `nvram AutoLoad Yes` in the
installed system, so the guest asks for its address by DHCP and the PROM boots
IRIX without waiting at its menu, then prints
`instigator: post-install commands finished`. Wait for that line and `Inst>`
before entering `quit`. The RAD4 package's miniroot exit command reports
`autoconfig: cannot find /usr/sbin/lboot`. The kernel is relinked at `quit`
instead, so the message needs no action.

With RAD4, the guest boots to PsiTech's unbuffered X server and the graphical
login. PsiTech's documented choices are made on the running system with
`chkconfig`: `rad4X_buffered_server on` selects the buffered server, and
`rad4X_usekm_devs on` selects a RAD4-KM keyboard and mouse. The driver keeps
its shipped DMA setting.

A machine left at `Inst>` by an earlier release's single-pass RAD4 script
installed the driver before IRIX's own files, and this release cannot finish
it. Finish it with that release's `install-finish`, or reinstall.

A RAD4 guest finished by an earlier release's helper runs with the driver's
DMA turned off. To turn it back on, which makes window drags and resizes much
faster, set `rad4pci_no_dma = 0` in `/var/sysgen/master.d/rad4pci` as root,
run `autoconfig` and reboot.

After the guest restarts, stop the machine before using
`origami network-set my-origin --mode user` to return it to QEMU user networking.
`--mode none` disconnects it.
The PROM boot, disk preparation and first-run questions remain interactive.

An installed guest has DHCP and automatic booting turned on. With user
networking it takes its address, default route and DNS server from QEMU by
DHCP, and its hostname from the machine directory's name when that is a valid
hostname. A cold start, `origami stop` then `origami run`, boots straight to
IRIX. Press Esc while the PROM prints "Starting up the system" to stop at its
System Maintenance Menu instead. After a panic the machine may restart and boot
IRIX again on its own, as IRIX's `reboot_on_panic` setting decides. A real
Origin 200 waits at that menu until `AutoLoad` is set. To turn it off again,
run `nvram AutoLoad No` as root, or `setenv AutoLoad No` in the command
monitor.

A forward binds only to the host loopback address. The example sends host TCP
port 2222 to guest port 22 while user networking is active. Use
`origami network-forward-remove my-origin ssh` to remove it. Forwards stay in
`machine.toml` while private installation networking or no networking is
selected, and become active again with `network-set --mode user`. Stop the
machine before changing its network or forwards.

To put the guest on a real LAN or the host's vmnet subnet, connect it to a
host network device instead. These modes need the machine's MAC, so supply
`--mac` if it has none. The guest gets its own address there, from that
network's DHCP server or by hand, and forwards stay inactive. Origami runs no
setup scripts and needs no administrator rights itself: the host device is
created once, as root, beforehand.

```sh
origami network-set my-origin --mode tap --interface tap0                    # Linux
origami network-set my-origin --mode tap --interface "OpenVPN TAP-Windows6"  # Windows
origami network-set my-origin --mode vmnet --endpoint /opt/homebrew/var/run/socket_vmnet  # macOS
```

- **Linux:** create a tap device owned by your user and add it to a bridge
  that holds the host's Ethernet interface:
  `sudo ip tuntap add dev tap0 mode tap user $USER` then
  `sudo ip link set tap0 master br0 up`. Setting up `br0` itself depends on
  the distribution's network manager; see the
  [QEMU networking documentation](https://wiki.qemu.org/Documentation/Networking#Tap).
  Each running machine needs its own tap device.
- **Windows:** install a TAP-Windows6 adapter, from OpenVPN or
  [tap-windows6](https://github.com/OpenVPN/tap-windows6), as an administrator.
  `--interface` is the adapter's name in Network Connections. Use Bridge
  Connections there to join it to the LAN. To run without administrator rights,
  set the adapter's Non-Admin Access property to Allowed.
- **macOS:** install and start [socket_vmnet](https://github.com/lima-vm/socket_vmnet):
  `brew install socket_vmnet && sudo brew services start socket_vmnet`.
  `--endpoint` is its socket. The default socket gives a shared NAT subnet;
  socket_vmnet's bridged mode gives a socket such as
  `socket_vmnet.bridged.en0` that joins the LAN. `run` refuses to start if
  nothing answers on the socket. Tap mode is not available on macOS, and vmnet
  mode only on macOS.

If QEMU cannot open the device, `run` shows QEMU's message and a hint.

`origami machines` lists 16 starter presets for Origin 200, Origin 2000,
Onyx2, Origin 300, Onyx 350, Octane, Octane2, Fuel and Tezro, each with its
graphics, and then every graphics choice those machines offer.
`origin200-1`, `origin200-2`, `origin200-dual`, `origin2000-8` and
`origin300-2` are headless.
`origin200-rad4` is the two-CPU Origin 200 with RAD4 graphics, the setup for
an IRIX desktop. The IMPACT presets fit an SI board; `origin2000-impact` is a
one-node Origin 2000 deskside with its SI in XIO slot 3. `octane2-vpro`,
`fuel-1`, `onyx350-vpro` (a four-CPU Onyx 350) and `tezro-vpro` (a
four-CPU Tezro tower, V12 at PIC widget 12) have VPro V12.
`onyx2-infinite-reality` is an Onyx2 deskside with InfiniteReality2, and
`onyx2-rack-infinite-reality` a one-node Onyx2 rack with InfiniteReality3.
Origin 300 has no graphics.

### Graphics

QEMU's machine list describes each machine's graphics: its slots, the boards
each slot takes, every setting a board leaves open with the values on offer,
what QEMU approximates, and why a combination is refused. Origami offers only
what that list offers. A preset gives a machine its graphics unless `create`
names others:

- `--graphics PRODUCT` picks a board by its product name, such as `si`, `esi`,
  `v12`, `ir2`, `ir3` or `rad4`, or `none` for no graphics.
- `--graphics-slot SLOT` puts it in another slot, such as `io5` on an Origin
  2000 or `widget-9` on an Octane. Without it the board goes in the slot the
  preset names, else the machine's default graphics slot, else the first slot
  that takes it.
- `--graphics-setting NAME=VALUE`, repeatable, changes one setting from its
  default, such as `rm-count=1` or `display=dg5-2` for InfiniteReality.

```sh
origami create my-octane2 --preset octane2-vpro
origami create my-octane --preset octane-impact --graphics esi --graphics-slot widget-9
origami create my-onyx2 --preset onyx2-infinite-reality --graphics ir2 \
  --graphics-setting geometry=ge14-2 --graphics-setting display=dg5-2
```

A refused choice prints the reason the machine list gives, such as four raster
managers on an Onyx2 deskside or a V12 on an Octane. Onyx2 and Fuel are not
offered without graphics. Machines that have no graphics by default, such as
the Origin 200, are headless unless a board is chosen.

Two IRIX desktops do not start yet. An Origin 2000's BaseIO board has no
keyboard or mouse ports, so the X server stops at "Couldn't find core keyboard
device" although `hinv` lists the SI board. On the Onyx2 deskside IRIX reports
"IR0: Fatal error" for the InfiniteReality2 pipe and starts no X server; the
Onyx2 rack's InfiniteReality3 reaches the desktop.

`machine.toml` records each board as a `[[pipe]]` with its slot, family,
product and every setting, so a later release that changes a default does not
change an existing machine. `show` lists the board, its settings and any
approximation. Graphics cannot change after a machine is created yet:
`graphics-set` refuses, and the way to try other graphics is a new machine
with the old machine's disks attached.

The console uses the serial line QEMU's machine list marks as the machine's
console: the L1 on Origin 300 and Fuel, and IOC3 port A elsewhere. A Fuel
whose PROM environment is set to `console=d` talks on IOC3 port A instead.
Choose another line while the machine is stopped, or omit `--port` to return
to the default. Lines take their machine list names, such as `l1`, `ioc3_a` and
`ioc3_b`; an unknown name lists the machine's choices.

```sh
origami console-set my-fuel --port ioc3_a
```

`create --memory-per-node MiB` selects one of the values accepted by the preset.

Origin 200 presets take `create --processor ID` to fit one of the processor
modules QEMU models; `origami machines` lists them. Without it a machine gets
the 180 MHz R10000 module with 1 MB cache. The 225 MHz R10000 and 270 MHz
R12000 modules hold two CPUs, so with them `origin200-1` and `origin200-impact`
get two. Memory depends on the module: the PROM turns off 256 MB DIMM banks
on a 180 MHz node, which therefore takes at most 1664 MiB and refuses sizes
such as 1024 and 1536, while the faster modules take up to 1792 MiB. A
refused size lists the accepted ones.

`create` writes the module into each node's boot flash and records it as
`processor` in `machine.toml`; a machine without that entry has the default
module. The module cannot change afterwards. QEMU does not rewrite the flash,
so an edited entry leaves the flash describing the old module, and the machine
then starts with a silent console. To try another module, create a new
machine and attach the disks. QEMU shows Hub revision 3 and the 180 MHz IP29 board in
`hinv` for every module, where real 225 and 270 MHz machines have Hub
revision 5 and IP29 030-1389-001.
The full QEMU machine list also validates additional topologies and processor
populations configured in `machine.toml`. Use `topology` and `population` to
identify those configurations. Support remains experimental: a selectable
configuration does not imply that firmware, installation or graphics work.
The [QEMU machine documentation](https://github.com/jamesbraid/qemu-sgi-origami/blob/sgi-origami/docs/specs/sgi-sn.rst)
describes topology and board options. QEMU's machine list gives each machine its
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
machine list names them. `create` runs QEMU's `qemu-sgi-machine-init` to
build these files from the PROM images, and keeps no copy of the images.
Later runs need neither the PROM files nor a download. PROM updates and
settings the guest saves persist in these files.

Origami never rebuilds a missing state file, because that would discard what
the guest saved. A missing file is an error naming its path. Restore it from a
backup, or create a new machine and attach the old machine's disks.
Other commands refuse a machine created by Origami 0.1, 0.2 or 0.3 until it
is upgraded in place, which keeps its flash, NVRAM, drives, network and MAC.
Stop the machine first. Upgrading a 0.2 Origin 200, Origin 2000 or Onyx2 adds
the module controller NVRAM file those machines now have, in the state `create`
gives a new machine. The upgrade reads the machine's PROMs to build that
file. It downloads them as `create` does, or takes local files. Upgrading a
machine that is already current changes nothing.

The upgrade also turns the old one-word `graphics` value into the board it
built, with the machine list's defaults for every setting. A few machines
change what the guest sees at its next boot, and the upgrade says so: an
Origin 200 GIGAchannel's IMPACT board moves from io3 to io6, where real
GIGAchannel machines carry graphics; a dual Origin 200's RAD4 moves to module
0; a Fuel gains the VPro V12 every Fuel has; and an Onyx2 deskside's pipe
answers with its raster manager, texture and display boards configured.

Some old machines have no upgrade, and the upgrade refuses them with the
reason, leaving `machine.toml` unchanged. InfiniteReality on an Origin 2000
or an Origin 200 GIGAchannel is refused, because an Origin 2000 with an
InfiniteReality pipe is an Onyx2: create an `onyx2-deskside` or `onyx2-rack`
machine and attach the disks. The `origin300-v12-direct` and
`origin300-vbrick` topologies are gone, because they modelled hardware SGI
never built.

```sh
origami upgrade my-origin
origami upgrade my-origin --prom ip27prom.img
origami upgrade my-onyx2 --io-prom io6prom.img
```

The CLI starts an existing disk or CD-ROM image and prepares the Instigator server. The PROM, disk formatter, and first-run questions remain interactive. `install-apply` runs the generated `inst` package-selection script and restarts the installed system.

## Firmware and remote installation media

Omit `--prom` and `--io-prom` to fetch the pinned PROM images the machine
reads over HTTPS. Origami checks their size and SHA-256 before creating the
machine. Verified files are cached in the
host's user cache directory, and creation builds the machine's flash from them.
A warm cache supports offline creation. `--prom FILE` keeps the local-file
workflow, including custom IP27 PROMs. Running an existing machine never fetches
firmware.

```sh
origami create my-origin --preset origin200-rad4
origami install-init my-origin --mac 08:00:69:12:34:56
```

Without `--media-root`, install initialization configures remote sources.
Instigator fetches disc ranges and archive entries as needed during installation.
If the server refuses range requests, it can download a whole disc instead.
Initialization itself downloads nothing.

Choose `--profile minimal` for the six IRIX 6.5.30 overlay, foundation and NFS
discs, or `--profile standard` (the default) for all ten discs, adding
applications, complementary software and the development discs. Both install
IRIX's standard selection from their discs. Remote disc objects use `.iso`
filenames and retain the original SGI disc bytes. MIPSpro and the RAD4 driver
are add-ons that can use a local path or public HTTPS source.

Use `--media-root /path/to/media` for local installation. Its disc paths retain
`.image` filenames. The `base` and `desktop` profiles are now `minimal` and
`standard`. A media configuration that names an old one keeps working. The
`development` profile is gone, as is the recipe that a media configuration
without a profile used: both installed MIPSpro, which is now the `mipspro`
add-on. Remove `install/media.toml`, run `install-init` again and add MIPSpro
with `install-addon`.

PROM downloads have pinned checksums. Instigator's remote installation sources
do not verify a whole-image checksum before serving them. The public standard
media with RAD4 completed a fresh Linux Origin 200 installation and reached
an IRIX 6.5.30 serial root shell. Other profiles and machines remain
experimental. Desktop responsiveness was not established by that check.
