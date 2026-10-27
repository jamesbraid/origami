# Changelog

## Unreleased

- `origami machines` prints one table of the presets, and
  `origami machines PRESET` or `--all` shows a preset's graphics choices,
  slots, refusals and processor modules, in place of the long listing.
- `show-command` prints a QEMU command you can copy, edit and run.
  `[qemu] extra-args` in `machine.toml` and `--qemu-arg` on `run` append
  your own QEMU options.
- `origami nvram DIR` reads and changes a stopped machine's PROM
  environment, such as `console` and `AutoLoad`.

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
