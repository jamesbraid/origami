#!/bin/sh
# Point the Homebrew tap's cask and formula at a release.
# usage: update-tap.sh TAG SHA256SUMS TAP_DIR
set -eu
tag=$1 sums=$2 tap=$3
version=${tag#v}

sum() {
  hash=$(awk -v f="$1" '$2 == f {print $1}' "$sums")
  case $hash in
    *[!0-9a-f]* | '') hash= ;;
  esac
  if [ "${#hash}" != 64 ]; then
    echo "$sums has no valid checksum for $1" >&2
    exit 1
  fi
  echo "$hash"
}

app=origami-$version-macos-universal.zip
cli=origami-cli-$version-macos-universal.tar.gz
app_sum=$(sum "$app")
cli_sum=$(sum "$cli")

cask=$tap/Casks/origami.rb
formula=$tap/Formula/origami-cli.rb
sed -i.bak \
  -e "s/^  version \".*\"/  version \"$version\"/" \
  -e "s/^  sha256 \".*\"/  sha256 \"$app_sum\"/" "$cask"
sed -i.bak \
  -e "s#/download/v[^/]*/origami-cli-[^\"]*#/download/v$version/$cli#" \
  -e "s/^  sha256 \".*\"/  sha256 \"$cli_sum\"/" "$formula"
rm -f "$cask.bak" "$formula.bak"

# Each check is its own statement: set -e ignores a failed grep before &&.
grep -qF "  version \"$version\"" "$cask"
grep -qF "  sha256 \"$app_sum\"" "$cask"
grep -qF "/v$version/$cli\"" "$formula"
grep -qF "  sha256 \"$cli_sum\"" "$formula"
