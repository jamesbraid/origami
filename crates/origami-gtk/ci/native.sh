#!/bin/sh
# The Forgejo job's steps, run directly as root in a Debian trixie (or newer)
# container: install packages and Rust, lint, build, unit-test, fetch the pinned
# QEMU runtime and run the scenarios on lavapipe under Xvfb, then the pointer
# scenario under sway. The job's runner has no podman, which is why this
# exists beside linux.sh.
#
# usage: native.sh SCRATCH [scenario...]     (default: first-light settings popout)
# SHOTS_ONLY=1 skips the lint, the unit tests and the pointer scenario: the
# screenshots workflow wants the build and the named scenario alone.
set -eu
S=$(mkdir -p "$1" && cd "$1" && pwd); shift
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../../.." && pwd)
SCEN=${*:-first-light settings layout upgrade popout sidebar diagnostics console headless hidpi}
mkdir -p "$S/cache" "$S/work" "$S/out"

export DEBIAN_FRONTEND=noninteractive
apt-get update
apt-get install -y --no-install-recommends $(grep -v '^#' "$HERE/packages.txt")
if ! command -v cargo >/dev/null 2>&1; then
  curl --fail --show-error --silent --proto '=https' --tlsv1.2 https://sh.rustup.rs -o "$S/rustup-init.sh"
  sh "$S/rustup-init.sh" -y --profile minimal --component clippy --default-toolchain stable
  PATH="$HOME/.cargo/bin:$PATH"
fi
export CARGO_TARGET_DIR="$S/target"
cd "$ROOT"
[ -n "${SHOTS_ONLY:-}" ] || cargo clippy --locked -p origami-gtk --features iris --all-targets -- -D warnings
# iris is hosted by the origami runner, so both are built with it.
cargo build --locked --release -p origami-gtk -p origami --features origami-gtk/iris,origami-gtk/selftest,origami/iris
cargo build --locked --release -p origami-gtk --features test-tools --bin vptr
[ -n "${SHOTS_ONLY:-}" ] || cargo test --locked -p origami-gtk -p origami-terminal
# The renderer's GPU tests, on lavapipe; elsewhere they skip without an adapter.
[ -n "${SHOTS_ONLY:-}" ] || VK_DRIVER_FILES=/usr/share/vulkan/icd.d/lvp_icd.json ORIGAMI_REQUIRE_GPU=1 cargo test --locked -p origami-render
cargo xtask runtime --platform linux-x86_64
rm -rf "$S/rt" && mkdir -p "$S/rt"
cp -rL "$S/target/runtime/linux-x86_64/tree/libexec" "$S/target/runtime/linux-x86_64/tree/share" "$S/rt/"

run() { # NAME MODE SCENARIO
  rm -rf "$S/work/$1" "$S/out/$1"
  mkdir -p "$S/work/$1" "$S/out/$1"
  VK_DRIVER_FILES=/usr/share/vulkan/icd.d/lvp_icd.json GTK_A11Y=none RUST_BACKTRACE=1 \
  APP="$S/target/release/origami-gtk" VPTR="$S/target/release/vptr" WORK="$S/work/$1" \
  ORIGAMI_BIN="$S/target/release/origami" ORIGAMI_RUNTIME_DIR="$S/rt/libexec/origami" ORIGAMI_CACHE_DIR="$S/cache" \
    sh "$HERE/session.sh" "$S/out/$1" "$2" "$3"
}
for sc in $SCEN; do run "$sc" x11 "$sc"; done
[ -n "${SHOTS_ONLY:-}" ] || [ -n "${*:-}" ] || GDK_SCALE=2 XVFB_SCREEN=3200x2600 run hidpi-2x x11 hidpi
# The behaviour table at 1x and 2x, on a screen big enough for its larger rows.
[ -n "${SHOTS_ONLY:-}" ] || [ -n "${*:-}" ] || XVFB_SCREEN=3200x2400 run behaviour x11 behaviour
[ -n "${SHOTS_ONLY:-}" ] || [ -n "${*:-}" ] || GDK_SCALE=2 XVFB_SCREEN=4200x3000 run behaviour-2x x11 behaviour
[ -n "${SHOTS_ONLY:-}" ] || run pointer-sway sway pointer
