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
# Started through LaunchServices so the app is frontmost: its windows then draw as active (coloured traffic lights).
# A capture needs this script's screen permission, so the app asks for each by file and the loop below takes it.
rm -f $work/stderr.log $out/want-*.txt(N) $out/got-*.txt(N) $out/done.txt
(
    while [[ ! -e $out/done.txt ]]; do
        for f in $out/want-*.txt(N); do
            name=${${f:t:r}#want-}
            /usr/sbin/screencapture -x -o -l$(<$f) $out/$name.png
            rm -f $f; touch $out/got-$name.txt
        done
        sleep 0.3
    done
) &
loop=$!
open -W -n --stdout $work/stdout.log --stderr $work/stderr.log $work/Origami.app --args --docs-shots $work/machines $out
touch $out/done.txt
wait $loop
cat $work/stdout.log $work/stderr.log
