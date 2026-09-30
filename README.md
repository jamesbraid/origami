# Origami

An experimental SGI emulator built on QEMU. Vibe coded paper silicon.

Inspired by [Iris](https://github.com/techomancer/iris), and nostalgia for the
systems I cut my teeth on as a young fella. This became an adventure in burning
tokens. Somehow, IRIX boots.

[Our extremely elite website](https://origami.irix.fans) ·
[User guide](docs/usage.md) ·
[Contributing](.github/CONTRIBUTING.md)

![IRIX running the OpenGL Performer Lotus demo on emulated SI graphics](docs/images/si-performer.png)

*SI graphics, IRIX and a Lotus. Development capture.*

## What's in here

The Rust `origami` CLI manages machines, disks, consoles and network installs.
QEMU does the emulation. [Instigator](https://github.com/jamesbraid/instigator)
serves the installation media.

| Hardware | Emulated bits |
|---|---|
| Machines | Origin 200, Origin 2000, Origin 300, Onyx2, with deskside, rack and GIGAchannel configurations |
| CPUs | R10000 and R14000, multiple CPUs and NUMA nodes |
| Graphics | PsiTech RAD1 and RAD4, SI (MGRAS), InfiniteReality |
| Interconnect | Hub, Bedrock, Crossbow, Bridge, XBridge, routers and CrayLink |
| I/O boards | BaseIO, BaseIO-G / MediaIO, IO-8, MENET, MSCSI, PCI carriers and shoehorns |
| Storage | QLogic ISP1040 / ISP12160 SCSI controllers, disks, CD-ROMs and tapes |
| Networking | IOC3 Ethernet, Tigon and BCM570x adapters |
| Peripherals | Serial ports, PS/2 keyboard and mouse, OHCI USB, RAD1 audio |
| Housekeeping | ELSC, module management, L1, flash, EEPROMs and clocks |

VPro / Odyssey is also under development. It isn't in the current QEMU pin yet.
The CLI has fewer presets than QEMU has configurations. The
[user guide](docs/usage.md) covers those, and the
[QEMU machine reference](qemu/docs/specs/sgi-sn.rst) covers the rest.

Implemented does not mean finished. Some combinations boot to a desktop.
Others are an opportunity to stare at a serial console. This is a hobby
experiment. Expect broken things and frequent changes.

## A little more paper silicon

| VPro / Odyssey: IRIX desktop | InfiniteReality: PROM menu |
|---|---|
| ![IRIX desktop with gr_osview and Icon Catalog on emulated VPro graphics](docs/images/vpro-desktop.png) | ![InfiniteReality graphical System Maintenance Menu on emulated InfiniteReality graphics](docs/images/infinitereality-prom.png) |

These are development captures from different QEMU revisions, including work
newer than the bundled version. [Capture details](docs/images/README.md).

## Trying it

Preview builds target Linux x86-64, Windows x86-64 and macOS arm64.
You'll need your own firmware and guest installation media. They aren't bundled.
Start with `origami machines` and the [user guide](docs/usage.md).

## License

[BSD-3-Clause](LICENSE) for the frontend and build tooling. QEMU and bundled
dependencies retain their own licenses.
