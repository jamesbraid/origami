#!/bin/zsh
# The macOS app's nightly check, one command for the workflow to call: build
# the app on the core's fake engine and run its headless self-test. No window
# is shown; then a second pass shows the real window and checks what it draws
# against the core's behaviour table.
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
ORIGAMI_FIXTURES=$root/tests/fixtures/machines $work/Origami.app/Contents/MacOS/OrigamiApp \
    --selftest $work/machines $work/out | tee $work/out/selftest.jsonl
exit_code=${pipestatus[1]}
set -e
echo "self-test exit status: $exit_code"

# The window is on screen for this one: it needs a window server, so it has its own library and output.
rm -rf $work/display-machines $work/display-out; mkdir -p $work/display-machines $work/display-out
set +e
$work/Origami.app/Contents/MacOS/OrigamiApp --selftest-display $work/display-machines $work/display-out \
    | tee $work/out/selftest-display.jsonl
display_code=${pipestatus[1]}
set -e
cp -R $work/display-out/. $work/out/ 2>/dev/null || true
echo "display self-test exit status: $display_code"
(( exit_code == 0 && display_code == 0 ))
