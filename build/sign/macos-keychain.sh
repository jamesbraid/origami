#!/bin/bash
# Import the Developer ID certificate into a throwaway keychain and write the notary key from base64 secrets.
# Exports SIGN_KEYCHAIN, SIGN_IDENTITY and NOTARY_KEY_FILE to $GITHUB_ENV.
set -euo pipefail
: "${RUNNER_TEMP:?}" "${MACOS_CERT_P12_BASE64:?}" "${MACOS_CERT_PASSWORD:?}" "${APPLE_TEAM_ID:?}" "${ASC_KEY_ID:?}" "${ASC_KEY_P8_BASE64:?}"
kc="$RUNNER_TEMP/origami-signing.keychain-db"
pw=$(uuidgen)
security create-keychain -p "$pw" "$kc"
security set-keychain-settings -lut 21600 "$kc"
security unlock-keychain -p "$pw" "$kc"
( umask 077; printf %s "$MACOS_CERT_P12_BASE64" | openssl base64 -d -A > "$RUNNER_TEMP/origami-cert.p12" )
security import "$RUNNER_TEMP/origami-cert.p12" -k "$kc" -P "$MACOS_CERT_PASSWORD" -T /usr/bin/codesign -T /usr/bin/security >/dev/null
rm -f "$RUNNER_TEMP/origami-cert.p12"
# Lets codesign use the key without a UI prompt.
security set-key-partition-list -S apple-tool:,apple:,codesign: -s -k "$pw" "$kc" >/dev/null
# shellcheck disable=SC2046
security list-keychains -d user -s "$kc" $(security list-keychains -d user | tr -d '"')
identity=$(security find-identity -v -p codesigning "$kc" \
    | awk -v t="($APPLE_TEAM_ID)" '/Developer ID Application/ && index($0, t) {print $2; exit}')
if [ -z "$identity" ]; then
    echo "::error::no Developer ID Application identity for team $APPLE_TEAM_ID in the imported certificate"
    exit 1
fi
key="$RUNNER_TEMP/AuthKey_$ASC_KEY_ID.p8"
( umask 077; printf %s "$ASC_KEY_P8_BASE64" | openssl base64 -d -A > "$key" )
{
    echo "SIGN_KEYCHAIN=$kc"
    echo "SIGN_IDENTITY=$identity"
    echo "NOTARY_KEY_FILE=$key"
} >> "$GITHUB_ENV"
