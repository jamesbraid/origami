#!/usr/bin/env python3
"""Boot a prepared IRIX guest with a product archive and check that it works.

    tests/smoke/boot.py --product DIR --work DIR [--results FILE]

--product is an extracted product tree (`bin/origami`, `libexec/origami/`) or
the release archive itself (.tar.gz or .zip), which is extracted into --work.
The prepared Origin 200 disk is named in disk.json by URL
and SHA-256; it is downloaded, verified and unpacked into --work. Everything
here is plain Python and user-mode networking, so it runs the same on Linux,
macOS and Windows.

Checks: login, hinv, ping the user-network gateway, an NFS mount of
`origami share` with read and write checksums, a CD-ROM read checksum and a
clean `halt -p`. A failed check is recorded, not raised, and the results file
is always written before the exit status is set: 1 when anything failed, 0
when every check passed.
"""
import argparse
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import tarfile
import time
import urllib.request
import zipfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent / "integration"))
from console import Console, Timeout, login  # noqa: E402

GATEWAY = "10.98.0.2"
NFS_SERVER = "10.98.0.4"
SHARE = "smoke"
CD_TARGET = "4"
CD_BYTES = 1 << 20


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


class Results:
    """What the run found. Written on every exit path."""

    def __init__(self, path):
        self.path = path
        self.data = {"stage": "start", "status": "fail", "error": None,
                     "checks": {}, "attempts": 0, "started": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())}
        self.t0 = time.monotonic()

    def check(self, name, ok, detail="", seconds=None):
        self.data["checks"][name] = {"ok": bool(ok), "detail": str(detail)[-400:],
                                     **({"seconds": round(seconds, 1)} if seconds is not None else {})}
        print(f"[check] {name}: {'ok' if ok else 'FAIL'} {str(detail)[-200:]}", flush=True)

    def write(self):
        self.data["duration_s"] = round(time.monotonic() - self.t0)
        checks = self.data["checks"]
        if self.data["error"] is None and checks and all(c["ok"] for c in checks.values()):
            self.data["status"] = "pass"
        else:
            self.data["status"] = "fail"
        tmp = f"{self.path}.tmp"
        Path(tmp).write_text(json.dumps(self.data, indent=2) + "\n")
        os.replace(tmp, self.path)


def fetch(pin, dest):
    """Download the prepared disk, check its hash, return the local path."""
    dest.mkdir(parents=True, exist_ok=True)
    path = dest / pin["url"].rsplit("/", 1)[-1]
    if path.exists() and sha256(path) == pin["sha256"]:
        return path
    last = None
    for attempt in range(4):
        try:
            print(f"downloading {pin['url']} (attempt {attempt + 1})", flush=True)
            h = hashlib.sha256()
            with urllib.request.urlopen(pin["url"], timeout=60) as r, open(path, "wb") as f:
                while True:
                    block = r.read(1 << 20)
                    if not block:
                        break
                    h.update(block)
                    f.write(block)
            if h.hexdigest() != pin["sha256"]:
                raise RuntimeError(f"sha256 {h.hexdigest()} is not the pinned {pin['sha256']}")
            return path
        except Exception as e:  # noqa: BLE001
            last = e
            print(f"download failed: {e}", flush=True)
            time.sleep(5 * (attempt + 1))
    raise RuntimeError(f"download failed: {last}")


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for block in iter(lambda: f.read(1 << 20), b""):
            h.update(block)
    return h.hexdigest()


def unpack(archive, dest):
    """A fresh machine directory from the archive, whose single top-level
    directory is `machine`."""
    shutil.rmtree(dest, ignore_errors=True)
    dest.mkdir(parents=True)
    with tarfile.open(archive, "r:xz") as tf:
        tf.extractall(dest, filter="data")
    return dest / "machine"


def extract_product(archive, dest):
    """The product tree inside a release archive: its one top-level directory."""
    shutil.rmtree(dest, ignore_errors=True)
    dest.mkdir(parents=True)
    if archive.suffix == ".zip":
        with zipfile.ZipFile(archive) as zf:
            zf.extractall(dest)
    else:
        with tarfile.open(archive, "r:gz") as tf:
            tf.extractall(dest, filter="data")
    tops = [p for p in dest.iterdir() if p.is_dir()]
    if len(tops) != 1:
        raise RuntimeError(f"{archive.name} should hold one directory, found {[p.name for p in tops]}")
    return tops[0]


class Product:
    def __init__(self, root, work):
        exe = ".exe" if os.name == "nt" else ""
        self.cli = str(Path(root).resolve() / "bin" / f"origami{exe}")
        self.work = work.resolve()
        # Keep the product's cache and config out of the runner's profile.
        home = work / "home"
        home.mkdir(parents=True, exist_ok=True)
        self.env = dict(os.environ, HOME=str(home), XDG_CACHE_HOME=str(work / "xdg"),
                        USERPROFILE=str(home), LOCALAPPDATA=str(home / "local"),
                        APPDATA=str(home / "roaming"))
        for name in ("ORIGAMI_RUNTIME_DIR", "LD_LIBRARY_PATH", "DYLD_LIBRARY_PATH"):
            self.env.pop(name, None)

    def origami(self, *args, check=True, timeout=120):
        r = subprocess.run([self.cli, *map(str, args)], env=self.env, text=True,
                           capture_output=True, timeout=timeout, cwd=self.work)
        print("+ origami", *args, "->", r.returncode, (r.stdout + r.stderr).strip()[-300:], flush=True)
        if check and r.returncode:
            raise RuntimeError(f"origami {args[0]}: {r.stderr.strip()[-300:]}")
        return r


class Guest:
    def __init__(self, product, machine, log):
        self.p, self.dir, self.log, self.con = product, machine, log, None

    def start(self, timeout):
        self.p.origami("run", self.dir, "--display", "none", "--background")
        end = time.monotonic() + 60
        while True:
            try:
                self.con = Console(self.dir, self.log)
                break
            except (OSError, ValueError):  # run.json or the port is not there yet
                if time.monotonic() > end:
                    raise
                time.sleep(1)
        login(self.con, timeout)
        self.sh("true")

    def sh(self, command, timeout=120):
        rc, out = self.con.run(command, timeout=timeout)
        lines = out.split("\n")
        # The shell's next prompt arrives ahead of the output.
        return rc, re.sub(r"^(# )+", "", "\n".join(lines[1:]).strip())

    def stopped(self):
        return self.p.origami("status", self.dir, check=False).stdout.strip() == "stopped"

    def halt(self, timeout=240):
        """Shut IRIX down; True if QEMU exited on its own."""
        self.con.line("halt -p")
        end = time.monotonic() + timeout
        while time.monotonic() < end:
            if self.stopped():
                return True
            time.sleep(5)
        return False

    def kill(self):
        try:
            if self.con:
                self.con.close()
        except Exception:  # noqa: BLE001
            pass
        self.con = None
        if not self.stopped():
            self.p.origami("stop", self.dir, check=False)
            time.sleep(3)


def make_cd(path):
    data = os.urandom(CD_BYTES)
    path.write_bytes(data)
    return data


def checks(g, results, share, cd_data, deadline):
    """Run every check on a logged-in guest; each one's failure is recorded."""
    def timed(name, fn):
        t = time.monotonic()
        try:
            ok, detail = fn()
        except Exception as e:  # noqa: BLE001
            ok, detail = False, f"{type(e).__name__}: {e}"
        results.check(name, ok, detail, time.monotonic() - t)
        if time.monotonic() > deadline:
            raise TimeoutError("overall deadline reached")
        return ok

    def hinv():
        rc, out = g.sh("hinv")
        want = {"cpu": r"R10000", "memory": r"256 Mbyte", "cdrom": r"CDROM", "ethernet": r"ef0|Ethernet"}
        missing = [k for k, rx in want.items() if not re.search(rx, out)]
        return rc == 0 and not missing, f"missing {missing}: {out}" if missing else out[:300]

    def address():
        end = time.monotonic() + 300
        out = ""
        while time.monotonic() < end:
            _, out = g.sh("ifconfig ef0")
            m = re.search(r"inet (10\.98\.0\.\d+)", out)
            if m:
                return True, m.group(1)
            time.sleep(10)
        return False, out

    def ping():
        rc, out = g.sh(f"ping -c 3 {GATEWAY}", timeout=60)
        return rc == 0 and "0% packet loss" in out, out[-200:]

    def nfs_mount():
        rc, out = g.sh(f"mkdir -p /mnt/s; mount {NFS_SERVER}:/{SHARE} /mnt/s", timeout=240)
        return rc == 0, out

    def nfs_read():
        rc, out = g.sh("cksum /mnt/s/data.bin", timeout=300)
        got = " ".join(out.split()[:2])
        return rc == 0 and got == posix_cksum(share["data"]), f"guest {got}"

    def nfs_write():
        rc, out = g.sh("dd if=/mnt/s/data.bin of=/mnt/s/copy.bin bs=8k; sync", timeout=300)
        # A file the guest makes itself, so the checksum is not of data it only copied.
        rc2, _ = g.sh("dd if=/unix bs=32k count=64 of=/mnt/s/guest.bin; sync", timeout=300)
        _, local = g.sh("dd if=/unix bs=32k count=64 2>/dev/null | cksum", timeout=120)
        copy_ok = (share["dir"] / "copy.bin").read_bytes() == share["data"]
        want = posix_cksum((share["dir"] / "guest.bin").read_bytes())
        guest_ok = " ".join(local.split()[:2]) == want
        g.sh("umount /mnt/s")
        return rc == 0 and rc2 == 0 and copy_ok and guest_ok, f"copy {copy_ok}, guest file {guest_ok}"

    def cdrom():
        # The device needs 2048-byte reads.
        rc, out = g.sh(f"dd if=/dev/rdsk/dks0d{CD_TARGET}vol bs=2048 count={CD_BYTES // 2048} 2>/dev/null | cksum",
                       timeout=300)
        got = " ".join(out.split()[:2])
        return rc == 0 and got == posix_cksum(cd_data), f"guest {got}"

    timed("hinv", hinv)
    if timed("network_address", address):
        timed("ping_gateway", ping)
    if timed("nfs_mount", nfs_mount):
        timed("nfs_read_checksum", nfs_read)
        timed("nfs_write_checksum", nfs_write)
    timed("cdrom_read_checksum", cdrom)


RETRY_NEEDS = 600


def attempt(n, args, results, pin_archive, product, deadline):
    """One boot of a freshly unpacked disk. Returns True if the guest reached
    a shell (a hang is worth another try; a failed check is not)."""
    work = args.work / f"run{n}"
    machine = unpack(pin_archive, work)
    evidence = args.work / "evidence"
    evidence.mkdir(exist_ok=True)
    g = Guest(product, machine, evidence / f"console-{n}.log")
    share_dir = work / "share"
    share_dir.mkdir()
    data = os.urandom(1 << 20)
    (share_dir / "data.bin").write_bytes(data)
    iso = work / "cdrom.img"
    cd_data = make_cd(iso)
    try:
        product.origami("network-set", machine, "--mode", "user")
        product.origami("share", machine, SHARE, share_dir)
        product.origami("drive-attach", machine, iso, "--type", "cdrom", "--target", CD_TARGET)
        results.data["stage"] = "boot"
        t = time.monotonic()
        g.start(min(args.login_timeout, max(60, int(deadline - time.monotonic()))))
        results.check("login", True, "root shell on the serial console", time.monotonic() - t)
    except (Timeout, EOFError, RuntimeError, OSError, subprocess.TimeoutExpired) as e:
        results.check("login", False, f"{type(e).__name__}: {e}")
        g.kill()
        return False
    try:
        results.data["stage"] = "checks"
        checks(g, results, {"dir": share_dir, "data": data}, cd_data,
               min(time.monotonic() + args.check_timeout, deadline))
        results.data["stage"] = "halt"
        t = time.monotonic()
        results.check("halt", g.halt(max(30, min(240, int(deadline - time.monotonic())))),
                      "halt -p until QEMU exited", time.monotonic() - t)
    except Exception as e:  # noqa: BLE001
        results.data["error"] = f"{type(e).__name__}: {e}"
    finally:
        g.kill()
    return True


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--product", type=Path, required=True)
    ap.add_argument("--work", type=Path, required=True)
    ap.add_argument("--pin", type=Path, default=HERE / "disk.json")
    ap.add_argument("--results", type=Path)
    ap.add_argument("--attempts", type=int, default=2, help="boots to try if the guest never reaches a shell")
    ap.add_argument("--login-timeout", type=int, default=900)
    ap.add_argument("--check-timeout", type=int, default=900)
    ap.add_argument("--budget", type=int, default=1260,
                    help="seconds for the whole run, download included; a second boot starts only if "
                         "RETRY_NEEDS seconds are left. The workflow step's own timeout is just above it")
    args = ap.parse_args()
    args.work = args.work.resolve()
    args.work.mkdir(parents=True, exist_ok=True)
    results = Results(args.results or args.work / "results.json")
    pin = json.loads(args.pin.read_text())
    results.data["disk"] = {k: pin[k] for k in ("name", "url", "sha256")}
    results.data["platform"] = sys.platform
    try:
        results.data["stage"] = "download"
        archive = fetch(pin, args.work / "download")
        root = args.product
        if root.is_file():
            results.data["stage"] = "extract"
            root = extract_product(root, args.work / "product")
        product = Product(root, args.work)
        results.data["stage"] = "version"
        results.data["version"] = product.origami("version").stdout.strip()[:400]
        deadline = results.t0 + args.budget
        for n in range(1, args.attempts + 1):
            if n > 1 and deadline - time.monotonic() < RETRY_NEEDS:
                print("not enough of the budget left for another boot", flush=True)
                break
            results.data["attempts"] = n
            results.data["checks"] = {}
            if attempt(n, args, results, archive, product, deadline):
                break
        results.data["stage"] = "done"
    except BaseException as e:  # noqa: BLE001
        results.data["error"] = f"{type(e).__name__}: {e} (during {results.data['stage']})"
        print(results.data["error"], file=sys.stderr, flush=True)
    finally:
        results.write()
    print(json.dumps(results.data, indent=2))
    return 0 if results.data["status"] == "pass" else 1


if __name__ == "__main__":
    sys.exit(main())
