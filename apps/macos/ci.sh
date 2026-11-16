#!/bin/zsh
# The macOS app's nightly check, one command for the workflow to call: build
# the app on the core's fake engine and run its headless self-test. No window
# is shown.
#
#   apps/macos/ci.sh [WORKDIR]
#
# Leaves WORKDIR/out/selftest.jsonl (one JSON line per step) and the pictures.
set -eu
here=${0:A:h}
root=${here:h:h}
work=${1:-${RUNNER_TEMP:-$(mktemp -d)}/origami-app-macos}
mkdir -p $work
cd $root

# The fake engine: it needs no firmware and no QEMU, and exercises the whole
# app path (events, the frame wake-up, the renderer, input, power).
APP_OUT=$work/Origami.app ORIGAMI_FFI_FEATURES=fake $here/build.sh

rm -rf $work/machines $work/out; mkdir -p $work/machines $work/out

set +e
$work/Origami.app/Contents/MacOS/OrigamiApp \
    --selftest $work/machines $work/out | tee $work/out/selftest.jsonl
status=${pipestatus[1]}
set -e
echo "self-test exit status: $status"
exit $status
