#!/usr/bin/env python3
"""Run the integration scenarios against a product build.

    tests/integration/run.py --product DIR --work DIR [--cache DIR]
        [--evidence DIR] [--jobs N] [--list] [scenario ...]

DIR for --product is a runnable product tree (`bin/origami`,
`libexec/origami/`), such as the build directory's `run/`. Each scenario
declares the installed image it needs (harness/images.py). Missing images
are installed once into --cache, which is shared by every scenario and run;
then each scenario runs in its own container and network namespace, at most
--jobs at a time, with its own timeout. Results are DIR/<scenario>/result.json
and evidence beside it, and DIR/summary.json. See README.md.
"""
import argparse
import json
import os
import shlex
import subprocess
import sys
import time
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
from harness.scenario import load_all  # noqa: E402

IMAGE = "localhost/origami-net-lab"
INSTALL_TIMEOUT = 4 * 3600


def run(cmd, **kw):
    print("+", shlex.join(map(str, cmd)), flush=True)
    return subprocess.run(cmd, **kw)


def common(engine, product, work, cache, name):
    return [*engine, "run", "--rm", "--pids-limit=-1", "--name", name,
            "-v", f"{product}:/product:ro", "-v", f"{HERE}:/it:ro",
            "-v", f"{work}:/work", "-v", f"{cache}:/cache",
            "-e", "PYTHONUNBUFFERED=1", "-w", "/work"]


def install(engine, product, work, cache, spec):
    """Install one image, in a container with outside network access for the
    public media and no tap devices."""
    d = cache / spec.dirname
    if (d / "DONE").exists():
        return 0
    w = work / f"install-{spec.dirname}"
    w.mkdir(parents=True, exist_ok=True)
    cmd = common(engine, product, w, cache, f"it-install-{spec.key}")
    cmd += ["-e", f"DISK_DIR=/cache/{spec.dirname}", "-e", f"PRESET={spec.preset}",
            "-e", f"PROFILE={spec.profile}", "-e", f"DISK_MB={spec.disk_mb}", IMAGE,
            "timeout", "-k", "60", str(INSTALL_TIMEOUT), "python3", "/it/install.py"]
    return run(cmd).returncode


def scenario(engine, product, work, cache, evidence, s):
    """One scenario in a fresh container: no outside network, its own
    namespace for the bridge and taps. The timeout is enforced inside, so
    the container, its guests and its network go with it."""
    w = work / s.name
    if w.exists():
        run(["rm", "-rf", str(w)])
    w.mkdir(parents=True)
    cmd = common(engine, product, w, cache, f"it-{s.name}-{os.getpid()}")
    cmd += ["-e", f"IMAGE_DIR=/cache/{s.image.dirname}",
            "--network", "none", "--cap-add", "NET_ADMIN", "--cap-add", "NET_RAW",
            "--cap-add", "SYS_ADMIN", "--device", "/dev/net/tun", IMAGE,
            "timeout", "-k", "60", str(s.timeout), "python3", "/it/inside.py", s.name]
    t0 = time.monotonic()
    code = run(cmd).returncode
    result_file = w / "result.json"
    if result_file.exists():
        result = json.loads(result_file.read_text())
    else:
        reason = "timed out" if code in (124, 137) else f"container exited {code} without a result"
        result = {"name": s.name, "status": "fail", "error": reason,
                  "duration_s": round(time.monotonic() - t0)}
    if evidence:
        dest = evidence / s.name
        dest.mkdir(parents=True, exist_ok=True)
        (dest / "result.json").write_text(json.dumps(result, indent=2))
        run(["cp", "-r", f"{w}/evidence/.", str(dest)])
    return result


def main():
    scenarios = load_all()
    ap = argparse.ArgumentParser()
    ap.add_argument("--product", type=Path)
    ap.add_argument("--engine", default=os.environ.get("ORIGAMI_IT_ENGINE", "podman"))
    ap.add_argument("--work", type=Path)
    ap.add_argument("--cache", type=Path, default=Path.home() / ".cache/origami-integration")
    ap.add_argument("--evidence", type=Path)
    ap.add_argument("--jobs", type=int, default=1, help="scenarios to run at once")
    ap.add_argument("--list", action="store_true")
    ap.add_argument("names", nargs="*", metavar="scenario")
    args = ap.parse_args()
    if args.list:
        for s in scenarios.values():
            print(f"{s.name:12} {s.image.dirname}  {s.summary}")
        return 0
    if not args.product or not args.work:
        ap.error("--product and --work are required")
    unknown = set(args.names) - set(scenarios)
    if unknown:
        ap.error(f"unknown scenario: {', '.join(sorted(unknown))}")
    chosen = [scenarios[n] for n in (args.names or scenarios)]
    engine = shlex.split(args.engine)
    product, work, cache = (p.resolve() for p in (args.product, args.work, args.cache))
    for d in (work, cache):
        d.mkdir(parents=True, exist_ok=True)

    # Layers are cached, so rebuilding is cheap. CI's engine wrapper only
    # passes `build` and `run`, so there is no "does the image exist" probe.
    r = run([*engine, "build", "-t", IMAGE, "-f", str(HERE / "Containerfile"), str(HERE)])
    if r.returncode:
        return r.returncode
    for spec in dict.fromkeys(s.image for s in chosen):
        if install(engine, product, work, cache, spec):
            print(f"installing {spec.dirname} failed", file=sys.stderr)
            return 2
    with ThreadPoolExecutor(max_workers=max(1, args.jobs)) as pool:
        results = list(pool.map(
            lambda s: scenario(engine, product, work, cache, args.evidence, s), chosen))
    summary = {r["name"]: {k: r.get(k) for k in ("status", "duration_s", "error")} for r in results}
    print(json.dumps(summary, indent=2))
    if args.evidence:
        (args.evidence / "summary.json").write_text(json.dumps(summary, indent=2))
    return 0 if all(r["status"] == "pass" for r in results) else 1


if __name__ == "__main__":
    sys.exit(main())
