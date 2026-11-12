"""The same server from user-mode networking, through QEMU's NAT."""
import time

from harness.guest import EXPORT, EXPORT_OPEN, PEER, posix_cksum
from harness.images import ORIGIN200_MINIMAL
from harness.scenario import Scenario


def mounted_roundtrip(ctx, g, export, results):
    data = ctx.make_data(export, "data.bin", 1 << 20)
    got = g.must("cksum /mnt/x/data.bin", timeout=300).split()
    results["read_ok"] = " ".join(got[:2]) == posix_cksum(data)
    rc, _ = g.sh("dd if=/mnt/x/data.bin of=/mnt/x/copy.bin bs=8k", timeout=300)
    g.sh("sync")
    results["write_ok"] = rc == 0 and open(f"{export}/copy.bin", "rb").read() == data


def run(ctx):
    results = {}
    g = ctx.guest("natguest", "08:00:69:e1:03:20")
    g.network("--mode", "user")
    g.start()
    results["address"] = g.address("10.98.0.")
    rc, out = g.sh(f"ping -c 3 {PEER}")
    results["ping_rc"] = rc
    results["ping"] = out[-300:]
    for label, export in (("secure_export", EXPORT), ("insecure_export", EXPORT_OPEN)):
        dump = ctx.capture(f"nat-{label}-packets")
        rc, out = g.sh(f"mkdir -p /mnt/x; mount {PEER}:{export} /mnt/x", timeout=240)
        time.sleep(2)
        dump.terminate()
        results[label] = {"mount_rc": rc, "output": out[-400:]}
        if rc == 0:
            try:
                mounted_roundtrip(ctx, g, export, results[label])
            finally:
                g.sh("umount /mnt/x")
    assert results["ping_rc"] == 0, "ping through NAT failed"
    ok = results["insecure_export"]
    assert ok["mount_rc"] == 0 and ok["read_ok"] and ok["write_ok"], ok
    # A server that insists on a reserved client port refuses NAT'd requests,
    # whose source port is the host's. Record which it was.
    secure = results["secure_export"]
    if secure["mount_rc"]:
        assert "Permission denied" in secure["output"], secure
        results["nat_limit"] = ("the peer refuses mounts from unprivileged source "
                                "ports, and NAT gives every guest packet one")
    return results


SCENARIO = Scenario(
    name="nat-user",
    summary="user-mode guest reaches the LAN server through NAT; what NFS needs",
    image=ORIGIN200_MINIMAL, taps=0, timeout=3600, run=run)
