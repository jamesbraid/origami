"""A guest on a bridge: DHCP from the peer, ping, NFS mount, checksums."""
from harness import nfs
from harness.guest import EXPORT, PEER
from harness.images import ORIGIN200_MINIMAL
from harness.scenario import Scenario


def run(ctx):
    results = {}
    g = ctx.guest("tapguest", "08:00:69:e1:03:10")
    g.network("--mode", "tap", "--interface", "tap0", "--mac", g.mac)
    g.start()
    addr = g.address("10.77.0.")
    results["address"] = addr
    leases = open(f"{ctx.lab.logs}/dnsmasq.leases").read()
    assert addr in leases, f"peer DHCP never leased {addr}: {leases!r}"
    g.must(f"ping -c 3 {PEER}")
    dump = ctx.capture("tap-mount-packets")
    nfs.roundtrip(ctx, g, PEER, EXPORT, results)
    dump.terminate()
    return results


SCENARIO = Scenario(
    name="lan-tap",
    summary="tap guest: DHCP, ping, NFS mount, checksummed read and write",
    image=ORIGIN200_MINIMAL, taps=1, timeout=3600, run=run)
