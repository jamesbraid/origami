#!/bin/sh
# usage: build-lib.sh OUT   Builds the pinned libghostty-vt into OUT/{lib,include}.
# Uses zig 0.17.1 from PATH, else fetches it (needs curl and xz).
set -eu
V=1.0.1
SRC_SHA=333dfce97641ef9b536bb9e08907ad86abaaada50e8f7417b70d9284adef37aa
DEPS_SHA=91a4e10960a52bf5bf4b15aa2a2cddaff8a89d6cd903d3055bda4fd17de3356d
KEY=RWQlAjJC23149WL2sEpT/l0QKy7hMIFhYdQOFy0Z7z7PbneUgvlsnYcV
BASE=https://release.files.ghostty.org/libghostty-vt/$V
ZIG_V=0.17.1
OUT=$1
mkdir -p "$OUT"
OUT=$(cd "$OUT" && pwd)
W=$(mktemp -d)
trap 'rm -rf "$W"' EXIT

if [ "$(zig version 2>/dev/null)" != "$ZIG_V" ]; then
  case $(uname -m) in
    x86_64) ARCH=x86_64; ZIG_SHA=3581863e3cbcfd0fce0efc682ce7e9e93da96668a977791d356a26d2a7e9b17a ;;
    aarch64|arm64) ARCH=aarch64; ZIG_SHA=a1546d5646eb184437918d6635ca6b762a0e889eac09cef720ee7a1ed3fe7795 ;;
    *) echo "no pinned Zig for $(uname -m)" >&2; exit 1 ;;
  esac
  ZIG_DIR=$OUT/../zig-$ZIG_V-$ARCH
  if [ ! -x "$ZIG_DIR/zig" ]; then
    curl -fsSL --proto '=https' --tlsv1.2 -o "$W/zig.tar.xz" "https://ziglang.org/download/$ZIG_V/zig-$ARCH-linux-$ZIG_V.tar.xz"
    echo "$ZIG_SHA  $W/zig.tar.xz" | sha256sum -c -
    mkdir -p "$ZIG_DIR" && tar -xJf "$W/zig.tar.xz" -C "$ZIG_DIR" --strip-components=1
  fi
  PATH=$ZIG_DIR:$PATH
fi

cd "$W"
for f in libghostty-vt-$V.tar.gz libghostty-vt-$V-deps.tar.gz; do
  curl -fsSL --proto '=https' --tlsv1.2 -O "$BASE/$f"
  if command -v minisign >/dev/null 2>&1; then
    curl -fsSL --proto '=https' --tlsv1.2 -O "$BASE/$f.minisig"
    minisign -V -P "$KEY" -m "$f"
  fi
done
echo "$SRC_SHA  libghostty-vt-$V.tar.gz" | sha256sum -c -
echo "$DEPS_SHA  libghostty-vt-$V-deps.tar.gz" | sha256sum -c -
tar -xzf libghostty-vt-$V.tar.gz
tar -xzf libghostty-vt-$V-deps.tar.gz
cd libghostty-vt-$V
# A fresh cache: Zig 0.17 can crash on a populated one.
ZIG_GLOBAL_CACHE_DIR="$W/cache" zig build --prefix "$OUT" \
  --system "$W/libghostty-vt-$V-deps" -Doptimize=ReleaseFast -Dcpu=baseline -Dstrip
