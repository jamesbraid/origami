#!/bin/sh
# The Linux app's pull-request check, in one command: build the images, build
# and unit-test the crate, fetch the QEMU runtime the product pins, and run
# the named scenarios on lavapipe under Xvfb (a real-GPU run adds gpu=N).
#
# usage: linux.sh SCRATCH [scenario...]     (default: first-light settings)
# SCRATCH is a directory this script may fill (cargo target, runtime, outputs).
set -eu
S=$(cd "$1" && pwd); shift
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../../.." && pwd)
SCEN=${*:-first-light settings}
mkdir -p "$S/ctx" "$S/cargo" "$S/target" "$S/cache" "$S/work" "$S/out"
cp "$HERE/Containerfile" "$HERE/packages.txt" "$S/ctx/"
podman build --memory 16g -t localhost/app-linux-build:1 -t localhost/app-linux-run:1 -f "$S/ctx/Containerfile" "$S/ctx"
sh "$HERE/build.sh" "$S" build --release -p origami-gtk -p origami --features origami-gtk/iris,origami/iris
sh "$HERE/build.sh" "$S" build --release -p origami-gtk --features test-tools --bin vptr
sh "$HERE/build.sh" "$S" test -p origami-gtk
# The runtime tree (QEMU, keymaps) from the pinned release; xtask lays it out.
sh "$HERE/build.sh" "$S" xtask runtime --platform linux-x86_64
rm -rf "$S/rt" && mkdir -p "$S/rt"
cp -rL "$S/target/runtime/linux-x86_64/tree/libexec" "$S/target/runtime/linux-x86_64/tree/share" "$S/rt/"
for sc in $SCEN; do
  rm -rf "$S/work/$sc" "$S/out/$sc"
  sh "$HERE/run.sh" "$S" "$sc" x11 none -- "$sc"
done
# Capture under Wayland: sway has pointer constraints and a virtual pointer.
rm -rf "$S/work/pointer-sway" "$S/out/pointer-sway"
sh "$HERE/run.sh" "$S" pointer-sway sway none -- pointer
