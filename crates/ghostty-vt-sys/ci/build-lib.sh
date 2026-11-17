#!/bin/sh
# usage: build-lib.sh OUT   (runs inside the Containerfile's image; OUT is writable)
# Downloads the pinned libghostty-vt release and its dependency tarball,
# checks the checksums and the upstream minisign signatures, and installs the
# library under OUT/{lib,include}.
set -eu
V=1.0.1
SRC_SHA=333dfce97641ef9b536bb9e08907ad86abaaada50e8f7417b70d9284adef37aa
DEPS_SHA=91a4e10960a52bf5bf4b15aa2a2cddaff8a89d6cd903d3055bda4fd17de3356d
KEY=RWQlAjJC23149WL2sEpT/l0QKy7hMIFhYdQOFy0Z7z7PbneUgvlsnYcV
BASE=https://release.files.ghostty.org/libghostty-vt/$V
OUT=$1
W=$(mktemp -d)
cd "$W"
for f in libghostty-vt-$V.tar.gz libghostty-vt-$V-deps.tar.gz; do
  curl -fsSLO "$BASE/$f"
  curl -fsSLO "$BASE/$f.minisig"
  minisign -V -P "$KEY" -m "$f"
done
echo "$SRC_SHA  libghostty-vt-$V.tar.gz" | sha256sum -c -
echo "$DEPS_SHA  libghostty-vt-$V-deps.tar.gz" | sha256sum -c -
tar -xzf libghostty-vt-$V.tar.gz
tar -xzf libghostty-vt-$V-deps.tar.gz
cd libghostty-vt-$V
# A fresh cache: Zig 0.17 can crash on a populated one.
ZIG_GLOBAL_CACHE_DIR="$W/cache" zig build --prefix "$OUT" \
  --system "$W/libghostty-vt-$V-deps" -Doptimize=ReleaseFast -Dcpu=baseline -Dstrip
rm -rf "$W"
