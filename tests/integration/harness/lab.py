"""The virtual LAN inside the lab container.

    br0 10.77.0.1/24  --  tap0, tap1 (one per guest)
      |
     veth  --  peer network namespace 10.77.0.2/24: dnsmasq (DHCP),
               rpcbind (UDP/TCP 111) and unfsd (NFSv3)

The peer sits in its own network namespace, so guests reach it across the
bridge as they would a separate machine. The lab container itself plays the
host: user-mode (slirp) networking leaves from it, over br0.
"""
import os
import subprocess
import time

SUBNET = "10.77.0.0/24"
HOST = "10.77.0.1"
PEER = "10.77.0.2"
EXPORT = "/srv/export"
EXPORT_OPEN = EXPORT + "-open"


def sh(*args, check=True, **kw):
    return subprocess.run(args, check=check, text=True, capture_output=True, **kw)


class Lab:
    def __init__(self, logs="/work/logs"):
        self.logs = logs
        self.holder = None
        self.daemons = []

    def peer(self, *cmd):
        return ["nsenter", "-t", str(self.holder.pid), "-n", *cmd]

    def start(self, taps=2):
        sh("ip", "link", "set", "lo", "up")
        sh("ip", "link", "add", "br0", "type", "bridge", "stp_state", "0", "forward_delay", "0")
        sh("ip", "addr", "add", f"{HOST}/24", "dev", "br0")
        sh("ip", "link", "set", "br0", "up")
        for n in range(taps):
            sh("ip", "tuntap", "add", "dev", f"tap{n}", "mode", "tap")
            sh("ip", "link", "set", f"tap{n}", "master", "br0", "up")
        # A process that owns the peer's network namespace.
        self.holder = subprocess.Popen(["unshare", "-n", "sleep", "infinity"])
        time.sleep(0.3)
        sh("ip", "link", "add", "vlab", "type", "veth", "peer", "name", "vpeer")
        sh("ip", "link", "set", "vlab", "master", "br0", "up")
        sh("ip", "link", "set", "vpeer", "netns", str(self.holder.pid))
        sh(*self.peer("ip", "link", "set", "lo", "up"))
        sh(*self.peer("ip", "addr", "add", f"{PEER}/24", "dev", "vpeer"))
        sh(*self.peer("ip", "link", "set", "vpeer", "up"))
        self.start_peer_services()

    def daemon(self, name, *cmd):
        log = open(f"{self.logs}/{name}.log", "ab")
        p = subprocess.Popen(self.peer(*cmd), stdout=log, stderr=log)
        self.daemons.append(p)
        return p

    def start_peer_services(self):
        os.makedirs(self.logs, exist_ok=True)
        os.makedirs(EXPORT, exist_ok=True)
        os.makedirs(EXPORT + "-open", exist_ok=True)
        os.makedirs("/run/rpcbind", exist_ok=True)
        with open("/etc/exports.lab", "w") as f:
            # The second accepts requests from unprivileged source ports,
            # which NAT gives every guest packet.
            f.write(f"{EXPORT} (rw,no_root_squash)\n")
            f.write(f"{EXPORT}-open (rw,no_root_squash,insecure)\n")
        with open("/etc/dnsmasq.lab", "w") as f:
            f.write(
                "port=0\nno-ping\nbind-interfaces\ninterface=vpeer\nexcept-interface=lo\n"
                f"dhcp-range=10.77.0.100,10.77.0.150,255.255.255.0,1h\n"
                f"dhcp-option=option:router,{PEER}\n"
                f"dhcp-leasefile={self.logs}/dnsmasq.leases\n"
                f"log-dhcp\nlog-facility={self.logs}/dnsmasq-dhcp.log\n"
            )
        self.daemon("rpcbind", "rpcbind", "-f", "-w")
        time.sleep(1)
        self.daemon("dnsmasq", "dnsmasq", "-k", "-C", "/etc/dnsmasq.lab")
        self.daemon("unfsd", "unfsd", "-d", "-e", "/etc/exports.lab")
        time.sleep(1)

    def stop(self):
        for p in self.daemons:
            p.terminate()
        if self.holder:
            self.holder.terminate()
