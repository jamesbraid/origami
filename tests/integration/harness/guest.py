"""Running guests and checking them: the context a scenario receives."""
import os
import re
import shutil
import subprocess
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from console import Console, login  # noqa: E402
from harness.lab import EXPORT, EXPORT_OPEN, PEER, Lab  # noqa: E402,F401

ORIGAMI = "/product/bin/origami"
QEMU_IMG = "/product/libexec/origami/qemu-img"


def posix_cksum(data):
    """POSIX cksum(1): CRC-32 with the length folded in, as `cksum` prints."""
    table = []
    for i in range(256):
        c = i << 24
        for _ in range(8):
            c = ((c << 1) ^ 0x04C11DB7) if c & 0x80000000 else c << 1
        table.append(c & 0xFFFFFFFF)
    crc = 0
    for b in data:
        crc = ((crc << 8) ^ table[((crc >> 24) ^ b) & 0xFF]) & 0xFFFFFFFF
    n = len(data)
    while n:
        crc = ((crc << 8) ^ table[((crc >> 24) ^ n) & 0xFF]) & 0xFFFFFFFF
        n >>= 8
    return f"{crc ^ 0xFFFFFFFF} {len(data)}"


class Ctx:
    """Per-scenario paths, the lab and helpers. `work` is the scenario's
    scratch directory; `evidence` is what the runner keeps."""

    def __init__(self, work, image_dir, lab):
        self.work = work
        self.evidence = f"{work}/evidence"
        self.machines = f"{work}/machines"
        self.image_dir = image_dir
        self.lab = lab
        self.env = dict(os.environ, XDG_CACHE_HOME=f"{work}/xdg", HOME=f"{work}/home")
        self.guests = []
        for d in (self.evidence, self.machines, f"{work}/xdg", f"{work}/home"):
            os.makedirs(d, exist_ok=True)

    def origami(self, *args, check=True):
        r = subprocess.run([ORIGAMI, *args], env=self.env, cwd=self.machines,
                           text=True, capture_output=True)
        print("+ origami", *args, "->", r.returncode, (r.stdout + r.stderr).strip(), flush=True)
        if check and r.returncode:
            raise RuntimeError(f"origami {args}: {r.stderr.strip()}")
        return r

    def guest(self, name, mac, src=None):
        g = Guest(self, name, mac)
        self.guests.append(g)
        return g.create(src or self.prepared())

    def prepared(self):
        """The installed system holds a DHCP lease from the install network,
        and IRIX keeps using it on any other network until the lease files
        go, as the user guide says: asked on a new network, its DHCP client
        takes the server's offers for mismatches and loops until it runs out
        of descriptors. Boot once on user networking, remove them and shut
        down; the result is an overlay of the cached disk that guests build
        on. Done once per scenario."""
        path = f"{self.machines}/prepared"
        if os.path.exists(path):
            return path
        g = Guest(self, "prepared", "08:00:69:e1:03:01").create(f"{self.image_dir}/machine")
        try:
            g.network("--mode", "user")
            g.start()
            g.must("ls /var/adm/proclaim.lease_info* /etc/resolv.conf")
            g.must("rm -f /var/adm/proclaim.lease_info* /etc/resolv.conf; sync")
        finally:
            g.stop()
        return path

    def teardown(self):
        for g in self.guests:
            g.stop()

    def capture(self, name, count=80):
        """The first packets to the peer's portmapper and NFS port, as
        evidence of the source ports guests' requests carry."""
        out = open(f"{self.evidence}/{name}.txt", "w")
        p = subprocess.Popen(["tcpdump", "-nn", "-U", "-c", str(count), "-i", "br0",
                              "dst", PEER, "and", "(port", "111", "or", "port", "2049)"],
                             stdout=out, stderr=subprocess.STDOUT)
        time.sleep(1)
        return p

    def make_data(self, directory, name, size):
        data = os.urandom(size)
        with open(f"{directory}/{name}", "wb") as f:
            f.write(data)
        return data


class Guest:
    def __init__(self, ctx, name, mac):
        self.ctx, self.name, self.mac = ctx, name, mac
        self.dir = f"{ctx.machines}/{name}"
        self.con = None

    def create(self, src):
        """A copy-on-write overlay of `src`'s disk; the original is never
        written. The copy is a clone and keeps the original's MAC, so it
        gets its own: machines on one LAN need distinct ones."""
        shutil.rmtree(self.dir, ignore_errors=True)
        shutil.copytree(src, self.dir, ignore=shutil.ignore_patterns(
            "system.qcow2", "machine.lock", "run.json"))
        subprocess.run([QEMU_IMG, "create", "-q", "-f", "qcow2", "-b", f"{src}/drives/system.qcow2",
                        "-F", "qcow2", f"{self.dir}/drives/system.qcow2"], check=True)
        toml = open(f"{self.dir}/machine.toml").read()
        toml, n = re.subn(r'(?m)^mac = ".*"$', f'mac = "{self.mac}"', toml)
        assert n == 1
        open(f"{self.dir}/machine.toml", "w").write(toml)
        return self

    def network(self, *args):
        self.ctx.origami("network-set", self.dir, *args)

    def start(self):
        self.ctx.origami("run", self.dir, "--display", "none", "--background")
        self.con = Console(self.dir, f"{self.ctx.evidence}/{self.name}-console.log")
        login(self.con, 1500)
        self.sh("true", timeout=120)
        return self

    def sh(self, command, timeout=120):
        rc, out = self.con.run(command, timeout=timeout)
        lines = out.split("\n")
        # The shell's next prompt arrives ahead of the output.
        return rc, re.sub(r"^(# )+", "", "\n".join(lines[1:]).strip())

    def must(self, command, timeout=120):
        rc, out = self.sh(command, timeout)
        print(f"[{self.name}] {command} -> {rc}: {out[:400]}", flush=True)
        assert rc == 0, f"{self.name}: {command!r} exited {rc}: {out}"
        return out

    def address(self, prefix, timeout=300):
        """The IPv4 address ef0 gets on `prefix`, waiting for DHCP. The
        installed system starts on the address it held during the install,
        so the first address seen is not the answer."""
        end = time.monotonic() + timeout
        out = ""
        while time.monotonic() < end:
            _, out = self.sh("ifconfig ef0")
            m = re.search(r"inet (\d+\.\d+\.\d+\.\d+)", out)
            if m and m.group(1).startswith(prefix):
                return m.group(1)
            time.sleep(10)
        raise AssertionError(f"{self.name}: ef0 got no {prefix}* address; last: {out}")

    def stop(self):
        """Shut IRIX down cleanly, then make sure QEMU is gone. Safe to call
        twice and on a guest that never started."""
        env = self.ctx.env
        try:
            if self.con:
                self.con.line("halt -p")
                end = time.monotonic() + 240
                while time.monotonic() < end:
                    status = subprocess.run([ORIGAMI, "status", self.dir], env=env,
                                            capture_output=True, text=True)
                    if status.stdout.strip() == "stopped":
                        break
                    time.sleep(5)
                self.con.close()
                self.con = None
        except Exception:
            pass
        if os.path.exists(f"{self.dir}/state/run.json"):
            subprocess.run([ORIGAMI, "stop", self.dir], env=env, capture_output=True)
            time.sleep(3)
