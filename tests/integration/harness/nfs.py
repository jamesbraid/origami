"""NFS checks shared by scenarios."""
from harness.guest import posix_cksum


def roundtrip(ctx, g, server, export, results):
    """Mount `server:export` on the guest, read a peer file and write files
    back, checking checksums on both sides. `export` is also the peer's
    local path to the same directory."""
    data = ctx.make_data(export, "data.bin", 1 << 20)
    want = posix_cksum(data)
    g.must("mkdir -p /mnt/x")
    g.must(f"mount {server}:{export} /mnt/x", timeout=180)
    got = g.must("cksum /mnt/x/data.bin").split()
    assert " ".join(got[:2]) == want, f"read: guest {got} peer {want}"
    results["read_checksum"] = want
    g.must("dd if=/mnt/x/data.bin of=/mnt/x/copy.bin bs=8k", timeout=300)
    g.must("sync")
    assert open(f"{export}/copy.bin", "rb").read() == data, "peer's copy of the written file differs"
    # A file made on the guest.
    g.must("dd if=/unix bs=32k count=64 of=/mnt/x/guest.bin", timeout=300)
    g.must("sync")
    local = g.must("dd if=/unix bs=32k count=64 2>/dev/null | cksum").split()
    peer = posix_cksum(open(f"{export}/guest.bin", "rb").read())
    assert " ".join(local[:2]) == peer, f"write: guest {local} peer {peer}"
    results["write_checksum"] = peer
    g.must("umount /mnt/x")
