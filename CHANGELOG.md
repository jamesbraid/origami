# Changelog

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
