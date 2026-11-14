"""Evidence for an install that fails or times out.

An IRIX install that stalls leaves little behind: the console is quiet and the
logs stop. This records, in one small directory, what tells the layers apart:
the loopback socket between QEMU and Instigator (queue sizes show which side
stopped reading), QEMU's own view of the guest, what every thread of both
processes is waiting on, and a goroutine dump from Instigator. Every step is
bounded and failures are written down, never raised, because this runs while
the install is already being torn down.
"""
import json
import os
import signal
import socket
import subprocess
import time

# The outer `timeout -k 60` kills this process 60 seconds after it asks it to
# stop, so the whole capture has to stay well inside that: ss gets 5 seconds,
# the QEMU monitor 15 in total, and the Instigator dump 2 more.
SS_TIMEOUT = 5
HMP_BUDGET = 15


def _write(path, text):
    with open(path, "w") as f:
        f.write(text)


def _step(out, name, fn):
    try:
        _write(f"{out}/{name}.txt", fn())
    except Exception as e:  # noqa: BLE001
        _write(f"{out}/{name}.txt", f"capture failed: {type(e).__name__}: {e}\n")


def _descendants(root):
    """`root` and every process below it, found by parent pid. Nothing is
    chosen by program name, so another session's QEMU or Instigator on a
    shared host is never touched."""
    parent = {}
    for entry in os.listdir("/proc"):
        if not entry.isdigit():
            continue
        try:
            stat = open(f"/proc/{entry}/stat").read()
            parent[int(entry)] = int(stat.rsplit(")", 1)[1].split()[1])
        except (OSError, ValueError, IndexError):
            continue
    found = {root}
    grew = True
    while grew:
        grew = False
        for pid, ppid in parent.items():
            if ppid in found and pid not in found:
                found.add(pid)
                grew = True
    return sorted(p for p in found if p == root or p in parent)


def _cmdline(pid):
    try:
        argv = open(f"/proc/{pid}/cmdline", "rb").read().split(b"\0")
    except OSError:
        return "?"
    return b" ".join(argv).decode(errors="replace")[:200]


def _threads(pid):
    lines = []
    for tid in sorted(os.listdir(f"/proc/{pid}/task"), key=int):
        base = f"/proc/{pid}/task/{tid}"
        try:
            comm = open(f"{base}/comm").read().strip()
            # The state is the field after the parenthesised command name.
            state = open(f"{base}/stat").read().rsplit(")", 1)[1].split()[0]
            wchan = open(f"{base}/wchan").read().strip() or "-"
            lines.append(f"{tid} {comm} state={state} wchan={wchan}")
        except OSError:
            continue
    return "\n".join(lines)


def _hmp(machine_dir, commands):
    """Run HMP commands through QMP's human-monitor-command. The CLI has no
    monitor command, but QMP's loopback port is in the run record. Returns
    what it got when HMP_BUDGET runs out, since `info registers` can wait on
    a vCPU that is not running."""
    record = json.load(open(f"{machine_dir}/state/run.json"))
    deadline = time.monotonic() + HMP_BUDGET
    replies = []

    def remaining():
        left = deadline - time.monotonic()
        if left <= 0:
            raise TimeoutError(f"QEMU monitor budget of {HMP_BUDGET}s used up")
        return left

    try:
        with socket.create_connection(("127.0.0.1", record["qmp_port"]), remaining()) as sock:
            stream = sock.makefile("rw")

            def line():
                sock.settimeout(remaining())
                return stream.readline()

            def call(request):
                stream.write(json.dumps(request) + "\n")
                stream.flush()
                while True:
                    reply = json.loads(line())
                    if "return" in reply or "error" in reply:
                        return reply

            line()  # greeting
            call({"execute": "qmp_capabilities"})
            for command in commands:
                reply = call({"execute": "human-monitor-command",
                              "arguments": {"command-line": command}})
                replies.append(f"(qemu) {command}\n{reply.get('return', reply.get('error'))}")
    except (OSError, ValueError) as e:  # TimeoutError is an OSError
        replies.append(f"stopped: {type(e).__name__}: {e}")
    return "\n".join(replies)


def capture(out_dir, machine_dir, instigator_pid):
    """Save the evidence into `out_dir`, for the machine at `machine_dir`
    and the Instigator the caller started as `instigator_pid`. Only those and
    the processes below the machine's runner (its pid is in the run record)
    are looked at or signalled."""
    out = f"{out_dir}/install-stall"
    os.makedirs(out, exist_ok=True)
    _write(f"{out}/when.txt", time.strftime("%Y-%m-%dT%H:%M:%SZ\n", time.gmtime()))

    def ss():
        return subprocess.run(["ss", "-tanpi"], capture_output=True, text=True,
                              timeout=SS_TIMEOUT).stdout

    _step(out, "ss", ss)
    _step(out, "qemu-monitor", lambda: _hmp(
        machine_dir, ["info status", "info network", "info registers"]))

    def threads():
        runner = json.load(open(f"{machine_dir}/state/run.json"))["pid"]
        text = ""
        for label, group in (("qemu runner and children", _descendants(runner)),
                             ("instigator", [instigator_pid])):
            for pid in group:
                try:
                    text += f"== {label} pid {pid}: {_cmdline(pid)}\n{_threads(pid)}\n"
                except OSError as e:
                    text += f"== {label} pid {pid}: {e}\n"
        return text

    _step(out, "threads", threads)

    def quit_instigator():
        # Go prints every goroutine's stack to stderr on SIGQUIT and exits.
        # install-serve execs Instigator with stderr on its own server.log,
        # so find that file through the process before signalling it.
        log = os.readlink(f"/proc/{instigator_pid}/fd/2")
        os.kill(instigator_pid, signal.SIGQUIT)
        time.sleep(2)
        with open(log, "rb") as f:
            f.seek(0, os.SEEK_END)
            f.seek(max(0, f.tell() - 1_000_000))
            tail = f.read()
        with open(f"{out}/instigator-goroutines.txt", "wb") as f:
            f.write(tail)
        return f"sent SIGQUIT to {instigator_pid}; last 1 MB of {log} saved as instigator-goroutines.txt\n"

    _step(out, "instigator-sigquit", quit_instigator)
