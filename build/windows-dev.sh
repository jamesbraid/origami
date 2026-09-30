#!/bin/sh
set -eu

if [ "$#" -lt 3 ] || [ "$#" -gt 4 ]; then
    printf 'usage: %s QEMU-SOURCE INSTIGATOR-SOURCE SCRATCH-DIR [ARCHIVE-DIR]\n' "$0" >&2
    exit 2
fi

product=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd -P)
if [ ! -d "$1" ]; then
    printf 'QEMU source is missing: %s; initialize the frontend submodule with git submodule update --init -- qemu\n' "$1" >&2
    exit 2
fi
if [ ! -d "$2" ]; then
    printf 'Instigator source is missing: %s; initialize the frontend submodule with git submodule update --init -- instigator\n' "$2" >&2
    exit 2
fi
source=$(CDPATH= cd -- "$1" && pwd -P)
instigator=$(CDPATH= cd -- "$2" && pwd -P)
scratch=$(CDPATH= cd -- "$3" && pwd -P)
archive_dir=${4:-$scratch}
mkdir -p "$archive_dir"
archive_dir=$(CDPATH= cd -- "$archive_dir" && pwd)
engine=${SGI_CONTAINER_ENGINE:-podman}
. "$product/build/gitlink-revision.sh"
expected=$(gitlink_revision qemu)
instigator_expected=$(gitlink_revision instigator)
source_top=$(git -C "$source" rev-parse --show-toplevel)
instigator_top=$(git -C "$instigator" rev-parse --show-toplevel)
if [ "$(CDPATH= cd -- "$source_top" && pwd -P)" != "$source" ]; then
    printf 'QEMU source is not a standalone checkout: %s\n' "$source" >&2
    exit 2
fi
if [ "$(CDPATH= cd -- "$instigator_top" && pwd -P)" != "$instigator" ]; then
    printf 'Instigator source is not a standalone checkout: %s\n' "$instigator" >&2
    exit 2
fi
actual=$(git -C "$source" rev-parse HEAD)
if [ "$actual" != "$expected" ] || ! git -C "$source" diff --quiet || \
    ! git -C "$source" diff --cached --quiet; then
    printf 'QEMU needs a clean checkout at %s\n' "$expected" >&2
    exit 2
fi
instigator_actual=$(git -C "$instigator" rev-parse HEAD)
if [ "$instigator_actual" != "$instigator_expected" ] || \
    ! git -C "$instigator" diff --quiet || ! git -C "$instigator" diff --cached --quiet; then
    printf 'Instigator needs a clean checkout at %s\n' "$instigator_expected" >&2
    exit 2
fi
product_revision=$(git -C "$product" rev-parse HEAD)
product_state=clean
if ! git -C "$product" diff --quiet || ! git -C "$product" diff --cached --quiet || \
    [ -n "$(git -C "$product" ls-files --others --exclude-standard)" ]; then
    product_state=dirty
fi

"$engine" build -t localhost/sgi-win64-builder:dev \
    -f "$source/tests/docker/dockerfiles/fedora-win64-cross.docker" \
    "$source/tests/docker/dockerfiles"
"$engine" build -t localhost/sgi-win64-rust-builder:dev \
    -f "$product/build/Windows.Containerfile" "$product/build"
"$engine" run --rm -v "$product:/product:ro" \
    localhost/sgi-win64-rust-builder:dev sh -ec '
        python3 /product/build/tests/test_bundle_windows.py
        python3 /product/build/tests/test_bundle_windows_runtime.py
    '
"$engine" build -t localhost/sgi-instigator-builder:dev \
    --build-arg "BASE_IMAGE=${SGI_INSTIGATOR_BASE_IMAGE:-docker.io/library/debian:forky-slim}" \
    -f "$product/build/Instigator.Containerfile" "$product/build"

build_source=$(sh "$product/build/prepare-qemu-source.sh" "$source" "$scratch" "$expected")
"$engine" run --rm -v "$build_source:/src" -v "$scratch:/work" \
    -w /work -e MESON_PACKAGE_CACHE_DIR=/work/meson-package-cache \
    localhost/sgi-win64-builder:dev sh -ec '
        mkdir -p qemu-win-build
        cd qemu-win-build
        if [ ! -f build.ninja ]; then
            /src/configure --cross-prefix=x86_64-w64-mingw32- \
                --target-list=mips64-softmmu --disable-docs --disable-gtk \
                --enable-sdl --enable-vnc --enable-slirp
        fi
        meson configure -Dc_args=-DBUILDING_LIBSLIRP \
            -Dcurl=disabled -Dgnutls=disabled -Dgcrypt=disabled \
            -Dbzip2=disabled \
            -Dlibssh=disabled -Dlibnfs=disabled -Dlibiscsi=disabled \
            -Drbd=disabled -Dvde=disabled -Dspice=disabled \
            -Dopengl=disabled -Dvirglrenderer=disabled
        ninja -j4 qemu-system-mips64.exe qemu-img.exe
        test -s qemu-system-mips64.exe && test -s qemu-img.exe
    '

"$engine" run --rm -v "$product:/src:ro" -v "$scratch:/work" \
    -w /src -e CARGO_HOME=/work/cargo -e CARGO_TARGET_DIR=/work/target-win \
    -e SGI_QEMU_REVISION="$expected" -e SGI_INSTIGATOR_REVISION="$instigator_expected" \
    -e CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=x86_64-w64-mingw32-gcc \
    localhost/sgi-win64-rust-builder:dev sh -ec '
        cargo build --locked --release --target x86_64-pc-windows-gnu
        test -s /work/target-win/x86_64-pc-windows-gnu/release/origami.exe
    '

"$engine" run --rm -v "$instigator:/src:ro" -v "$scratch:/work" \
    -w /src -e GOMODCACHE=/work/go-mod -e GOCACHE=/work/go-build \
    -e GOTOOLCHAIN=local -e GOOS=windows -e GOARCH=amd64 -e CGO_ENABLED=0 \
    localhost/sgi-instigator-builder:dev sh -ec '
        go build -mod=readonly -trimpath -o /work/instigator-windows.exe ./cmd/instigator
        test -s /work/instigator-windows.exe
        go list -mod=readonly -deps \
            -f "{{if .Module}}{{.Module.Path}}|{{.Module.Version}}|{{.Module.Dir}}{{end}}" \
            ./cmd/instigator | sort -u > /work/instigator-go-deps-windows.txt
    '

"$engine" run --rm -v "$scratch:/work" -v "$archive_dir:/out" \
    -v "$product:/product:ro" \
    -v "$source:/qemu:ro" -v "$build_source:/qemu-build-source:ro" \
    -v "$instigator:/instigator:ro" \
    -e PRODUCT_REV="$product_revision" -e PRODUCT_STATE="$product_state" \
    -e QEMU_REV="$expected" -e INSTIGATOR_REV="$instigator_expected" \
    localhost/sgi-win64-rust-builder:dev sh -ec '
        rm -rf /work/windows-dev
        mkdir -p /work/windows-dev/bin /work/windows-dev/libexec/sgi \
            /work/windows-dev/share/sgi/licenses
        cp /work/target-win/x86_64-pc-windows-gnu/release/origami.exe \
            /work/windows-dev/bin/origami.exe
        cp /work/instigator-windows.exe /work/windows-dev/bin/instigator.exe
        cp /work/qemu-win-build/qemu-system-mips64.exe \
            /work/qemu-win-build/qemu-img.exe /work/windows-dev/libexec/sgi/
        cp /product/LICENSE /work/windows-dev/share/sgi/licenses/origami.LICENSE
        cp /qemu/LICENSE /work/windows-dev/share/sgi/licenses/qemu.LICENSE
        cp /qemu/COPYING /work/windows-dev/share/sgi/licenses/qemu.COPYING
        cp /qemu/COPYING.LIB /work/windows-dev/share/sgi/licenses/qemu.COPYING.LIB
        libslirp_directory=$(sed -n "s/^directory = //p" \
            /qemu-build-source/subprojects/libslirp.wrap)
        test -n "$libslirp_directory"
        cp "/qemu-build-source/subprojects/$libslirp_directory/COPYRIGHT" \
            /work/windows-dev/share/sgi/licenses/libslirp.COPYRIGHT
        cp "/product/build/licenses/$libslirp_directory.copyright" \
            /work/windows-dev/share/sgi/licenses/libslirp.NOTICES
        rm -rf /work/windows-dev/share/sgi/qemu/keymaps
        mkdir -p /work/windows-dev/share/sgi/qemu/keymaps
        for keymap in /qemu/pc-bios/keymaps/*; do
            [ "${keymap##*/}" = meson.build ] || cp "$keymap" /work/windows-dev/share/sgi/qemu/keymaps/
        done
        cp /instigator/LICENSE /work/windows-dev/share/sgi/licenses/instigator.LICENSE
        printf "product=%s (%s)\nqemu=%s\ninstigator=%s\n" \
            "$PRODUCT_REV" "$PRODUCT_STATE" "$QEMU_REV" "$INSTIGATOR_REV" \
            > /work/windows-dev/share/sgi/source-revisions.txt
        rpm -qa | sort > /work/windows-dev/share/sgi/build-packages.txt
        python3 /product/build/bundle-windows.py /work/windows-dev
        python3 /product/build/bundle-rust-licenses.py /work/cargo /work/windows-dev
        python3 /product/build/bundle-go-licenses.py \
            /work/instigator-go-deps-windows.txt /work/windows-dev
        cd /work/windows-dev
        find bin libexec share -type f -print0 | sort -z | xargs -0 sha256sum > SHA256SUMS
        cd /work
        # ZIP cannot represent the pre-1980 timestamp in one bundled Rust license.
        find windows-dev -type f ! -newermt 1980-01-02 \
            -exec touch -d "1980-01-02 00:00:00 UTC" {} +
        python3 -m zipfile -c /out/origami-windows-x86_64-preview.zip windows-dev
        cd /out
        sha256sum origami-windows-x86_64-preview.zip > origami-windows-x86_64-preview.zip.sha256
    '
printf 'Windows preview archive: %s/origami-windows-x86_64-preview.zip\n' "$archive_dir"
