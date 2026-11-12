"""Two guests on one bridge: distinct identities, a shared export, and
guest-to-guest NFS."""
import shutil
import time

from harness.guest import EXPORT, PEER, posix_cksum
from harness.images import ORIGIN200_MINIMAL
from harness.scenario import Scenario


def run(ctx):
    results = {}
    a = ctx.guest("guesta", "08:00:69:e1:03:31")
    b = ctx.guest("guestb", "08:00:69:e1:03:32")
    a.network("--mode", "tap", "--interface", "tap0", "--mac", a.mac)
    b.network("--mode", "tap", "--interface", "tap1", "--mac", b.mac)
    # A new machine may not take a MAC a sibling already uses.
    dup = ctx.origami("create", f"{ctx.machines}/dupmac", "--preset", "origin200-1",
                      "--mac", a.mac, check=False)
    assert dup.returncode != 0 and "collides" in dup.stderr, dup.stderr
    results["duplicate_mac_refused"] = dup.stderr.strip()
    # A copied directory is a clone with the same MAC. Record whether
    # validate, which `run` also performs, notices.
    clone = ctx.guest("clone", a.mac)
    n = ctx.origami("network-set", clone.dir, "--mode", "tap", "--interface", "tap1", check=False)
    v = ctx.origami("validate", clone.dir, check=False)
    results["clone_with_same_mac"] = {
        "network_set": [n.returncode, (n.stdout + n.stderr).strip()],
        "validate": [v.returncode, (v.stdout + v.stderr).strip()]}
    ctx.guests.remove(clone)
    shutil.rmtree(clone.dir)

    a.start()
    b.start()
    addr_a, addr_b = a.address("10.77.0."), b.address("10.77.0.")
    assert addr_a != addr_b, (addr_a, addr_b)
    results["addresses"] = [addr_a, addr_b]
    leases = open(f"{ctx.lab.logs}/dnsmasq.leases").read()
    for g, addr in ((a, addr_a), (b, addr_b)):
        assert f"{g.mac} {addr} " in leases, f"no lease of {addr} to {g.mac}: {leases!r}"
    results["leases"] = leases.strip().split("\n")
    for g in (a, b):
        g.must(f"ping -c 2 {PEER}")
    a.must(f"ping -c 2 {addr_b}")
    for g in (a, b):
        g.must("mkdir -p /mnt/x")
        g.must(f"mount {PEER}:{EXPORT} /mnt/x", timeout=180)
    data = ctx.make_data(EXPORT, "shared.bin", 256 << 10)
    for g in (a, b):
        got = g.must("cksum /mnt/x/shared.bin").split()
        assert " ".join(got[:2]) == posix_cksum(data)
    # Guest to guest: A exports a directory, B mounts it.
    a.must("mkdir -p /export/a && echo 'from guest a' > /export/a/hello.txt && chmod 777 /export/a")
    a.must("echo '/export/a -rw' >> /etc/exports")
    a.must("chkconfig nfs on; /etc/init.d/nfs start; exportfs -a; exportfs", timeout=300)
    time.sleep(15)
    out = b.must(f"mkdir -p /mnt/a && mount {addr_a}:/export/a /mnt/a && cat /mnt/a/hello.txt",
                 timeout=300)
    assert "from guest a" in out
    b.must("echo 'from guest b' > /mnt/a/reply.txt; sync")
    assert "from guest b" in a.must("cat /export/a/reply.txt")
    results["guest_to_guest_nfs"] = "ok"
    return results


SCENARIO = Scenario(
    name="two-guests",
    summary="two guests on one bridge: distinct MACs and leases, shared export, guest-to-guest NFS",
    image=ORIGIN200_MINIMAL, taps=2, timeout=5400, run=run)
