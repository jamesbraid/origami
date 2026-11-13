#!/usr/bin/env python3
"""Turn an installed integration image into the smoke test's prepared disk.

    tests/smoke/prepare_disk.py --product DIR --image DIR --work DIR --out DIR

--image is an entry of the integration cache, as `tests/integration/run.py`
leaves it after installing (`irix-origin200-1-minimal-<key>`, holding
`machine/` and `DONE`). One boot removes the install network's DHCP lease and
resolver file, which IRIX otherwise keeps on any other network and loops on,
then halts cleanly. The machine directory is packed as
`o200-irix-6.5.30-minimal-<sha8>.tar.xz` beside `<name>.sha256` and
`<name>.manifest.json`, the form tools/publish-asset takes. Publishing and the
pin in disk.json are left to the person running this.
"""
import argparse
import hashlib
import json
import shutil
import sys
import tarfile
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
from boot import Guest, Product  # noqa: E402

SKIP = {"machine.lock", "run.json", "install", "logs"}


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--product", type=Path, required=True)
    ap.add_argument("--image", type=Path, required=True)
    ap.add_argument("--work", type=Path, required=True)
    ap.add_argument("--out", type=Path, required=True)
    args = ap.parse_args()
    args.work, args.out, args.image = (p.resolve() for p in (args.work, args.out, args.image))
    shutil.rmtree(args.work, ignore_errors=True)
    args.work.mkdir(parents=True)
    args.out.mkdir(parents=True, exist_ok=True)
    machine = args.work / "machine"
    shutil.copytree(args.image / "machine", machine, ignore=lambda d, names: SKIP & set(names))
    product = Product(args.product, args.work)
    product.origami("network-set", machine, "--mode", "user")

    g = Guest(product, machine, args.work / "prepare-console.log")
    try:
        g.start(1800)
        rc, out = g.sh("ls /var/adm/proclaim.lease_info* /etc/resolv.conf")
        print("before:", out, flush=True)
        rc, out = g.sh("rm -f /var/adm/proclaim.lease_info* /etc/resolv.conf; sync; ls /var/adm/proclaim.lease_info* /etc/resolv.conf 2>&1")
        assert "No such file" in out, out
        assert g.halt(), "IRIX did not power the machine off"
    finally:
        g.kill()
    version = product.origami("version").stdout.strip()

    tmp = args.out / "pack.tar.xz"
    # Reproducible: fixed owner and modification time, sorted names.
    def clean(info):
        info.uid = info.gid = 0
        info.uname = info.gname = ""
        info.mtime = 0
        return info
    with tarfile.open(tmp, "w:xz", preset=6) as tf:
        for path in sorted(machine.rglob("*")):
            rel = path.relative_to(args.work)
            if path.name in SKIP:
                continue
            tf.add(path, arcname=rel.as_posix(), recursive=False, filter=clean)
    h = hashlib.sha256()
    with open(tmp, "rb") as f:
        for block in iter(lambda: f.read(1 << 20), b""):
            h.update(block)
    digest = h.hexdigest()
    name = f"o200-irix-6.5.30-minimal-{digest[:8]}.tar.xz"
    final = args.out / name
    tmp.rename(final)
    (args.out / f"{name}.sha256").write_text(f"{digest}  {name}\n")
    manifest = {
        "name": name, "sha256": digest, "size": final.stat().st_size,
        "machine": "Origin 200 (preset origin200-1)", "irix": "6.5.30",
        "install_profile": "minimal", "disk_mb": 4096,
        "prepared": "booted once on user networking; DHCP lease files and /etc/resolv.conf removed; halt -p",
        "product_version": version,
        "date": time.strftime("%Y-%m-%d", time.gmtime()),
        "layout": "machine/ (machine.toml, state/, drives/system.qcow2)",
    }
    (args.out / f"{name}.manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    print(json.dumps(manifest, indent=2))


if __name__ == "__main__":
    main()
