# Origami preview

Experimental SGI emulation. Vibe coded paper silicon, with the usual paper cuts.

The bundle includes the `origami` CLI, QEMU and Instigator. It covers Origin
200, Origin 2000, Origin 300 and Onyx2, SMP and multiple nodes, plus RAD1/RAD4,
IMPACT/SI, InfiniteReality and VPro graphics. The CLI provides a smaller set
of managed presets. Other configurations use the bundled QEMU directly.
Fuel and Octane remain works in progress.

Archives target Linux x86-64 with glibc 2.39 or newer, Windows 11 x86-64 and
macOS 15 on Apple Silicon. Verify the supplied SHA-256 before extracting.
The binaries are unsigned. There is no package manager or automatic updater.

Firmware, guest disks and installation media are not included. Supply your
own, then start with `origami machines` and the
[user guide](https://github.com/jamesbraid/origami/blob/main/docs/usage.md).

Some configurations reach an IRIX desktop. Others remain unfinished. Graphics,
input, firmware and guest support vary by machine and revision. Expect broken
things and frequent releases. The user guide records tested paths and limits.

The frontend, build tooling and our original QEMU additions use BSD-3-Clause.
The combined QEMU emulator is GPL. Upstream code and bundled dependencies
retain their own licenses, included under `share/sgi/licenses`.
