#!/usr/bin/env python3
"""Check that a guest on `origami network-set --mode vmnet` reaches the host's vmnet network.

Run on macOS with socket_vmnet already listening on --socket. The Octane2 PROM
broadcasts a BOOTP request from its Ethernet interface and netboots over TFTP.
The check passes when the request crosses vmnet to a dnsmasq on the vmnet
bridge, the PROM reports the address dnsmasq gave it and dnsmasq sends the
boot file back to that address.

Needs passwordless sudo for dnsmasq and tcpdump, which use privileged ports and
the bridge, and to stop macOS's own bootpd holding the DHCP port. Origami runs
as the calling user.
"""
from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import socket
import subprocess
import sys
import tempfile
import time
import tomllib
from pathlib import Path


def log(message):
    print(f"[vmnet-check] {message}", flush=True)


def origami(binary, *args, check=True):
    env = {k: v for k, v in os.environ.items()
           if k not in ("ORIGAMI_RUNTIME_DIR", "DYLD_LIBRARY_PATH")}
    result = subprocess.run([str(binary), *map(str, args)], capture_output=True, text=True,
                            env=env, timeout=120)
    log(f"origami {' '.join(map(str, args))} -> {result.returncode}")
    if result.stdout.strip():
        print(result.stdout.rstrip(), flush=True)
    if result.stderr.strip():
        print(result.stderr.rstrip(), file=sys.stderr, flush=True)
    if check and result.returncode:
        raise SystemExit(f"origami {args[0]} failed")
    return result


def bridge_for(gateway):
    """The host interface that carries the vmnet gateway address."""
    text = subprocess.run(["ifconfig"], capture_output=True, text=True, check=True).stdout
    name = None
    for line in text.splitlines():
        header = re.match(r"^(\w+): flags", line)
        if header:
            name = header.group(1)
        elif re.search(rf"\binet {re.escape(gateway)}\b", line):
            return name
    return None


class Console:
    """The background guest's serial console, on the loopback port origami records."""

    def __init__(self, port):
        self.sock = socket.create_connection(("127.0.0.1", port), timeout=10)
        self.sock.settimeout(0.5)
        self.text = ""

    def pump(self):
        try:
            data = self.sock.recv(65536)
        except socket.timeout:
            return
        if not data:
            raise SystemExit("console closed")
        self.text += data.decode("latin-1").replace("\r", "")

    def wait(self, pattern, timeout, start=0):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            self.pump()
            found = re.search(pattern, self.text[start:])
            if found:
                return found
        return None

    def send(self, data):
        self.sock.sendall(data.encode())


POOL = r"192\.168\.105\.1[01]\d"  # the dnsmasq range below, 100 to 119


def evidence_failures(mac, console, dnsmasq, capture):
    """What the logs fail to show; an empty list means the guest used vmnet."""
    failures = []
    leased = re.search(rf"Setting \$netaddr to ({POOL})\b", console)
    if not leased:
        failures.append("the PROM never reported an address from the server")
        return failures
    address = leased.group(1)
    if "Obtaining /unix from server" not in console:
        failures.append("the PROM did not start the TFTP download")
    if not re.search(rf"BOOTP\(\S+\) {re.escape(address)} {re.escape(mac)}", dnsmasq):
        failures.append(f"dnsmasq did not log answering {mac} with {address}")
    if not re.search(rf"dnsmasq-tftp\[\d+\]: sent \S.*/unix to {re.escape(address)}$",
                     dnsmasq, re.M):
        failures.append(f"dnsmasq did not log sending the boot file to {address}")
    if f"Request from {mac}" not in capture:
        failures.append("tcpdump on the vmnet bridge saw no request from the guest")
    if not re.search(rf"Reply.*\n(?:.*\n)*?\s+Your-IP {re.escape(address)}\n", capture):
        failures.append("tcpdump on the vmnet bridge saw no reply carrying the address")
    return failures


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("package", type=Path, help="extracted origami package")
    parser.add_argument("--socket", required=True, type=Path, help="socket_vmnet socket")
    parser.add_argument("--gateway", default="192.168.105.1",
                        help="address socket_vmnet gives the host side of the network")
    parser.add_argument("--dnsmasq", default="dnsmasq", help="dnsmasq executable")
    parser.add_argument("--scratch", required=True, type=Path)
    parser.add_argument("--out", required=True, type=Path, help="directory for logs")
    args = parser.parse_args()
    binary = args.package.resolve(strict=True) / "bin/origami"
    args.out.mkdir(parents=True, exist_ok=True)

    bridge = bridge_for(args.gateway)
    log(f"bridge carrying {args.gateway}: {bridge}")
    if not bridge:
        raise SystemExit(f"no host interface has {args.gateway}; is socket_vmnet running?")

    machine = Path(tempfile.mkdtemp(prefix="vmnet check ", dir=args.scratch)) / "octane2"
    origami(binary, "create", machine, "--preset", "octane2-vpro")
    origami(binary, "network-set", machine, "--mode", "vmnet", "--endpoint", args.socket)
    mac = tomllib.loads((machine / "machine.toml").read_text())["identity"]["mac"].lower()
    log(f"guest MAC {mac}")
    origami(binary, "show-command", machine)

    def start():
        origami(binary, "run", machine, "--display", "none", "--background")
        state = json.loads((machine / "state/run.json").read_text())
        return Console(state["console_port"])

    def stop():
        origami(binary, "stop", machine, check=False)
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            if origami(binary, "status", machine, check=False).stdout.strip() == "stopped":
                return
            time.sleep(1)

    # A first start makes the PROM write its default environment, which
    # `nvram` can then change; the serial console is off until then.
    console = start()
    log(f"first start: {bool(console.wait(r'Re-Initializing PDS', 120))}")
    time.sleep(5)
    stop()
    origami(binary, "nvram", machine, "set", "console=d")

    # launchd holds the DHCP port for macOS's bootpd, which ignores the PROM's
    # BOOTP request. Take it over so dnsmasq can serve the vmnet bridge.
    subprocess.run(["sudo", "launchctl", "bootout", "system/com.apple.bootpd"], check=False)
    # The boot file stays under one TFTP block so a logged transfer is a whole one.
    tftp = machine.parent / "tftp"
    tftp.mkdir()
    (tftp / "unix").write_bytes(b"origami vmnet check\n" * 10)
    dnsmasq_log = args.out / "dnsmasq.log"
    dnsmasq = subprocess.Popen(
        ["sudo", shutil.which(args.dnsmasq) or args.dnsmasq, "--keep-in-foreground",
         "--conf-file=/dev/null", "--port=0", f"--interface={bridge}", "--bind-interfaces",
         f"--listen-address={args.gateway}",
         "--dhcp-range=192.168.105.100,192.168.105.119,255.255.255.0,1h",
         "--bootp-dynamic", "--dhcp-broadcast", "--dhcp-boot=/unix", "--enable-tftp",
         f"--tftp-root={tftp}", "--log-dhcp", "--log-facility=-", "--user=root",
         f"--dhcp-leasefile={machine.parent / 'leases'}", f"--pid-file={machine.parent / 'pid'}"],
        stdout=dnsmasq_log.open("w"), stderr=subprocess.STDOUT, start_new_session=True)
    time.sleep(3)
    if dnsmasq.poll() is not None:
        print(dnsmasq_log.read_text(), flush=True)
        raise SystemExit("dnsmasq did not start")
    capture = args.out / "bootp.txt"
    dump = subprocess.Popen(
        ["sudo", "tcpdump", "-i", bridge, "-l", "-n", "-e", "-vv", "udp port 67 or udp port 68"],
        stdout=capture.open("w"), stderr=subprocess.STDOUT, start_new_session=True)
    time.sleep(2)
    console = None
    try:
        console = start()
        # With no disk the power-on diagnostics fail and wait for a key.
        if console.wait(r"Press any key to continue", 240):
            console.send(" ")
        if not console.wait(r"Command Monitor", 120):
            raise SystemExit("the PROM never offered its System Maintenance Menu")
        console.send("5\r")
        console.wait(r">> ", 30)
        # BOOTP replies go to the address the PROM already holds, so give it a
        # placeholder on this network; the server's address replaces it.
        console.send("setenv netaddr 192.168.105.77\r")
        time.sleep(2)
        mark = len(console.text)
        console.send(f"boot -f bootp(){args.gateway}:/unix\r")
        # The PROM cannot run the file; it gives up after reading it.
        console.wait(r"(?i)(unable to load|no server)", 90, mark)
        time.sleep(3)
        console.pump()
    finally:
        if console:
            (args.out / "console.txt").write_text(console.text)
            print(console.text[-3000:], flush=True)
        stop()
        subprocess.run(["sudo", "pkill", "-INT", "tcpdump"], check=False)
        subprocess.run(["sudo", "pkill", "-TERM", "dnsmasq"], check=False)
        time.sleep(1)
    print(capture.read_text()[:6000], flush=True)
    print(dnsmasq_log.read_text(), flush=True)
    failures = evidence_failures(mac, console.text, dnsmasq_log.read_text(), capture.read_text())
    for failure in failures:
        log(f"FAIL: {failure}")
    if not failures:
        log("PASS: the guest's BOOTP request crossed vmnet and its TFTP download came back")
    return 1 if failures else 0


if __name__ == "__main__":
    raise SystemExit(main())
