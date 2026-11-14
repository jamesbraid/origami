#!/usr/bin/env python3
"""Write sgi-machines.json, the machine list the unit tests read, from a real
QEMU's own `query-sgi-machines` reply.

    tests/fixtures/generate-machines.py QEMU-SYSTEM-MIPS64 > tests/fixtures/sgi-machines.json

The reply is about a megabyte, so this keeps the offerings the tests use, the
topologies those offerings name, and only the record fields the CLI reads. No
value is edited: a field is either copied as QEMU reported it or left out.
Regenerate it whenever the QEMU pin moves.
"""
import json
import subprocess
import sys

OFFERINGS = """
fuel.1 o2.1 o2plus.1 octane.1 octane2.1
onyx2-deskside.2-2 onyx2-deskside.2 onyx2-rack.1 onyx2-rack.1-1 onyx2-rack.2-2
onyx2-rack.2-1
onyx3000.4-4.r12000-400mhz-8mb onyx3000.4-4.r14000-500mhz-8mb
onyx3000.4-4.r14000-600mhz-8mb onyx3000.4-4.r16000-800mhz-8mb
onyx3400-ir.4-4-4-4
onyx350-ip53.4 onyx350-ip59.4 onyx4.4
origin200.1.r10000-180mhz-1mb origin200.2.r10000-180mhz-1mb
origin200.2.r12000-270mhz-4mb origin200.2.r10000-180mhz-2mb
origin200-dual.2-2.r10000-180mhz-1mb
origin200-gigachannel.1.r10000-180mhz-1mb
origin2000-deskside.2 origin2000-rack.2-2-2-2
origin300.2 origin300.4 origin300-pci-expansion.2
origin300-numalink-pci-expansion.2-2
origin3200.4-4.r12000-400mhz-8mb origin3200.4-4.r14000-500mhz-8mb
origin3200.4-4.r14000-600mhz-8mb origin3200.4-4.r16000-800mhz-8mb
origin3400.4-4-4-4.r12000-400mhz-8mb origin3400.4-4-4-4.r14000-600mhz-8mb
origin350-ip53.4.r16000-700mhz-4mb origin350-ip53.4.r16000-800mhz-8mb-rev22
origin350-ip59.4
origin3900-ip53.4-4-4-4 origin3900-ip59.4-4-4-4
tezro-ip53.4.r16000-700mhz-4mb tezro-ip53.2.r16000-700mhz-4mb
tezro-ip53.4.r16000-800mhz-8mb-rev22
tezro-ip59.4 tezro-ip59.2 tezro-one-processor.1
tezro-rack-ip53.4.r16000-700mhz-4mb tezro-rack-ip59.4
""".split()

# What the CLI reads of an offering and of a topology.
OFFERING_KEYS = """id product topology nodes smp cpus-per-node default-cpu-model
processor memory-per-node-mib machine-options init-inputs storage scsi-adapters
needs-debug-leds-off settings setting-groups consoles networks hostid-source
clock firmware launch-recipe""".split()
TOPOLOGY_KEYS = ["name", "processor-options", "graphics"]


def catalogue(qemu):
    child = subprocess.Popen(
        [qemu, "-M", "none", "-qmp", "stdio", "-nodefaults", "-display", "none"],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        text=True,
    )
    child.stdout.readline()
    child.stdin.write('{"execute":"qmp_capabilities"}\n{"execute":"query-sgi-machines"}\n')
    child.stdin.flush()
    child.stdout.readline()
    reply = json.loads(child.stdout.readline())["return"]
    child.kill()
    child.wait()
    return reply


def main():
    full = catalogue(sys.argv[1])
    by_id = {o["id"]: o for o in full["offerings"]}
    missing = [i for i in OFFERINGS if i not in by_id]
    if missing:
        sys.exit(f"this QEMU has no offering {', '.join(missing)}")
    offerings = [
        {k: by_id[i][k] for k in OFFERING_KEYS if k in by_id[i]} for i in OFFERINGS
    ]
    names = {o["topology"] for o in offerings}
    topologies = [
        {k: t[k] for k in TOPOLOGY_KEYS if k in t}
        for t in full["topologies"]
        if t["name"] in names
    ]
    # One record to a line, so a regenerated fixture diffs by record.
    def lines(records):
        return ",\n".join(" " + json.dumps(r, sort_keys=True) for r in records)

    print("{")
    print(f' "schema": {json.dumps(full["schema"])},')
    print(f' "version": {full["version"]},')
    print(f' "offerings": [\n{lines(offerings)}\n ],')
    print(f' "topologies": [\n{lines(topologies)}\n ],')
    print(f' "setting-definitions": [\n{lines(full["setting-definitions"])}\n ]')
    print("}")


main()
