#!/bin/bash
# Run the signed qemu-system-mips64 from a macOS app zip: list machines, then boot octane2 for a few seconds and quit over QMP.
# A zero-filled PROM executes as nops, so the vCPU runs through TCG's MAP_JIT buffer; a missing entitlement kills QEMU here.
set -euo pipefail
: "${RUNNER_TEMP:?}"
work=$(mktemp -d "$RUNNER_TEMP/smoke.XXXXXX")
trap '[ -z "${pid:-}" ] || kill "$pid" 2>/dev/null || true; rm -rf "$work"' EXIT
ditto -x -k "$1" "$work"
qemu=$(find "$work" -name qemu-system-mips64 -type f | head -n1)
[ -n "$qemu" ] || { echo "$1: no qemu-system-mips64" >&2; exit 1; }
codesign -d --entitlements - "$qemu"
"$qemu" -M help | tee "$work/machines"
grep -q '^octane2 ' "$work/machines"

qmp() { # qmp COMMAND: one QMP session, prints the reply
    /usr/bin/python3 -I - "$work/qmp" "$1" <<'PY'
import json, socket, sys
s = socket.socket(socket.AF_UNIX)
s.connect(sys.argv[1])
f = s.makefile("rw")
f.readline()
for cmd in ("qmp_capabilities", sys.argv[2]):
    f.write(json.dumps({"execute": cmd}) + "\n")
    f.flush()
    while True:
        r = json.loads(f.readline())
        if "return" in r or "error" in r:
            break
print(json.dumps(r))
PY
}

dd if=/dev/zero of="$work/prom.bin" bs=1024 count=1024 2>/dev/null
"$qemu" -M octane2 -bios "$work/prom.bin" -display none -qmp "unix:$work/qmp,server,nowait" &
pid=$!
for _ in $(seq 50); do [ -S "$work/qmp" ] && break; sleep 0.2; done
sleep 3
kill -0 "$pid" || { echo "::error::qemu exited while running" >&2; wait "$pid" || true; exit 1; }
status=$(qmp query-status)
echo "$status"
case $status in *'"running": true'*) ;; *) echo "::error::guest is not running" >&2; exit 1 ;; esac
qmp quit
rc=0
wait "$pid" || rc=$?
pid=
[ "$rc" -eq 0 ] || { echo "::error::qemu exited $rc after quit" >&2; exit 1; }
