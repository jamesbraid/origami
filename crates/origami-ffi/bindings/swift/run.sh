#!/bin/sh
# macOS only: generate the bindings, compile main.swift against them and the
# C frame plane's module, and run it.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../../../.." && pwd)
target=${CARGO_TARGET_DIR:-$repo/target}
out=$(mktemp -d)
trap 'rm -rf "$out"' EXIT
"$here/gen.sh" "$out" "$target"
swiftc -o "$out/bindings-test" "$here/main.swift" "$out/origami_ffi.swift" \
  -Xcc -fmodule-map-file="$out/origami_ffiFFI.modulemap" \
  -Xcc -fmodule-map-file="$here/../../include/module.modulemap" \
  -I "$here/../../include" -L "$target/debug" -lorigami_ffi
DYLD_LIBRARY_PATH="$target/debug" "$out/bindings-test"
