#!/bin/sh
# Generate the Swift bindings (origami_ffi.swift, origami_ffiFFI.h and its
# modulemap) from the built library. Runs on Linux and macOS.
# Usage: gen.sh OUT_DIR [CARGO_TARGET_DIR]
set -eu
here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../../../.." && pwd)
out=${1:?usage: gen.sh OUT_DIR [CARGO_TARGET_DIR]}
target=${2:-${CARGO_TARGET_DIR:-$repo/target}}
cargo build --manifest-path "$repo/Cargo.toml" -p origami-ffi --features fake --lib
case $(uname -s) in
  Darwin) lib=$target/debug/liborigami_ffi.dylib ;;
  *) lib=$target/debug/liborigami_ffi.so ;;
esac
cargo run --manifest-path "$repo/Cargo.toml" -q -p origami-ffi --features fake \
  --bin uniffi-bindgen -- generate --library "$lib" --language swift --out-dir "$out"
