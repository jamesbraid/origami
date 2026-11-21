#!/bin/sh
# Windows (Git Bash) or Linux with the .NET SDK: generate the bindings, build
# the test program against them, and run it.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../../../.." && pwd)
target=${CARGO_TARGET_DIR:-$repo/target}
out=$(mktemp -d)
trap 'rm -rf "$out"' EXIT
# dotnet is a native Windows program: give it Windows paths under Git Bash.
native() { if command -v cygpath >/dev/null 2>&1; then cygpath -m "$1"; else printf %s "$1"; fi; }
"$here/gen.sh" "$out/gen" "$target"
# The Windows library imports ghostty-vt.dll, which the build leaves in its cache.
for dll in "$target"/ghostty-vt/*/ghostty-vt.dll; do
  [ -f "$dll" ] && cp "$dll" "$target/debug/"
done
dotnet run --project "$(native "$here/BindingsTest.csproj")" -c Release \
  -p:GeneratedBindings="$(native "$out/gen/origami_ffi.cs")" \
  -p:OrigamiLibDir="$(native "$target/debug")"
