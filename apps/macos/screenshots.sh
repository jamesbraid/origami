#!/bin/zsh
# Builds the app on the QEMU engine and runs its --docs-shots mode: apps/macos/screenshots.sh OUTDIR
# leaves OUTDIR/macos-*.png. Fetches the pinned QEMU and builds the runner; the PROM downloads on first use.
set -eu
here=${0:A:h}
root=${here:h:h}
out=${1:?usage: screenshots.sh OUTDIR}
work=${out:A}.work
mkdir -p $out
cd $root

export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-$root/target}
cargo xtask runtime --platform macos-arm64
cargo build --release --locked --bin origami
product=$work/product
rm -rf $product; mkdir -p $product/bin
tree=$CARGO_TARGET_DIR/runtime/macos-arm64/tree
cp -R $tree/libexec $product/libexec
[[ -d $tree/share ]] && cp -R $tree/share $product/share
cp $CARGO_TARGET_DIR/release/origami $product/bin/origami

APP_OUT=$work/Origami.app ORIGAMI_PRODUCT=$product $here/build.sh

rm -rf $work/machines; mkdir -p $work/machines
$work/Origami.app/Contents/MacOS/OrigamiApp --docs-shots $work/machines $out
