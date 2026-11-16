# origami-gtk

The Linux app: GTK 4 and libadwaita in Rust. It links the engine and display
protocol directly (no FFI) and follows `origami-app/ux/design`.

Until the shared core lands, the engine and the display protocol are compiled
in from `origami-core`; two files still come from the Slint testbed by `#[path]` (see `src/main.rs`).
When the core facade publishes, those two modules become a dependency.

## The picture

| Transport | State |
|---|---|
| QEMU dma-buf direct | waits for display protocol v4 |
| Vulkan import, GPU copy of the changed lines into dma-buf images, `GdkDmabufTexture` in a `GtkGraphicsOffload` | built; the GPU copy is waited for on a worker thread, never the UI thread |
| Shared memory, changed lines copied, `GdkMemoryTexture` | built; used with `--no-import` and when the Vulkan device cannot export (lavapipe) |

The GL route of the spike is gone.

## Running

    origami-gtk [--library DIR] [--start NAME] [--automation SOCKET] ...

The library is the folder of machine directories (default
`~/.local/share/origami/machines`). Create machines with `origami create` or
New Machine in the app.

## Checks

`ci/linux.sh SCRATCH [scenario...]` builds the images, builds and unit-tests the
crate, lays out the pinned QEMU runtime and runs the scenarios under Xvfb on
lavapipe. `ci/run.sh SCRATCH NAME x11|weston|cage gpu=N|none -- SCENARIO`
runs one session; `gpu=0` passes the first NVIDIA device through (rootless
Podman needs `--group-add keep-groups`, which `run.sh` adds, for the render
node).

The `first-light` scenario drives the real app over `--automation` and real
OS input (XTest under Xvfb, wtype under cage), and checks: the library, Start,
the first picture against a QMP `screendump` (0 differing pixels), the
zero-copy counters, typing at the PROM (`hinv`, read back by OCR of the
emulator's screendump), follow and capture pointer modes, key release on focus
loss, and idle cost.

Not covered yet: pointer injection under Wayland (no tool in the images),
weston screenshots, IRIX, CD swap and the Console and Diagnostics windows.
