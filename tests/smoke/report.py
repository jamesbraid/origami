#!/usr/bin/env python3
"""Turn smoke results into per-OS verdicts and shields.io badge files.

    report.py job --os OS --out FILE [--archive FILE] [--results FILE] [--note TEXT]
    report.py badges --results DIR --out DIR --run-url URL

`job` runs at the end of each OS's smoke job and merges what the archive build
and boot.py found into one verdict, whatever went wrong earlier: a missing
file is itself a failure to report, not an error. `badges` runs once after
every OS has finished, reads the verdicts and writes endpoint JSON that
shields.io draws (https://shields.io/badges/endpoint-badge), one file per OS
and `smoke.json` for the whole matrix. Neither exits non-zero on a failing
verdict: the workflow run stays green and the badge carries the result.
"""
import argparse
import json
import time
from pathlib import Path

OSES = ("linux", "macos", "windows")


def load(path):
    try:
        return json.loads(Path(path).read_text())
    except (OSError, ValueError, TypeError):
        return None


def job(args):
    verdict = {"os": args.os, "status": "fail", "reason": "", "date": time.strftime("%Y-%m-%d", time.gmtime())}
    archive = load(args.archive) if args.archive else None
    results = load(args.results) if args.results else None
    if args.note:
        verdict["reason"] = args.note
    elif archive is None or archive.get("outcome") != "success":
        verdict["reason"] = "product archive did not build"
    elif results is None:
        verdict["reason"] = "boot.py wrote no results"
    elif results.get("status") == "pass":
        verdict["status"] = "pass"
    else:
        failed = [k for k, c in results.get("checks", {}).items() if not c["ok"]]
        verdict["reason"] = (f"failed checks: {', '.join(failed)}" if failed
                             else results.get("error") or f"stopped in {results.get('stage')}")
    if results:
        verdict["duration_s"] = results.get("duration_s")
        verdict["checks"] = {k: c["ok"] for k, c in results.get("checks", {}).items()}
        verdict["attempts"] = results.get("attempts")
    Path(args.out).write_text(json.dumps(verdict, indent=2) + "\n")
    print(json.dumps(verdict, indent=2))


def badge(label, message, color):
    return {"schemaVersion": 1, "label": label, "message": message, "color": color, "cacheSeconds": 900}


def badges(args):
    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    stamp = time.strftime("%-d %b", time.gmtime())
    verdicts = {}
    for name in OSES:
        v = load(Path(args.results) / f"verdict-{name}.json")
        # No verdict at all means the job itself died: that is red too.
        verdicts[name] = v or {"os": name, "status": "fail", "reason": "job produced no verdict"}
        ok = verdicts[name]["status"] == "pass"
        (out / f"smoke-{name}.json").write_text(json.dumps(
            badge(name, f"passing, {stamp}" if ok else f"failing, {stamp}", "brightgreen" if ok else "red")) + "\n")
    passed = [n for n in OSES if verdicts[n]["status"] == "pass"]
    if len(passed) == len(OSES):
        overall = badge("smoke", f"passing, {stamp}", "brightgreen")
    elif not passed:
        overall = badge("smoke", f"failing, {stamp}", "red")
    else:
        overall = badge("smoke", f"{len(passed)}/{len(OSES)} passing, {stamp}", "orange")
    (out / "smoke.json").write_text(json.dumps(overall) + "\n")
    (out / "status.json").write_text(json.dumps(
        {"run": args.run_url, "date": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
         "verdicts": verdicts}, indent=2) + "\n")
    print(json.dumps({n: v["status"] for n, v in verdicts.items()}))


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    j = sub.add_parser("job")
    j.add_argument("--os", required=True)
    j.add_argument("--out", required=True)
    j.add_argument("--archive")
    j.add_argument("--results")
    j.add_argument("--note")
    j.set_defaults(fn=job)
    b = sub.add_parser("badges")
    b.add_argument("--results", required=True)
    b.add_argument("--out", required=True)
    b.add_argument("--run-url", default="")
    b.set_defaults(fn=badges)
    args = ap.parse_args()
    args.fn(args)


if __name__ == "__main__":
    main()
