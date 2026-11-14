#!/usr/bin/env python3
"""Install IRIX 6.5.30 on a fresh machine and leave it in the disk cache.

Runs once per cache key, in a container with outside network access for the
public media. The result is a machine directory, DISK_DIR/machine, whose
system disk the scenarios use as a read-only backing file.
"""
import os
import shutil
import signal
import subprocess
import sys
import time

sys.path.insert(0, os.path.dirname(__file__))
from capture import capture  # noqa: E402
from console import Console, Timeout, login  # noqa: E402

ORIGAMI = "/product/bin/origami"
PRESET = os.environ["PRESET"]
PROFILE = os.environ["PROFILE"]
DISK_MB = os.environ["DISK_MB"]
DISK_DIR = os.environ["DISK_DIR"]
WORK = "/work/install"
BASE = f"{WORK}/base"
LOGS = "/work/logs"
env = dict(os.environ, XDG_CACHE_HOME=f"{WORK}/xdg", HOME=f"{WORK}/home")


def origami(*args, **kw):
    print("+ origami", *args, flush=True)
    return subprocess.run([ORIGAMI, *args], check=True, env=env, cwd=WORK, **kw)


def main():
    if os.environ.get("RESUME"):
        # Debugging aid: boot an installed machine left in WORK and publish it.
        origami("run", BASE, "--display", "none", "--background")
        first_boot()
        subprocess.run([ORIGAMI, "stop", BASE], env=env)
        time.sleep(3)
        origami("network-set", BASE, "--mode", "user")
        return publish()
    # The outer `timeout` sends SIGTERM, then SIGKILL 60 seconds later. Turn
    # it into an exception so the capture below runs while QEMU and
    # Instigator are still up.
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(143))
    shutil.rmtree(WORK, ignore_errors=True)
    os.makedirs(f"{WORK}/xdg")
    os.makedirs(f"{WORK}/home")
    os.makedirs(LOGS, exist_ok=True)
    origami("create", BASE, "--preset", PRESET, "--memory-per-node", "256")
    origami("drive-create", BASE, DISK_MB)
    origami("install-init", BASE, "--profile", PROFILE)
    origami("install-check", BASE)
    serve = subprocess.Popen([ORIGAMI, "install-serve", BASE], env=env, cwd=WORK,
                             stdout=open(f"{LOGS}/install-serve.log", "wb"), stderr=subprocess.STDOUT)
    try:
        time.sleep(5)
        origami("run", BASE, "--display", "none", "--background")
        drive(Console(BASE, f"{LOGS}/install-console.log"))
        origami("install-apply", BASE)
        first_boot()
    except BaseException:
        capture("/work/evidence", BASE, serve.pid)
        raise
    finally:
        serve.terminate()
    subprocess.run([ORIGAMI, "stop", BASE], env=env)
    time.sleep(3)
    origami("network-set", BASE, "--mode", "user")
    publish()


def drive(con):
    con.expect(r"Option\? ", 300)
    con.send("5\r")
    con.expect(r">> ", 60)
    con.line("boot -f bootp():/6.5.30/stand/fx.64")
    # fx: extended mode, device name, controller and drive all take defaults.
    con.expect(r"extended mode with all options available\? \(no\)", 300)
    for prompt in (r"\(dksc\) ", r"ctlr# = \(0\) ", r"drive# = \(1\) "):
        con.send("\r")
        con.expect(prompt, 60)
    con.send("\r")
    con.expect(r"fx> ", 60)
    con.line("exit")
    con.expect(r"write out changes\? \(yes\)", 60)
    con.send("\r")
    con.expect(r"Option\? ", 120)
    con.send("2\r")
    con.expect(r"or <enter> to start: ", 60)
    con.send("1\r")
    con.expect(r"name of the remote host: ", 60)
    con.line("10.98.0.2")
    con.expect(r"(?i)directory.*: ", 60)
    con.line("/6.5.30/dist")
    con.expect(r"<enter> to start: ", 60)
    con.send("\r")
    # Keep answering until the miniroot reaches Inst>.
    answers = [
        (r"Make new file system on /dev/dsk/realroot \[yes/no/sh/help\]: $", "yes"),
        (r"Are you sure\? \[y/n\] \(n\): $", "y"),
        (r"Block size of filesystem 512 or 4096 bytes\?\s*$", "4096"),
        (r"hostname \(system name\) of your machine\?\s*$", "irix"),
        (r"network address of irix\?\s*$", "10.98.0.65"),
        (r"Press Enter for the IP class default \[0x[0-9a-f]+\]: $", "255.255.255.0"),
    ]
    end = time.monotonic() + 1800
    while time.monotonic() < end:
        try:
            con.expect(r"Inst> ", 20)
            break
        except Timeout:
            print("WAITING>>>", con.buffer[-600:], flush=True)
            inject = "/work/inject.txt"
            if os.path.exists(inject):
                con.send(open(inject).read().replace("\\r", "\r"))
                os.remove(inject)
                con.buffer = ""
                continue
            for pattern, answer in answers:
                if __import__("re").search(pattern, con.buffer[-300:]):
                    con.line(answer)
                    con.buffer = ""
                    break
    # Hand over at Inst>: install-apply types the package selection.
    con.close()


def publish():
    """Keep the installed machine, without its lock, console ports or install cache,
    and mark the cache entry complete."""
    final = f"{DISK_DIR}/machine"
    shutil.rmtree(DISK_DIR, ignore_errors=True)
    os.makedirs(DISK_DIR)
    shutil.copytree(BASE, final, ignore=shutil.ignore_patterns("machine.lock", "run.json", "install", "logs"))
    open(f"{DISK_DIR}/DONE", "w").write("ok\n")


def first_boot():
    """Boot the installed system once so IRIX does its one-time setup, then
    shut it down cleanly."""
    con = Console(BASE, f"{LOGS}/first-boot-console.log")
    # install-apply returns once the login prompt is up.
    login(con)
    con.line("halt -p")
    time.sleep(30)
    con.close()


if __name__ == "__main__":
    main()
