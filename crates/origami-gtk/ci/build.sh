#!/bin/sh
# usage: build.sh SCRATCH [cargo args]   (cargo runs in the spike image; only the worktree and SCRATCH are mounted)
S=$1; shift
W=$(cd "$(dirname "$0")/../../.." && pwd)
exec podman run --rm --pids-limit=-1 --network host --memory 16g \
  -v "$W":/work -v "$S/cargo":/cargo -v "$S/target":/target -e CARGO_HOME=/cargo -e CARGO_TARGET_DIR=/target -w /work \
  localhost/app-linux-build:1 cargo "$@"
