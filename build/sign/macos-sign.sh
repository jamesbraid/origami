#!/bin/bash
# Sign, notarize and re-pack macOS archives: build/sign/macos-sign.sh ARCHIVE... (.zip with an .app, or .tar.gz).
# Needs macos-keychain.sh's exports and ASC_KEY_ID, ASC_ISSUER_ID.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
: "${SIGN_KEYCHAIN:?}" "${SIGN_IDENTITY:?}" "${NOTARY_KEY_FILE:?}" "${ASC_KEY_ID:?}" "${ASC_ISSUER_ID:?}" "${RUNNER_TEMP:?}"
export COPYFILE_DISABLE=1

sign() { # sign FILE [ENTITLEMENTS]
    local args=(--force --sign "$SIGN_IDENTITY" --keychain "$SIGN_KEYCHAIN" --options runtime --timestamp)
    [ -z "${2:-}" ] || args+=(--entitlements "$2")
    codesign "${args[@]}" "$1"
}

notarize() { # notarize ZIP
    local out status id
    out=$(xcrun notarytool submit "$1" --key "$NOTARY_KEY_FILE" --key-id "$ASC_KEY_ID" \
        --issuer "$ASC_ISSUER_ID" --wait --timeout 150m --output-format json)
    status=$(printf %s "$out" | /usr/bin/python3 -c 'import json,sys; print(json.load(sys.stdin).get("status",""))')
    id=$(printf %s "$out" | /usr/bin/python3 -c 'import json,sys; print(json.load(sys.stdin).get("id",""))')
    echo "notarization $id: $status"
    if [ "$status" != Accepted ]; then
        # --wait exits 0 for an Invalid submission, so the status is the check.
        xcrun notarytool log "$id" --key "$NOTARY_KEY_FILE" --key-id "$ASC_KEY_ID" --issuer "$ASC_ISSUER_ID" || true
        echo "::error::notarization of $1 ended $status"
        return 1
    fi
}

for archive in "$@"; do
    archive=$(cd "$(dirname "$archive")" && pwd)/$(basename "$archive")
    work=$(mktemp -d "$RUNNER_TEMP/sign.XXXXXX")
    case $archive in
        *.zip) ditto -x -k "$archive" "$work/tree" ;;
        *.tar.gz) mkdir "$work/tree" && tar -xzf "$archive" -C "$work/tree" ;;
        *) echo "$archive: not a .zip or .tar.gz" >&2; exit 1 ;;
    esac
    top=$(ls "$work/tree")
    [ "$(echo "$top" | wc -l)" -eq 1 ] || { echo "$archive: expected one top-level entry" >&2; exit 1; }
    root="$work/tree/$top"

    # Every Mach-O, deepest path first so nested code is signed before what holds it.
    machos="$work/machos"
    find "$root" -type f | awk -F/ '{print NF "\t" $0}' | sort -rn | cut -f2- | while IFS= read -r f; do
        case $(file -b "$f") in *Mach-O*) echo "$f" ;; esac
    done > "$machos"
    [ -s "$machos" ] || { echo "$archive: no Mach-O files found" >&2; exit 1; }
    while IFS= read -r f; do
        case $(basename "$f") in
            qemu-system-*) sign "$f" "$here/qemu.entitlements.plist" ;;
            *) sign "$f" ;;
        esac
    done < "$machos"

    case $top in
        *.app)
            sign "$root"
            ditto -c -k --keepParent "$root" "$work/submit.zip"
            notarize "$work/submit.zip"
            xcrun stapler staple "$root"
            xcrun stapler validate "$root"
            codesign --verify --deep --strict --verbose=2 "$root"
            codesign -dv --verbose=2 "$root"
            spctl -a -vv -t exec "$root"
            ;;
        *)
            ditto -c -k --keepParent "$root" "$work/submit.zip"
            notarize "$work/submit.zip"
            while IFS= read -r f; do codesign --verify --strict --verbose=2 "$f"; done < "$machos"
            ;;
    esac

    rm -f "$archive"
    case $archive in
        *.zip) ditto -c -k --keepParent "$root" "$archive" ;;
        *) tar -czf "$archive" -C "$work/tree" "$top" ;;
    esac
    rm -rf "$work"
    echo "signed $archive"
done
