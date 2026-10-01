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
export MACOSX_DEPLOYMENT_TARGET=14.0
for tool in brew cargo rustc git go ninja pkg-config python3 dylibbundler otool codesign shasum; do
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

rust_notices="$(rustc --print sysroot)/share/doc/rust"
test -s "$rust_notices/COPYRIGHT-library.html"
test -s "$rust_notices/licenses/MIT.txt"
go_root=$(go env GOROOT)
test -s "$go_root/LICENSE"

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
export SGI_QEMU_REVISION="$expected"
export SGI_INSTIGATOR_REVISION="$instigator_expected"
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
cp "$CARGO_TARGET_DIR/release/origami" "$stage/bin/origami"
cp "$scratch/instigator-macos" "$stage/bin/instigator"
cp "$build/qemu-system-mips64" "$build/qemu-img" "$stage/libexec/sgi/"
cp "$instigator/LICENSE" "$stage/share/sgi/licenses/instigator.LICENSE"
mkdir -p "$stage/share/sgi/licenses/toolchains/rust" "$stage/share/sgi/licenses/toolchains/go"
cp "$rust_notices/COPYRIGHT-library.html" "$stage/share/sgi/licenses/toolchains/rust/"
cp -R "$rust_notices/licenses" "$stage/share/sgi/licenses/toolchains/rust/"
rustc -Vv > "$stage/share/sgi/licenses/toolchains/rust/toolchain.txt"
cp "$go_root/LICENSE" "$stage/share/sgi/licenses/toolchains/go/"
go version > "$stage/share/sgi/licenses/toolchains/go/toolchain.txt"
(cd "$go_root" && find src -type f \( -name LICENSE -o -name "LICENSE.*" \
    -o -name "LICENSE-*" -o -name COPYING -o -name "COPYING.*" \
    -o -name NOTICE -o -name "NOTICE.*" -o -name COPYRIGHT -o -name PATENTS \)) \
    > "$scratch/go-standard-notices.txt"
while IFS= read -r notice; do
    destination="$stage/share/sgi/licenses/toolchains/go/$notice"
    mkdir -p "$(dirname "$destination")"
    cp "$go_root/$notice" "$destination"
done < "$scratch/go-standard-notices.txt"
cp "$product/LICENSE" "$stage/share/sgi/licenses/origami.LICENSE"
cp "$source/LICENSE" "$stage/share/sgi/licenses/qemu.LICENSE"
cp "$source/COPYING" "$stage/share/sgi/licenses/qemu.COPYING"
cp "$source/COPYING.LIB" "$stage/share/sgi/licenses/qemu.COPYING.LIB"
cp "$source/hw/mips/sgi/models/LICENSE" \
    "$stage/share/sgi/licenses/qemu.sgi-models.LICENSE"
libslirp_dir=$(sed -n 's/^directory = //p' "$source/subprojects/libslirp.wrap")
if [ -z "$libslirp_dir" ]; then
    printf 'QEMU libslirp wrap has no source directory\n' >&2
    exit 2
fi
cp "$build_source/subprojects/$libslirp_dir/COPYRIGHT" \
    "$stage/share/sgi/licenses/libslirp.COPYRIGHT"
cp "$product/build/licenses/$libslirp_dir.copyright" \
    "$stage/share/sgi/licenses/libslirp.NOTICES"
mkdir -p "$stage/share/sgi/qemu/keymaps"
for keymap in "$source"/pc-bios/keymaps/*; do
    [ "${keymap##*/}" = meson.build ] || cp "$keymap" "$stage/share/sgi/qemu/keymaps/"
done
printf 'product=%s (%s)\nqemu=%s\ninstigator=%s\n' \
    "$product_revision" "$product_state" "$expected" "$instigator_expected" \
    > "$stage/share/sgi/source-revisions.txt"
python3 "$product/build/bundle-macos.py" "$stage"
python3 "$product/build/collect-homebrew-notices.py" \
    "$stage" "$scratch/macos-homebrew-notices"
python3 "$product/build/bundle-rust-licenses.py" "$CARGO_HOME" "$stage"
python3 "$product/build/bundle-go-licenses.py" \
    "$scratch/instigator-go-deps-macos.txt" "$stage" "$GOMODCACHE"
"$stage/bin/origami" version
"$stage/bin/origami" machines
"$stage/bin/instigator" --help
"$stage/libexec/sgi/qemu-system-mips64" --version
"$stage/libexec/sgi/qemu-img" --version

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
tar -C "$package_work" -czf "$scratch/origami-macos-arm64-preview.tar.gz" macos-arm64-dev
(cd "$scratch" && shasum -a 256 origami-macos-arm64-preview.tar.gz \
    > origami-macos-arm64-preview.tar.gz.sha256)
printf 'macOS preview archive: %s/origami-macos-arm64-preview.tar.gz\n' "$scratch"
