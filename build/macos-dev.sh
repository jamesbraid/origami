#!/bin/sh
set -eu

if [ "$#" -ne 3 ]; then
    printf 'usage: %s QEMU-SOURCE INSTIGATOR-SOURCE SCRATCH-DIR\n' "$0" >&2
    exit 2
fi
if [ "$(uname -s)" != Darwin ] || [ "$(uname -m)" != arm64 ]; then
    printf 'macOS preview builds require a native arm64 Mac\n' >&2
    exit 2
fi
for tool in brew cargo git go ninja pkg-config python3 otool install_name_tool codesign shasum; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        printf 'missing build tool: %s\n' "$tool" >&2
        exit 2
    fi
done
if ! python3 -c 'import tomllib' >/dev/null 2>&1; then
    printf 'macOS preview builds require Python 3.11 or newer\n' >&2
    exit 2
fi
for library in glib-2.0 pixman-1 sdl2; do
    if ! pkg-config --exists "$library"; then
        printf 'missing pkg-config dependency: %s\n' "$library" >&2
        exit 2
    fi
done

product=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
source=$(CDPATH= cd -- "$1" && pwd)
instigator=$(CDPATH= cd -- "$2" && pwd)
scratch=$(CDPATH= cd -- "$3" && pwd)
expected=$(cat "$product/build/qemu-revision")
actual=$(git -C "$source" rev-parse HEAD)
if [ "$actual" != "$expected" ] || ! git -C "$source" diff --quiet || \
    ! git -C "$source" diff --cached --quiet; then
    printf 'QEMU needs a clean checkout at %s\n' "$expected" >&2
    exit 2
fi
instigator_expected=$(cat "$product/build/instigator-revision")
instigator_actual=$(git -C "$instigator" rev-parse HEAD)
if [ "$instigator_actual" != "$instigator_expected" ] || \
    ! git -C "$instigator" diff --quiet || \
    ! git -C "$instigator" diff --cached --quiet; then
    printf 'Instigator needs a clean checkout at %s\n' "$instigator_expected" >&2
    exit 2
fi
product_revision=$(git -C "$product" rev-parse HEAD)
product_state=clean
if ! git -C "$product" diff --quiet || ! git -C "$product" diff --cached --quiet || \
    [ -n "$(git -C "$product" ls-files --others --exclude-standard)" ]; then
    product_state=dirty
fi

build_source=$(sh "$product/build/prepare-qemu-source.sh" "$source" "$scratch" "$expected")
build="$scratch/qemu-macos-build"
export MESON_PACKAGE_CACHE_DIR="$scratch/meson-package-cache"
mkdir -p "$build"
if [ ! -f "$build/build.ninja" ]; then
    (cd "$build" && "$build_source/configure" --target-list=mips64-softmmu \
        --disable-docs --disable-gtk --disable-cocoa \
        --enable-sdl --enable-vnc --enable-slirp)
fi
ninja -C "$build" -j4 qemu-system-mips64 qemu-img
"$build/qemu-system-mips64" -display help | grep -qx sdl
"$build/qemu-system-mips64" -help | grep -q -- '^-vnc '

export CARGO_HOME="$scratch/cargo"
export CARGO_TARGET_DIR="$scratch/target-macos"
(cd "$product" && cargo test --locked && cargo build --locked --release)
export GOMODCACHE="$scratch/go-mod"
export GOCACHE="$scratch/go-build"
export GOTOOLCHAIN=local
export CGO_ENABLED=0
(cd "$instigator" && go test -mod=readonly ./internal/qemunet ./cmd/instigator)
(cd "$instigator" && go build -mod=readonly -trimpath \
    -o "$scratch/instigator-macos" ./cmd/instigator)
(cd "$instigator" && go list -mod=readonly -deps \
    -f '{{if .Module}}{{.Module.Path}}|{{.Module.Version}}|{{.Module.Dir}}{{end}}' \
    ./cmd/instigator > "$scratch/instigator-go-deps-macos.unsorted.txt")
sort -u "$scratch/instigator-go-deps-macos.unsorted.txt" \
    > "$scratch/instigator-go-deps-macos.txt"

package_work=$(mktemp -d "$scratch/macos-package.XXXXXXXX")
stage="$package_work/macos-arm64-dev"
mkdir -p "$stage/bin" "$stage/libexec/sgi" "$stage/share/sgi/licenses"
cp "$CARGO_TARGET_DIR/release/sgi" "$stage/bin/sgi"
cp "$scratch/instigator-macos" "$stage/bin/instigator"
cp "$build/qemu-system-mips64" "$build/qemu-img" "$stage/libexec/sgi/"
cp "$instigator/LICENSE" "$stage/share/sgi/licenses/instigator.LICENSE"
cp "$source/LICENSE" "$stage/share/sgi/licenses/qemu.LICENSE"
cp "$source/COPYING" "$stage/share/sgi/licenses/qemu.COPYING"
cp "$source/COPYING.LIB" "$stage/share/sgi/licenses/qemu.COPYING.LIB"
mkdir -p "$stage/share/sgi/qemu/keymaps"
for keymap in "$source"/pc-bios/keymaps/*; do
    [ "${keymap##*/}" = meson.build ] || cp "$keymap" "$stage/share/sgi/qemu/keymaps/"
done
printf 'product=%s (%s)\nqemu=%s\ninstigator=%s\n' \
    "$product_revision" "$product_state" "$expected" "$instigator_expected" \
    > "$stage/share/sgi/source-revisions.txt"
python3 "$product/build/bundle-macos.py" "$stage"
python3 "$product/build/bundle-rust-licenses.py" "$CARGO_HOME" "$stage"
python3 "$product/build/bundle-go-licenses.py" \
    "$scratch/instigator-go-deps-macos.txt" "$stage" "$GOMODCACHE"
python3 - "$stage" <<'PY'
import hashlib
import sys
from pathlib import Path
root = Path(sys.argv[1])
files = sorted(path for directory in ("bin", "lib", "libexec", "share")
               for path in (root / directory).rglob("*") if path.is_file())
with (root / "SHA256SUMS").open("w") as output:
    for path in files:
        output.write(f"{hashlib.sha256(path.read_bytes()).hexdigest()}  {path.relative_to(root)}\n")
PY
tar -C "$package_work" -czf "$scratch/sgi-macos-arm64-preview.tar.gz" macos-arm64-dev
(cd "$scratch" && shasum -a 256 sgi-macos-arm64-preview.tar.gz \
    > sgi-macos-arm64-preview.tar.gz.sha256)
printf 'macOS preview archive: %s/sgi-macos-arm64-preview.tar.gz\n' "$scratch"
