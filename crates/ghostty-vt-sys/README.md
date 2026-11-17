# ghostty-vt-sys

Hand-written FFI to the stable C ABI of libghostty-vt 1.0 (MIT), the terminal
engine behind `origami-terminal`'s `ghostty` feature. Pinned release: 1.0.1.
Layouts are asserted by a unit test; the declarations follow
`ghostty/vt/*.h` of that release.

Nothing links unless `GHOSTTY_VT_LIB_DIR` (the directory holding the library)
or `GHOSTTY_VT_DIR` (a prefix with `lib/`) is set, so the default workspace
build never needs the library or Zig.

## Linux

Zig 0.17.1 is needed only to build the library, and only inside a container:

    podman build --memory 8g -t localhost/ghostty-vt-build:1 ci
    podman run --rm --memory 8g -v $OUT:/out -v $PWD/ci:/ci:ro \
      localhost/ghostty-vt-build:1 /ci/build-lib.sh /out
    GHOSTTY_VT_DIR=$OUT cargo test -p origami-terminal --features ghostty

`ci/build-lib.sh` downloads the release and dependency tarballs from
`release.files.ghostty.org`, checks their SHA-256 against the values pinned in
the script and the upstream minisign signature (key
`RWQlAjJC23149WL2sEpT/l0QKy7hMIFhYdQOFy0Z7z7PbneUgvlsnYcV`), then builds with
an offline dependency set. The Containerfile pins the Zig tarball by SHA-256.
The result is a static `libghostty-vt.a`; it needs only glibc (2.36 or newer,
for `arc4random_buf`) and libgcc_s.

## macOS and Windows

Zig is not needed there: use upstream's prebuilt files from
`https://release.files.ghostty.org/libghostty-vt/1.0.1/`, which carry the same
MIT licence. Check each against `SHA256SUMS` (itself signed) and its
`.minisig`. Not built or linked from this repository's CI yet.

| file | SHA-256 |
| --- | --- |
| `ghostty-vt-1.0.1.xcframework.zip` | `c0f9631277f726154c8f521954d17cdc41aac4626fc0df8cf925a19ee58e829b` |
| `ghostty-vt-1.0.1-x86_64-windows-gnu.zip` | `8e2eb9e1822e85d786b456c8beb86f49d09c318c91f0c2913f91e7ffa12cda29` |
| `ghostty-vt-1.0.1-aarch64-windows-gnu.zip` | `cc61da5f968ea0b08639bdf019c6b7da46bad1d985774fd8a2de57af32862892` |

- macOS: unpack the XCFramework and set `GHOSTTY_VT_LIB_DIR` to
  `ghostty-vt.xcframework/macos-arm64_x86_64` (a universal static
  `libghostty-vt.a`).
- Windows: unpack the zip and set `GHOSTTY_VT_DIR` to its root. The build
  links the import library `lib/ghostty-vt.lib`, and `bin/ghostty-vt.dll`
  must ship next to the executable.
