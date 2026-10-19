# Changelog

## Unreleased

Machines created by 0.2.x must be upgraded once before use:
`origami upgrade DIR`. Origin 200, Origin 2000 and Onyx2 machines gain the
new module controller NVRAM file, which needs their PROMs: the upgrade
downloads them, or takes `--prom FILE` and `--io-prom FILE`. Flash, NVRAM,
drives and network settings are kept.

- RAD4 installs keep the driver's DMA enabled, which makes window drags
  and resizes several times faster. To update an existing RAD4 install,
  set `rad4pci_no_dma = 0` in `/var/sysgen/master.d/rad4pci` as root in
  the guest, run `autoconfig` and reboot.
- The Origin 200 presets now create headless machines.
- New preset `origin200-rad4`: the two-CPU Origin 200 with RAD4 graphics.
  `install-init` configures the RAD4 add-on on RAD4 machines.
- Origin 200 boots with more memory sizes, up to 1664 MiB per node.
- QEMU updated to c97bfd9.
- `network-set --mode tap` (Linux, Windows) and `--mode vmnet` (macOS, through
  socket_vmnet) put the guest on a host network.
- `install-apply --addon` installs IRIX first and the add-on in a second
  pass, then quits `inst` and restarts the guest to its login prompt.
  `install-finish` and the RAD4 finish script are gone: a RAD4 install
  now keeps IRIX's own kernel and X configuration links, and the kernel
  is relinked with the driver when `inst` exits.
- The documented RAD4 selection is `rad4x.sw.driver`, `rad4x.sw.xserver`
  and `rad4x.man`, leaving out PsiTech's 1998 kernel patch.
- Instigator updated to v0.4.0.
- User networking offers the machine directory's name as the DHCP hostname.
- `--profile base` installs no longer stop at package conflicts.
- Every guided install turns on IRIX's DHCP client before `inst` exits.
- Guided installs set `AutoLoad`, so a cold start boots straight to IRIX.

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
