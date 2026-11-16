#!/bin/sh
# Build liborigami_ffi with the fake engine, compile test.c against the
# checked-in header, and run it. Usage: run.sh [TARGET_DIR] (default: target).
set -eu
here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../../../.." && pwd)
target=${1:-${CARGO_TARGET_DIR:-$repo/target}}
cargo build -p origami-ffi --features fake --lib --manifest-path "$repo/Cargo.toml"
out=$(mktemp -d)
trap 'rm -rf "$out"' EXIT
${CC:-cc} -std=c11 -Wall -Wextra -Werror -I "$here/../../include" "$here/test.c" \
  -L "$target/debug" -lorigami_ffi -Wl,-rpath,"$target/debug" -o "$out/test"
"$out/test"
