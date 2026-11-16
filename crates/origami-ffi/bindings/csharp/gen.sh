#!/bin/sh
# Generate the C# bindings (origami_ffi.cs) from the built library with
# uniffi-bindgen-cs 0.12.0 (targets uniffi 0.32):
#   cargo install uniffi-bindgen-cs --git https://github.com/NordSecurity/uniffi-bindgen-cs \
#     --tag v0.12.0+v0.32.0 --locked
# Usage: gen.sh OUT_DIR [CARGO_TARGET_DIR]
set -eu
here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../../../.." && pwd)
out=${1:?usage: gen.sh OUT_DIR [CARGO_TARGET_DIR]}
target=${2:-${CARGO_TARGET_DIR:-$repo/target}}
cargo build --manifest-path "$repo/Cargo.toml" -p origami-ffi --features fake --lib
case $(uname -s) in
  Darwin) lib=$target/debug/liborigami_ffi.dylib ;;
  MINGW*|MSYS*|CYGWIN*) lib=$target/debug/origami_ffi.dll ;;
  *) lib=$target/debug/liborigami_ffi.so ;;
esac
uniffi-bindgen-cs --library "$lib" --out-dir "$out"
