# ghostty-vt-sys

Hand-written FFI to the stable C ABI of libghostty-vt 1.0 (MIT), the terminal
engine behind `origami-terminal`. Pinned release: 1.0.1. Layouts are asserted
by a unit test; the declarations follow `ghostty/vt/*.h` of that release.

`build.rs` finds the library, so a plain `cargo build` works:

| platform | library | how |
| --- | --- | --- |
| Linux | `libghostty-vt.a`, static | built once from upstream's source and dependency tarballs with Zig 0.17.1 (`ci/build-lib.sh`); the build script fetches the pinned Zig itself unless `zig` is on `PATH` |
| macOS | `libghostty-vt.a`, static (universal) | the XCFramework from upstream's release |
| Windows | `ghostty-vt.lib` (import library) and `ghostty-vt.dll` | upstream's `x86_64-windows-gnu` (or `aarch64`) zip; links with mingw |

Everything is fetched by `curl` from `release.files.ghostty.org` (and
`ziglang.org` for Zig) and checked against SHA-256 values pinned in `build.rs`
and `ci/build-lib.sh`; the Linux build also checks upstream's minisign
signature when `minisign` is installed. The result is cached in
`<target dir>/ghostty-vt/<version>-<os>-<arch>`, shared by profiles. The first Linux
build takes about five minutes; nothing else needs Zig.

`GHOSTTY_VT_LIB_DIR` (the directory holding the library) or `GHOSTTY_VT_DIR`
(a prefix with `lib/`) skips all of that, for an offline build or another
library build.

`ci/Containerfile` is a Zig 0.17.1 image for building the library by hand:

    podman run --rm --memory 8g -v $OUT:/out -v $PWD/ci:/ci:ro IMAGE /ci/build-lib.sh /out

## Windows

The DLL is used rather than the static library. Upstream's static Windows
library defines `___chkstk_ms` weakly, mingw's linker prefers that over
libgcc's definition and the link fails with relocation overflows. The DLL
imports only `ntdll`, `kernel32` and the universal C runtime. It must ship
next to `origami_ffi.dll` (the packaging steps copy it from
`<target dir>/ghostty-vt/1.0.1-windows-x86_64/`).

## Pins

| file | SHA-256 |
| --- | --- |
| `libghostty-vt-1.0.1.tar.gz` | `333dfce97641ef9b536bb9e08907ad86abaaada50e8f7417b70d9284adef37aa` |
| `libghostty-vt-1.0.1-deps.tar.gz` | `91a4e10960a52bf5bf4b15aa2a2cddaff8a89d6cd903d3055bda4fd17de3356d` |
| `ghostty-vt-1.0.1.xcframework.zip` | `c0f9631277f726154c8f521954d17cdc41aac4626fc0df8cf925a19ee58e829b` |
| `ghostty-vt-1.0.1-x86_64-windows-gnu.zip` | `8e2eb9e1822e85d786b456c8beb86f49d09c318c91f0c2913f91e7ffa12cda29` |
| `ghostty-vt-1.0.1-aarch64-windows-gnu.zip` | `cc61da5f968ea0b08639bdf019c6b7da46bad1d985774fd8a2de57af32862892` |
| `zig-x86_64-linux-0.17.1.tar.xz` | `3581863e3cbcfd0fce0efc682ce7e9e93da96668a977791d356a26d2a7e9b17a` |
| `zig-aarch64-linux-0.17.1.tar.xz` | `a1546d5646eb184437918d6635ca6b762a0e889eac09cef720ee7a1ed3fe7795` |
