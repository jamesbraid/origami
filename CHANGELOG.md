# Changelog

## 0.5.0

Big iron. The Origin 3200, 3400 and 3900 servers install and run IRIX
6.5.30, and any machine can now share a host folder with IRIX over NFS,
with no host NFS server or administrator rights.

    $ hinv            # Origin 3900
    16 700 MHZ IP35 Processors
    CPU: MIPS R16000 Processor Chip Revision: 3.0
    Main memory size: 8192 Mbytes
    ROUTER in Module 001c07/Slot 0: Revision 1: Active Ports [2,3,4,5,] (enabled)
    Integral SCSI controller 0: Version Fibre Channel QL2200, 33 MHz PCI
    Integral Fast Ethernet: ef0, version 1, module 001c07, pci 4

Every machine's `hinv` is in the [gallery](https://github.com/jamesbraid/origami/blob/v0.5.0/docs/gallery.md).

- New:
  - Presets `origin3200` (8 CPUs), `origin3400` (16 CPUs) and `origin3900`
    (16 R16000s). IRIX installs on the Origin 3200's Fibre Channel disk
    over its serial console, and the disk boots on the others.
  - `origami share DIR NAME HOSTDIR` serves a host folder to IRIX.
    Mount it with `mount 192.0.2.4:/NAME /mnt/NAME`.
  - `--mode private` with no install server running gives the guest its
    own network: `10.98.0.65` by DHCP, shares at `10.98.0.2`.
  - `install-init --release 6.5.22` installs IRIX 6.5.22, with MIPSpro
    and RAD4.
  - Presets `onyx3000-vpro` and `onyx3400-infinite-reality`: IRIX on an
    Onyx 3000 with VPro V12 and an Onyx 3400 with InfiniteReality3.
- Fixed:
  - An installed system is named after its machine directory, not IRIS.
  - Remote installs retry transient download errors instead of stopping.
- Installs include the NFS client.
- QEMU updated to cf2948f095. Instigator updated to 0.5.0.

## 0.4.1

Install IRIX on any graphics machine from its own console: the user guide
now walks through it with screenshots. This release also fixes what 0.4.0
left broken. Octane with SI shows its console, the Onyx 350 and Tezro
reach the network, and the VPro pointer is visible. VPro and
InfiniteReality are faster.

<img src="https://raw.githubusercontent.com/jamesbraid/origami/v0.4.1/docs/images/gallery/onyx350-vpro-prom.png" alt="Onyx 350 PROM menu" width="400"> <img src="https://raw.githubusercontent.com/jamesbraid/origami/v0.4.1/docs/images/gallery/onyx2-infinite-reality-hinv.png" alt="Onyx2 InfiniteReality2 desktop with hinv" width="400">

Every machine is in the [gallery](https://github.com/jamesbraid/origami/blob/v0.4.1/docs/gallery.md).

- Fixed:
  - Octane and Octane2 with SI no longer start to a black screen.
  - An Origin 200 with IMPACT installs X and reaches the 4Dwm desktop.
  - The Onyx 350 and Tezro connect their onboard Ethernet.
  - A new Onyx 350 starts on its graphics console.
  - The VPro pointer shows in the PROM and in X.
  - VPro no longer draws lighter squares behind desktop icons.
  - `/irix.cmds` installs the machine's add-ons too, so an Origin 200
    with RAD4 gets its X server.
- Faster:
  - The VPro textport scrolls about 4x faster.
  - Fuel, Tezro and Onyx 350 keep normal guest speed under heavy GL.
  - InfiniteReality texture loads are about 11x faster.
- New:
  - `origami nvram DIR` reads and changes a stopped machine's PROM
    environment, such as `console` and `AutoLoad`.
  - `show-command` prints a QEMU command you can copy, edit and run, and
    `[qemu] extra-args` or `--qemu-arg` add your own options.
  - `origami machines` prints one table of the presets; `origami
    machines PRESET` shows the detail.
  - `install-check` probes remote media instead of downloading it.
  - `create` gives each machine a unique SGI MAC. Non-SGI MACs are
    refused.
- QEMU updated to 49b5ef3757.

## 0.4.0

SGI graphics arrive. Nine machines now boot IRIX to a working GL desktop
with their own graphics hardware: Octane with SI, Octane2, Fuel, Tezro and
Onyx 350 with VPro V12, Origin 2000 with SI, Onyx2 with InfiniteReality2 or
InfiniteReality3, and Origin 200 with RAD4. Pick one with a preset, or any
board QEMU models with `create --graphics`.

Machines created by 0.3.x must be upgraded once: `origami upgrade DIR`.
An Origin 2000 or Origin 200 with InfiniteReality is refused, because that
hardware is an Onyx2: create an Onyx2 instead.

- New presets: `octane2-vpro`, `origin2000-impact`,
  `onyx2-rack-infinite-reality`, `onyx350-vpro` and `tezro-vpro`.
  `fuel-1` now comes with its VPro V12.
- `origami machines` lists every graphics board each machine takes, and its
  known limits.
- New Origin 2000 and Onyx2 machines get 256 MiB per node, the smallest
  SGI shipped, instead of 64 MiB. Existing machines keep their size.
- QEMU updated to 4013f9f84f.

## 0.3.1

- An Origin 200 can take any of its processor modules:
  `create --processor ID` picks a 180 MHz R10000 with 1 MB (the default)
  or 2 MB cache, a 225 MHz R10000 or a 270 MHz R12000, and `origami
  machines` lists them. The two faster modules always hold two CPUs and
  allow up to 1792 MiB per node. The module is fixed when the machine is
  created. Existing machines keep the default module and need no upgrade.
- Piping output, as in `origami machines | head`, ends quietly instead of
  panicking.
- `origami --version` reports Instigator as v0.4.0. The 0.3.0 archives
  showed a pseudo-version for the same code.
- QEMU updated to 47542527ab.

## 0.3.0

Machines created by 0.2.x must be upgraded once: `origami upgrade DIR`.
Origin 200, Origin 2000 and Onyx2 machines gain a module controller NVRAM
file built from their PROMs, which the upgrade downloads or takes from
`--prom FILE` and `--io-prom FILE`. Flash, NVRAM, drives and network
settings are kept.

Install profiles are now `minimal` and `standard`; `base`, `desktop` and
`development` are gone. The MIPSpro compiler is an add-on:
`--profile standard` with `install-addon --name mipspro`.

Source checkouts: QEMU now comes from github.com/jamesbraid/qemu-sgi-origami.
Run `git submodule sync && git submodule update` once.

- Origin 200 presets are headless. The new `origin200-rad4` preset is the
  two-CPU Origin 200 with RAD4 graphics, and `install-init` adds the RAD4
  add-on for it.
- Add-ons install in a second `inst` pass after IRIX, and `install-apply`
  restarts the guest to its login prompt. `install-finish` is gone.
  Without `--addon`, `install-apply` installs every configured add-on.
- RAD4 keeps the driver's DMA on, which makes window drags and resizes
  several times faster. On an existing RAD4 install, set
  `rad4pci_no_dma = 0` in `/var/sysgen/master.d/rad4pci`, run `autoconfig`
  and reboot.
- Guided installs turn on DHCP and `AutoLoad`. A cold start boots straight
  to IRIX, which gets its address and its hostname (the machine directory's
  name) from user networking.
- `minimal` installs no longer stop at package conflicts.
- `network-set --mode tap` (Linux, Windows) and `--mode vmnet` (macOS, via
  socket_vmnet) put the guest on a host network.
- Origin 200 accepts more memory sizes, up to 1664 MiB per node.
- QEMU updated to 24b27ce. Instigator updated to v0.4.0.

## 0.2.1

- QEMU updated to fa81284.
- The CLI talks to QEMU through the qapi library instead of its own
  protocol code.

## 0.2.0

Machines created by 0.1.x must be upgraded once before use:
`origami upgrade DIR`. Origin 2000 and Onyx2 machines may need
`--io-prom FILE` if the IO PROM cannot be downloaded. PROM settings,
logs, drives and port forwards are kept, and the old files stay in place.

- The machine list now comes from the bundled QEMU, so the CLI and QEMU
  always agree on what each machine supports.
- QEMU now creates a new machine's flash, NVRAM and clock files.
- Each machine has one MAC address, used for its Ethernet.
- QEMU updated to 370fbb5.

## 0.1.2

- Fix PROM downloads, which failed for every machine in 0.1.1.

## 0.1.1

First public release.

- Portable archives for Linux, Windows and MacOS
- QEMU's libraries come from pinned vcpkg ports instead of the build host.

## 0.1.0

First preview, not published as a release.

- The `origami` CLI creates, runs and stops Origin 200, Origin 2000,
  Origin 300 and Onyx2 machines from presets. Fetches PROMs on demand.
- Tested with IRIX installing from scratch on an emulated Origin 200 with RAD4
  graphics.
