#!/bin/sh
set -eu

if [ "$#" -lt 3 ] || [ "$#" -gt 4 ]; then
    printf 'usage: %s QEMU-SOURCE INSTIGATOR-SOURCE SCRATCH-DIR [ARCHIVE-DIR]\n' "$0" >&2
    exit 2
fi

product=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
source=$(CDPATH= cd -- "$1" && pwd)
instigator=$(CDPATH= cd -- "$2" && pwd)
scratch=$(CDPATH= cd -- "$3" && pwd)
archive_dir=${4:-$scratch}
mkdir -p "$archive_dir"
archive_dir=$(CDPATH= cd -- "$archive_dir" && pwd)
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
product_revision=$(git -C "$product" rev-parse HEAD)
product_state=clean
if ! git -C "$product" diff --quiet || ! git -C "$product" diff --cached --quiet || \
    [ -n "$(git -C "$product" ls-files --others --exclude-standard)" ]; then
    product_state=dirty
fi
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

build_source=$(sh "$product/build/prepare-qemu-source.sh" "$source" "$scratch" "$expected")
"$engine" run --rm -v "$build_source:/src" -v "$scratch:/work" \
    -w /work -e MESON_PACKAGE_CACHE_DIR=/work/meson-package-cache \
    localhost/sgi-qemu-builder:dev sh -ec '
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
    -w /src -e CARGO_TARGET_DIR=/work/target-stable -e CARGO_HOME=/work/cargo \
    localhost/sgi-rust-builder:dev sh -ec 'cargo test --locked && cargo build --locked --release'

"$engine" run --rm -v "$instigator:/src:ro" -v "$scratch:/work" \
    -w /src -e GOMODCACHE=/work/go-mod -e GOCACHE=/work/go-build \
    -e GOTOOLCHAIN=local -e CGO_ENABLED=0 \
    localhost/sgi-instigator-builder:dev sh -ec '
        go test -mod=readonly ./internal/qemunet ./cmd/instigator
        go build -mod=readonly -trimpath -o /work/instigator ./cmd/instigator
        test -s /work/instigator
        go list -mod=readonly -deps \
            -f "{{if .Module}}{{.Module.Path}}|{{.Module.Version}}|{{.Module.Dir}}{{end}}" \
            ./cmd/instigator | sort -u > /work/instigator-go-deps-linux.txt
    '

"$engine" run --rm -v "$scratch:/work" -v "$instigator:/instigator:ro" \
    -v "$source:/qemu:ro" -v "$product:/product:ro" \
    -e QEMU_REV="$expected" -e INSTIGATOR_REV="$instigator_expected" \
    -e PRODUCT_REV="$product_revision" -e PRODUCT_STATE="$product_state" \
    localhost/sgi-qemu-builder:dev sh -ec '
        mkdir -p /work/linux-dev/bin /work/linux-dev/libexec/sgi \
            /work/linux-dev/share/sgi/licenses
        rm -f /work/linux-dev/bin/qemu-system-mips64 /work/linux-dev/bin/qemu-img
        cp /work/target-stable/release/sgi /work/linux-dev/bin/
        test -s /work/instigator
        cp /work/instigator /work/linux-dev/bin/
        cp /work/qemu-build/qemu-system-mips64 /work/qemu-build/qemu-img \
            /work/linux-dev/libexec/sgi/
        cp /instigator/LICENSE /work/linux-dev/share/sgi/licenses/instigator.LICENSE
        cp /qemu/LICENSE /work/linux-dev/share/sgi/licenses/qemu.LICENSE
        cp /qemu/COPYING /work/linux-dev/share/sgi/licenses/qemu.COPYING
        cp /qemu/COPYING.LIB /work/linux-dev/share/sgi/licenses/qemu.COPYING.LIB
        rm -rf /work/linux-dev/share/sgi/qemu/keymaps
        mkdir -p /work/linux-dev/share/sgi/qemu/keymaps
        for keymap in /qemu/pc-bios/keymaps/*; do
            [ "${keymap##*/}" = meson.build ] || cp "$keymap" /work/linux-dev/share/sgi/qemu/keymaps/
        done
        printf "product=%s (%s)\nqemu=%s\ninstigator=%s\n" \
            "$PRODUCT_REV" "$PRODUCT_STATE" "$QEMU_REV" "$INSTIGATOR_REV" \
            > /work/linux-dev/share/sgi/source-revisions.txt
        python3 /product/build/bundle-linux.py /work/linux-dev
        python3 /product/build/bundle-rust-licenses.py /work/cargo /work/linux-dev
        python3 /product/build/bundle-go-licenses.py \
            /work/instigator-go-deps-linux.txt /work/linux-dev
        /work/linux-dev/libexec/sgi/qemu-system-mips64 -display help | grep -qx sdl
        cd /work/linux-dev
        find bin lib libexec share -type f -print0 | sort -z | xargs -0 sha256sum > SHA256SUMS
    '
"$engine" run --rm -v "$scratch:/work" -v "$archive_dir:/out" \
    localhost/sgi-qemu-builder:dev sh -ec '
    cd /work
    tar --sort=name --owner=0 --group=0 --numeric-owner \
        -czf /out/sgi-linux-x86_64-preview.tar.gz linux-dev
    cd /out
    sha256sum sgi-linux-x86_64-preview.tar.gz > sgi-linux-x86_64-preview.tar.gz.sha256
'
printf 'Linux preview archive: %s/sgi-linux-x86_64-preview.tar.gz\n' "$archive_dir"
