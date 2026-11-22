#!/bin/sh
# Check the downloaded build artifacts in DIR are the ten release archives and the two Windows
# installers named for VERSION, then write SHA256SUMS beside them.
# usage: release-assets.sh VERSION DIR
set -eu
version=$1 dir=$2
cd "$dir"
# The thin macOS packages feed the universal ones; they are not releases.
rm -f -- *-macos-arm64.* *-macos-x86_64.*
ls
test "$(ls origami-cli-*.tar.gz origami-cli-*.zip | wc -l)" = 5
test "$(ls origami-*.tar.gz origami-*.zip | grep -vc '^origami-cli-')" = 5
test "$(ls origami-"$version"-windows-*-setup.exe | wc -l)" = 2
# A dirty or untagged build would be named for the commit instead.
for f in origami-*; do
  case $f in
    origami-cli-"$version"-* | origami-"$version"-*) ;;
    *) echo "$f is not named for $version" >&2; exit 1 ;;
  esac
done
sha256sum -- *.tar.gz *.zip *-setup.exe > SHA256SUMS
sha256sum --check SHA256SUMS
