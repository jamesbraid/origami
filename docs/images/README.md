# Screenshot captures

Emulator captures. The `hinv` images are cropped, and the Origin 2000
image has contrast adjusted to make its dark text readable. The graphics
captures are unedited. These show individual development runs,
not compatibility guarantees for all configurations.

## si-performer.png

- Machine: `origin2000`.
- Graphics: SI (`sgi-mgras`).
- QEMU revision: `5a243476f2a48dc94a52021d2c5962785c8b8308`.

## vpro-desktop.png

- Machine: `origin300`.
- Graphics: VPro / Odyssey (`sgi-odyssey`).
- QEMU revision: `42455b48e1fc55e465b3bdab450aaad56e83181c`.

## infinitereality-prom.png

- Machine: `origin2000`.
- Graphics: InfiniteReality (`sgi-kona`) attached to an Origin 2000.
- QEMU revision: `37a094bca110d449b6e5d8fc099c173f566af557`.

The VPro capture includes visible rendering defects. InfiniteReality shows
the firmware menu, not an IRIX desktop. The SI capture shows one OpenGL
Performer demo, not general OpenGL compatibility.

## origin200-hinv.png

- Machine: `origin200`, four CPUs and two nodes.
- QEMU revision: `687740296cb0c1724623b949504f39ff4a692706`.
- Capture: `rad4-dualnode-hinv.ppm`, 18 September 2026.
- Cropped to the console. The Icon Catalog overlaps its right edge.

## origin2000-hinv.png

- Machine: `origin2000`, eight CPUs and four nodes, SI graphics.
- QEMU revision: `ca021b99c68e4c2e9460b730fcef867d9e89831d`.
- Capture: `attempt1-hinv.ppm`, 24 September 2026.
- Cropped to the console. Dark text was changed to light text against
  a dark background for readability. No text was added or replaced.
