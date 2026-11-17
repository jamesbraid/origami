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
lavapipe, then the pointer scenario under sway headless. `ci/run.sh SCRATCH
NAME x11|weston|cage|sway gpu=N|none -- SCENARIO` runs one session; `gpu=0`
passes the first NVIDIA device through (rootless Podman needs
`--group-add keep-groups`, which `run.sh` adds, for the render node).

Scenarios drive the real app over `--automation` and real OS input (XTest under
Xvfb, wtype under cage, a virtual pointer under sway):

- `first-light`: the library, Start, the first picture against a QMP
  `screendump` (0 differing pixels), the zero-copy counters, typing at the PROM
  (`hinv`, read back by OCR of the emulator's screendump), follow and capture,
  key release at focus loss, and idle cost.
- `pointer`: the same up to the picture, then only the pointer checks.
- `hidpi`: two machines of one product opened by real library clicks; the picture's
  scale step, filter and pointer mapping under `GDK_SCALE=2` (`XVFB_SCREEN`,
  `HIDPI_WINDOW`) or a scaled sway output (`SWAY_OUTPUT`).
- `frames`: boot to the PROM and print what presenting cost.
- `sidebar`: real clicks on the library (select, open, row menu), the tab bar and
  the summary cards.
- `diagnostics`: the Diagnostics window from the row menu, Copy Report and the
  clipboard; `first-light` and `pointer` also check a running machine's report.
- `console`: the Console window under real input, with the PROM's serial output
  on screen: mouse selection, copy, paste, scrollback and one window per machine.
- `settings`: edits through the core on a stopped machine, refused while it runs.

Capture takes the window system's pointer lock: Wayland pointer constraints
with relative pointer (sway), or warp-to-centre on X11. Cage has no pointer
constraints, so there it falls back to differencing positions.

Not covered yet: weston screenshots, IRIX and CD swap.
