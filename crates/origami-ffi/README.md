# origami-ffi

What the native apps (Swift, C#, GTK) bind to. Two planes over `origami-core`'s
facade (`crates/origami-core/src/facade/`):

- **Control plane, UniFFI.** Machines, operations, events, console, input,
  commands. Generated Swift and C#. Names carry an `Origami` prefix.
- **Frame plane, C ABI.** Pixels, the frame wake-up, cursor, input, presenting
  into a window. Header: [`include/origami_frame.h`](include/origami_frame.h)
  (checked in; regenerate with `bindings/gen-header.sh`, CI fails if stale).
  Ownership and threading rules are in the header.

`OrigamiMachine.frame_plane_handle()` joins them: it returns an owned
`origami_machine*` (as an integer) for the C functions. Release it with
`origami_machine_release`.

## Features

| feature | meaning |
|---|---|
| `fake` | `FakeEngine` is the only engine; adds `fake_*` helpers to drive it. Bindings tests and app development use this. |
| `qemu` (default) | `QemuEngine` (found by `Runtime::locate`, beside the executable). Ignored when `fake` is on. |
| `render` (default) | `origami_view_*` draw through `origami-render` (wgpu, Vulkan on Linux and Windows, Metal on macOS). |

`cargo build -p origami-ffi --features fake` gives `liborigami_ffi.{so,dylib}` /
`origami_ffi.dll` and the static library. The crate is a workspace member, not a
default member (it pulls wgpu and uniffi); select it with `-p`.

## Threading

Every call may be made from any thread. Fast calls (`view`, `send_*`,
`console_*`, `origami_frame_*`, `origami_input_*`) are safe on a UI thread.
Long calls (`start`, `stop`, `create_machine`, `start_machine`) return an
`OrigamiOperation`, except `start_machine`, which blocks. The observer is
called on a core-owned thread with no lock held; hop to your UI thread inside
it. Events are level triggered and coalesced: they say *what* changed on a
machine (`state`, `display`, `console`, `operation`, `library`); pull the new
state. `OrigamiCore.shutdown()` waits for a callback in flight, so do not call
it under a lock the observer takes.

## Control plane

`OrigamiCore(libraryRoot, observer?)` throws `CoreError`.

- `engines()`, `catalogue()` (offerings you can create), `list_machines()`
  (state `LeftRunning` when another process's runner holds the machine;
  `start_machine` attaches),
  `commands()` (menu table: id, title, `enabled_when`, shortcut per platform;
  `origami_command_enabled(when, state)`).
- `create_machine(OrigamiCreateRequest) -> OrigamiOperation`; its
  `result_machine()` is the new key. `engine` names the engine that makes the
  machine (an `OrigamiEngineInfo.id`); without it the first engine that lists
  the offering does, and an unknown one fails the operation with `NotFound`.
- `open_machine(key) -> OrigamiMachine` (same machine, same object state);
  `start_machine(key)` opens, starts and waits.
- `hinv()`: the About egg's text, an IRIX-style inventory of the host and library.
  Reads the disk and may open the GPU: call off the UI thread.
- `shutdown()`.

`OrigamiMachine`: `key()`, `name()`, `view()` (state `NotRunning | Starting |
Running | Paused | Stopping | Exited`, exit code and last error, `heads`,
`console`, `operation`, and once exited `exit_reason`: `GuestPoweredOff |
HostStopped | Crashed | RunnerError`, and `exit_detail`), `start()`, `stop(Clean | Force)` (a clean stop cannot
be cancelled and can take minutes), `control(Reset | PowerButton | Nmi | Pause |
Resume)`, `change_media(drive, image?)`, `screendump(head, path)`,
`settings()` (the machine file: processor, CPUs and memory with the values the
offering allows, graphics pipes, drives, network mode, port forwards and
shares), `apply_change(OrigamiChange)` (`SetProcessor | SetMemory | SetNetwork
| AddForward | RemoveForward | AddShare | RemoveShare | DetachDrive`; `Busy`
unless the machine is stopped, so read `settings()` again after it),
`send_key(OrigamiKey, down)`, `send_pointer(Move | At | Button | Wheel)`,
`release_all_keys()` (call when your window loses focus; returns how many it
lifted), `console_read(from, max)` (offset ring: pass `next` back; `dropped`
says what you missed), `console_send(bytes)`, `frame_plane_handle()`.

`OrigamiOperation`: `id()`, `title()`, `state()` (`Running | Succeeded |
Failed(kind, message) | Cancelled`), `wait(timeout_ms?)` (blocks), `cancel()`
(false when it cannot be), `result_machine()`.

`OrigamiTerminal(machine, rows, cols)` is the machine's console as a VT
terminal; a pump thread feeds it, so the app only draws. `row(i)` gives cells
(text, colours, attributes), `cursor()`, `scrollback_count()` and
`scrollback_row(i)` (0 is oldest), `scroll(lines)` moves the view,
`changed_since(sequence)` names the rows to redraw (or `full`), `resize`,
`send_key(key, ctrl, alt)` (`send_key_with` adds shift) and `paste(text)`.
Backspace sends ^H, or DEL once the guest prints a Linux or OpenBSD banner.
Grids clamp to 256x512.

`CoreError` has one variant per kind, each with `message` (a sentence safe to
show) and `detail`: `NotFound`, `Invalid`, `Busy`, `NotRunning`, `Unsupported`,
`NeedsUpgrade`, `Io`, `Protocol`, `Engine`, `Cancelled`, `Internal`. In C# the
type is `CoreException` with nested classes.

Generated code notes: Swift `OrigamiObserver.onEvent(event:)`; C# types are
`internal` in namespace `uniffi.origami_ffi` (compile the file into your app)
and the library name is `origami_ffi`.

## Frame plane

All functions return `origami_status` (`ORIGAMI_OK` = 0, negative
`ORIGAMI_ERR_*`) and catch panics; `origami_last_error` has the text.

- Machine: `origami_machine_release`; for plain C also `origami_core_new/
  release/machines`, `origami_machine_open/start`.
- Frames: `origami_frame_heads`, `origami_frame_take` (the copy path, into your
  buffer; `BUFFER_TOO_SMALL` loses nothing), `origami_frame_cursor`.
- Wake-up without polling: `origami_frame_set_wake(machine, cb, user_data)`,
  and `origami_frame_wake_handle(machine, &h)`: an eventfd (a pipe on macOS)
  to `poll`/`kqueue`, or an auto-reset event `HANDLE` on Windows. Neither
  allocates per frame. On Unix call `origami_frame_wake_clear` to re-arm.
- Input: `origami_input_send` (batch), `origami_input_release_all`. Key codes
  are `origami_key` (same order as `OrigamiKey`).
- Presenting: `origami_view_attach(machine, head, &surface, w, h, scale, &view)`,
  `_resize`, `_set_scale`, `_set_visible`, `_present`, `_detach`. Surfaces:
  Cocoa `NSView`/`CAMetalLayer`, Win32 `HWND`, X11 (Xlib or XCB), Wayland.
  A Win32 `HWND` swap chain is opaque; a WinUI `SwapChainPanel` or a
  DirectComposition visual blends (the cube asks for premultiplied alpha).
  Detach the view before its window goes.

- Cube: `origami_cube_attach(&surface, w, h, scale, &cube)`, `_resize`,
  `_set_visible`, `_set_style` (environment, scene, horizon colour, backdrop,
  granite tones, calm spin, reduce motion), `_pointer` (press, move, release, double
  click), `_next_scene` (step through the scenes in time-of-day order with a
  crossfade), `_current_scene` and `_scene_for_time(hour, minute)` (About's
  scene for the local time), `_set_machine` (reflect a machine's screen),
  `_present`, `_readback`, `_stats`, `_detach`, and `origami_cube_scene_count/
  text/find` and `origami_cube_scene_for_time` for the scene list and its
  credits. The host owns the clock: call `_present` on each display refresh
  while it reports `moving`, stop when it does not, and call it once after any
  other call. The backdrop is none by default (the view is transparent around the cube,
  over the host's non-opaque surface) or the granite tile.
  The machine's screen is copied by the view presenting that head, so it needs
  a presenting `origami_view`.

A typical render loop: wait on the wake handle (or post from the callback),
then `origami_view_present`; or, on the copy path, `origami_frame_take` and
upload.

A QEMU machine runs in the `origami` runner (`ORIGAMI_BIN`, else beside the
library) and this process attaches to its shared-memory display as a viewer,
so a machine outlives the app. On first use the core opens the process's one
GPU and imports the guest's picture into it; `take_frame` then reports
`Unsupported` ("present it with `origami_view_present`") because the rows are
in GPU memory. Without a GPU that imports (or with `ORIGAMI_GUI_ZERO_COPY=0`)
the rows are copied and `origami_frame_take` works.

Per platform, with no CPU copy of the picture:

- Linux: QEMU exports its scanout memory as a Vulkan buffer and the device
  imports the descriptor. Run on an RTX 4090.
- Windows: Direct3D 12 opens QEMU's shared-memory section as a heap
  (`OpenExistingHeapFromFileMapping`). Cross-compiled only, never run.
- macOS: Metal wraps the shared mapping (`newBufferWithBytesNoCopy`). Not
  rebuilt or run since the move into the core.

`origami_view_present` also draws the guest's cursor, when the engine does not
draw it into the picture. `origami_view_set_visible(0)` tells the display
client too: a hidden machine is sent no wake-ups, and showing it wakes once if
it changed meanwhile. `origami_frame_stats` (pixel bytes the CPU copied, lines
imported) and `origami_view_stats` (nanoseconds taking the frame, drawing it,
and in the window system's present) let tests hold the budgets.
`origami_view_set_scale_mode(view, ORIGAMI_SCALE_INTEGER | ORIGAMI_SCALE_FIT)`
picks how the picture fills the window. Integer draws whole-number scales,
and fits only when 1x is too big. Fit draws the biggest aspect-preserving
picture: whole device pixels with the nearest filter while that is at least
one device pixel per guest pixel, else a fractional fill with a smooth
filter. `origami_view_get_layout` returns the picture's rectangle and scale in
window pixels, to map the pointer.
`origami_view_readback` copies what the view last drew (cursor included, also
for an occluded window) to the caller as BGRX rows, for self-tests on every
platform; it waits for the GPU, so keep it off the frame path.

Input goes to QEMU on the display socket as typed events.
`OrigamiMachine` `control` (reset, power, NMI, pause, resume) and `screendump`
use QMP; `change_media` is not available on a runner machine.

## Bindings tests

`bindings/c`, `bindings/swift`, `bindings/csharp`: each has a program and a
script (`run.sh`; `gen.sh` only generates). They list the fake machines, start
one and receive an event. CI: `.github/workflows/bindings.yml`. The Swift and
C# programs build only on macOS and Windows; Linux CI generates both.
`uniffi-bindgen-cs` is pinned to 0.12.0 (uniffi 0.32); install it with
`cargo install uniffi-bindgen-cs --git https://github.com/NordSecurity/uniffi-bindgen-cs --tag v0.12.0+v0.32.0 --locked`.
