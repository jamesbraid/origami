# Changelog

## 0.4.0

Machines created by 0.3.x must be upgraded once: `origami upgrade DIR`.
The upgrade stores each machine's graphics as the board QEMU now builds and
says when the guest will see a change. InfiniteReality on an Origin 2000 or
Origin 200 GIGAchannel, and the `origin300-v12-direct` and `origin300-vbrick`
topologies, are refused with the reason and left untouched; create an Onyx2
for InfiniteReality.

- Graphics choices come from QEMU's machine list. `create --graphics PRODUCT`
  takes any board it lists (`si`, `esi`, `mxi`, `v12`, `ir2`, `ir3`, `rad4`
  and others) or `none`, with `--graphics-slot SLOT` and repeatable
  `--graphics-setting NAME=VALUE`. `origami machines` lists every slot, board,
  setting and known approximation; a refused choice prints QEMU's reason.
- New presets: `octane2-vpro` (V12), `origin2000-impact` (one-node Origin
  2000 deskside, SI in io3) and `onyx2-rack-infinite-reality` (one-node
  Onyx2 rack, InfiniteReality3), `onyx350-vpro` (four-CPU Onyx 350, V12)
  and `tezro-vpro` (four-CPU Tezro tower, V12).
  `fuel-1` now has its VPro V12. The Origin 300 V12 and V-brick presets are
  gone with their topologies.
- An Onyx2 deskside's InfiniteReality2 answers with its boards configured,
  two RM7 raster managers and a DG5-8, so `hinv` and `gfxinfo` list them.
- Graphics cannot change after a machine is created; `graphics-set` says so.
- New Origin 2000 rack, Onyx2 rack and Onyx2 deskside machines get
  256 MiB per node, the smallest size SGI shipped, instead of 64 MiB.
  Existing machines keep their size.
- QEMU updated to 16b4488221.

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
