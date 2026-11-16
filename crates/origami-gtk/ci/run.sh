#!/bin/sh
# usage: run.sh SCRATCH OUTNAME MODE [gpu=N|none] [-- app args]
# Runs one session in the spike run image, capped at 16g, mounting only SCRATCH and the QEMU build.
# SCRATCH holds target/ (cargo output), rt/ (the QEMU runtime tree), cache/ (PROMs), work/, out/.
S=$1; NAME=$2; MODE=$3; GPU=${4:-none}; shift 4 2>/dev/null || shift $#
[ "$1" = -- ] && shift
W=$(cd "$(dirname "$0")" && pwd)
GPUARG=""
ENVARG="-e VK_DRIVER_FILES=/usr/share/vulkan/icd.d/lvp_icd.json"
case $GPU in
  gpu=*) GPUARG="--device nvidia.com/gpu=${GPU#gpu=} --group-add keep-groups -e NVIDIA_RUN=1"; ENVARG="-e VK_DRIVER_FILES=/etc/vulkan/icd.d/nvidia_icd.json" ;;
esac
mkdir -p "$S/out/$NAME" "$S/work/$NAME"
exec podman run --rm --pids-limit=-1 --network host --memory 16g $GPUARG $ENVARG \
  -v "$S":"$S" -v "$W":/ci:ro -v /dev/shm:/dev/shm \
  -e APP="$S/target/release/origami-gtk" -e VPTR="$S/target/release/vptr" -e WORK="$S/work/$NAME" -e ORIGAMI_BIN="$S/target/release/origami" \
  -e ORIGAMI_RUNTIME_DIR="$S/rt/libexec/origami" -e ORIGAMI_CACHE_DIR="$S/cache" \
  -e GDK_DEBUG="${GDK_DEBUG:-}" -e GSK_RENDERER="${GSK_RENDERER:-}" -e GTK_A11Y=none -e RUST_BACKTRACE=1 \
  localhost/app-linux-run:1 /ci/session.sh "$S/out/$NAME" "$MODE" "$@"
