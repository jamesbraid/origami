#!/bin/sh
set -eu

if [ "$#" -ne 3 ]; then
    printf 'usage: %s QEMU-SOURCE INSTIGATOR-SOURCE SCRATCH-DIR\n' "$0" >&2
    exit 2
fi

product=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
source=$(CDPATH= cd -- "$1" && pwd)
instigator=$(CDPATH= cd -- "$2" && pwd)
scratch=$(CDPATH= cd -- "$3" && pwd)
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
instigator_expected=$(cat "$product/build/instigator-revision")
instigator_actual=$(git -C "$instigator" rev-parse HEAD)
if [ "$instigator_actual" != "$instigator_expected" ]; then
    printf 'Instigator source is %s; product expects %s\n' "$instigator_actual" "$instigator_expected" >&2
    exit 2
fi
if ! git -C "$instigator" diff --quiet || ! git -C "$instigator" diff --cached --quiet; then
    printf 'Instigator source has tracked changes; use a clean checkout at %s\n' "$instigator_expected" >&2
    exit 2
fi

"$engine" build -t localhost/sgi-qemu-builder:dev \
    -f "$product/build/Qemu.Containerfile" "$product/build"
"$engine" build -t localhost/sgi-rust-builder:dev \
    -f "$product/build/Containerfile" "$product/build"
"$engine" build -t localhost/sgi-instigator-builder:dev \
    -f "$product/build/Instigator.Containerfile" "$product/build"

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

"$engine" run --rm -v "$instigator:/src:ro" -v "$scratch:/work" \
    -w /src -e GOMODCACHE=/work/go-mod -e GOCACHE=/work/go-build \
    -e GOTOOLCHAIN=local -e CGO_ENABLED=0 \
    localhost/sgi-instigator-builder:dev sh -ec '
        go test -mod=readonly ./internal/qemunet ./cmd/instigator
        go build -mod=readonly -trimpath -o /work/instigator ./cmd/instigator
        test -s /work/instigator
    '

"$engine" run --rm -v "$scratch:/work" -v "$instigator:/instigator:ro" \
    -v "$source:/qemu:ro" \
    localhost/sgi-qemu-builder:dev sh -ec '
        mkdir -p /work/linux-dev/bin /work/linux-dev/libexec/sgi \
            /work/linux-dev/share/sgi/licenses
        rm -f /work/linux-dev/bin/qemu-system-mips64 /work/linux-dev/bin/qemu-img
        cp /work/target/release/sgi /work/linux-dev/bin/
        test -s /work/instigator
        cp /work/instigator /work/linux-dev/bin/
        cp /work/qemu-build/qemu-system-mips64 /work/qemu-build/qemu-img \
            /work/linux-dev/libexec/sgi/
        cp /instigator/LICENSE /work/linux-dev/share/sgi/licenses/instigator.LICENSE
        cp /qemu/LICENSE /work/linux-dev/share/sgi/licenses/qemu.LICENSE
        cp /qemu/COPYING /work/linux-dev/share/sgi/licenses/qemu.COPYING
        cp /qemu/COPYING.LIB /work/linux-dev/share/sgi/licenses/qemu.COPYING.LIB
        /work/linux-dev/libexec/sgi/qemu-system-mips64 -display help | grep -qx sdl
        cd /work/linux-dev
        sha256sum bin/sgi bin/instigator libexec/sgi/qemu-system-mips64 \
            libexec/sgi/qemu-img > SHA256SUMS
    '
printf 'development bundle: %s/linux-dev\n' "$scratch"
