#!/bin/bash
# Remove what macos-keychain.sh made. Safe to run when it did not run.
security delete-keychain "${SIGN_KEYCHAIN:-$RUNNER_TEMP/origami-signing.keychain-db}" 2>/dev/null || true
rm -f "${NOTARY_KEY_FILE:-}" "$RUNNER_TEMP/origami-cert.p12"
