#!/bin/zsh
# Builds the app on the fake engine and runs its --docs-shots mode: apps/macos/screenshots.sh OUTDIR
# leaves OUTDIR/macos-*.png.
set -eu
here=${0:A:h}
root=${here:h:h}
out=${1:?usage: screenshots.sh OUTDIR}
work=${out:A}.work
mkdir -p $out
cd $root

APP_OUT=$work/Origami.app ORIGAMI_FFI_FEATURES=fake $here/build.sh

rm -rf $work/machines; mkdir -p $work/machines
$work/Origami.app/Contents/MacOS/OrigamiApp --docs-shots $work/machines $out
