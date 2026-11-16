# Origami for macOS

Native SwiftUI for the library, settings and windows; AppKit for the machine's
screen, which the core's renderer draws into our `CAMetalLayer`. Built with the
command-line tools alone (`swiftc`, `cargo`), no Xcode project. It binds
`origami-ffi`: UniFFI for machines, operations and events, and the C frame
plane for the wake-up, input and drawing (see `crates/origami-ffi/README.md`).

    apps/macos/build.sh                       # build/Origami.app
    build/Origami.app/Contents/MacOS/OrigamiApp --machines DIR
    ORIGAMI_FFI_FEATURES=fake apps/macos/build.sh
    build/Origami.app/Contents/MacOS/OrigamiApp --selftest DIR OUT

`--selftest` shows no window and prints one JSON line per step; it exits
non-zero if any step failed. Built with the fake engine it checks events,
frames, drawing into the layer, input, both pointer modes and power. Built
for QEMU it starts the first machine and says whether the display is wired.
`ci.sh` runs the fake build nightly on GitHub.

## Layout

- `Sources/Core`: the models the views observe, the command table, and
  `CoreHost.swift`, the only file that calls the core's frame functions.
- `Sources/Display`: the AppKit display view (drawing requests, keyboard,
  follow-the-mouse and capture).
- `Sources/Views`, `Sources/App.swift`: the SwiftUI app, menus built from the
  command table, and the Origami identity (Indigo, chassis colours, granite,
  bezel, chrome cube).
- `Sources/Selftest`: the headless self-test.

Not done: New Machine, Console, CD change and settings editing wait for the
core; the identity artwork (granite, cube) is placeholder until its design lands.
Verified headless on an M2 Mac (see the pull request); a visible window, menus
and real input are not.
