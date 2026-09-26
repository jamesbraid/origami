#!/bin/sh
set -eu

if [ "$#" -ne 2 ]; then
    printf 'usage: %s QEMU-SOURCE SCRATCH-DIR\n' "$0" >&2
    exit 2
fi

product=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
source=$(CDPATH= cd -- "$1" && pwd)
scratch=$(CDPATH= cd -- "$2" && pwd)
engine=${SGI_CONTAINER_ENGINE:-podman}
expected=$(cat "$product/build/qemu-revision")
actual=$(git -C "$source" rev-parse HEAD)
if [ "$actual" != "$expected" ]; then
    printf 'QEMU source is %s; product expects %s\n' "$actual" "$expected" >&2
    exit 2
fi
if ! git -C "$source" diff --quiet || ! git -C "$source" diff --cached --quiet; then
    printf 'QEMU source has tracked changes; use a clean checkout at %s\n' "$expected" >&2
    exit 2
fi

"$engine" build -t localhost/sgi-qemu-builder:dev \
    -f "$product/build/Qemu.Containerfile" "$product/build"
"$engine" build -t localhost/sgi-rust-builder:dev \
    -f "$product/build/Containerfile" "$product/build"

"$engine" run --rm -v "$source:/src:ro" -v "$scratch:/work" \
    -w /work localhost/sgi-qemu-builder:dev sh -ec '
        mkdir -p qemu-build
        cd qemu-build
        if [ ! -f build.ninja ]; then
            /src/configure --target-list=mips64-softmmu --disable-docs \
                --enable-sdl --enable-vnc --enable-slirp --disable-gtk
        fi
        ninja -j4 qemu-system-mips64 qemu-img
        ./qemu-system-mips64 -display help | grep -qx sdl
        ./qemu-system-mips64 -help | grep -q -- "^-vnc "
    '

"$engine" run --rm -v "$product:/src:ro" -v "$scratch:/work" \
    -w /src -e CARGO_TARGET_DIR=/work/target -e CARGO_HOME=/work/cargo \
    localhost/sgi-rust-builder:dev sh -ec 'cargo test --locked && cargo build --locked --release'

"$engine" run --rm -v "$scratch:/work" \
    localhost/sgi-qemu-builder:dev sh -ec '
        mkdir -p /work/linux-dev/bin /work/linux-dev/libexec/sgi
        rm -f /work/linux-dev/bin/qemu-system-mips64 /work/linux-dev/bin/qemu-img
        cp /work/target/release/sgi /work/linux-dev/bin/
        cp /work/qemu-build/qemu-system-mips64 /work/qemu-build/qemu-img \
            /work/linux-dev/libexec/sgi/
        /work/linux-dev/libexec/sgi/qemu-system-mips64 -display help | grep -qx sdl
    '
printf 'development bundle: %s/linux-dev\n' "$scratch"
