#!/usr/bin/env python3
"""Regenerate the frontend's test catalogue from QEMU's machine catalogue.

The Rust tests read tests/fixtures/sgi-machines.json instead of starting
QEMU. It holds every offering of QEMU's catalogue, trimmed to the fields the
frontend reads. Regenerate it from a QEMU binary's query-sgi-machines reply
or from a QEMU checkout's compiled catalogue (by default the qemu submodule),
which QEMU embeds and returns unchanged. --check fails when the committed
fixture differs from what the source would generate.
"""

import argparse
import json
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
FIXTURE = ROOT / "tests/fixtures/sgi-machines.json"
SUBMODULE_CATALOGUE = ROOT / "qemu/hw/mips/sgi/sgi-machines.json"

# The offering fields the frontend reads. Add a field here before reading it.
OFFERING_FIELDS = (
    "product", "topology", "nodes", "machine-options", "cpus-per-node", "smp",
    "memory-per-node-mib", "init-inputs", "storage", "scsi-adapters",
    "consoles", "hardware-inputs", "default-cpu-model",
    "needs-debug-leds-off",
)


def query(qemu):
    """Return the catalogue a QEMU binary reports over QMP on stdio."""
    commands = "".join(json.dumps({"execute": name}) + "\n"
                       for name in ("qmp_capabilities", "query-sgi-machines", "quit"))
    result = subprocess.run(
        [str(qemu), "-M", "none", "-qmp", "stdio", "-nodefaults", "-display", "none"],
        input=commands, capture_output=True, text=True, timeout=60, check=False)
    replies = [json.loads(line) for line in result.stdout.splitlines() if line.strip()]
    replies = [reply for reply in replies if "return" in reply or "error" in reply]
    if len(replies) < 2 or "error" in replies[1]:
        raise SystemExit(f"{qemu} did not answer query-sgi-machines: "
                         f"{result.stdout}{result.stderr}")
    return replies[1]["return"]


def trim(catalogue):
    if catalogue.get("schema") != "sgi-machines":
        raise SystemExit(f"unexpected catalogue schema {catalogue.get('schema')!r}")
    offerings = []
    for offering in catalogue["offerings"]:
        missing = [field for field in OFFERING_FIELDS if field not in offering]
        if missing:
            raise SystemExit(f"offering {offering.get('topology')} lacks {missing}")
        offerings.append({field: offering[field] for field in OFFERING_FIELDS})
    return {"schema": catalogue["schema"], "version": catalogue["version"],
            "offerings": offerings}


def render(catalogue):
    return json.dumps(trim(catalogue), indent=1, sort_keys=True) + "\n"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    source = parser.add_mutually_exclusive_group()
    source.add_argument("--qemu", type=Path, help="query this qemu-system-mips64")
    source.add_argument("--catalogue", type=Path, default=SUBMODULE_CATALOGUE,
                        help="read this compiled sgi-machines.json")
    parser.add_argument("--check", action="store_true",
                        help="compare with the committed fixture instead of writing it")
    args = parser.parse_args()
    if args.qemu:
        text = render(query(args.qemu))
    else:
        text = render(json.loads(args.catalogue.read_text()))
    if args.check:
        if FIXTURE.read_text() != text:
            sys.exit(f"{FIXTURE.relative_to(ROOT)} is stale; run {Path(__file__).name}")
        return
    FIXTURE.write_text(text)


if __name__ == "__main__":
    main()
