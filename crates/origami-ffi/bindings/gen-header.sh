#!/bin/sh
# Regenerate include/origami_frame.h. Needs cbindgen 0.29.4:
#   cargo install cbindgen --version 0.29.4 --locked
set -eu
cd "$(dirname "$0")/.."
cbindgen --config cbindgen.toml --crate origami-ffi --lockfile ../../Cargo.lock \
  --output include/origami_frame.h
