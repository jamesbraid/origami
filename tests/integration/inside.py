#!/usr/bin/env python3
"""Run one scenario inside the lab container; run.py starts this.

Writes /work/result.json and keeps everything the scenario and the peer
services logged in /work/evidence. The container is the unit of teardown:
guests are shut down in `finally`, and anything left dies with it.
"""
import json
import os
import signal
import sys
import time
import traceback

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from harness.guest import Ctx  # noqa: E402
from harness.lab import Lab  # noqa: E402
from harness.scenario import load_all  # noqa: E402


def main(name):
    scenario = load_all()[name]
    work = "/work"
    # A timeout or Ctrl-C ends the scenario through the same cleanup.
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(143))
    lab = Lab(logs=f"{work}/evidence")
    ctx = Ctx(work, os.environ["IMAGE_DIR"], lab)
    result = {"name": name, "summary": scenario.summary, "image": scenario.image.dirname,
              "timeout_s": scenario.timeout, "started": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())}
    t0 = time.monotonic()
    try:
        lab.start(taps=scenario.taps)
        result["data"] = scenario.run(ctx)
        result["status"] = "pass"
    except BaseException as e:  # noqa: BLE001
        traceback.print_exc()
        result["status"] = "fail"
        result["error"] = f"{type(e).__name__}: {e}"
    finally:
        try:
            ctx.teardown()
        finally:
            lab.stop()
        result["duration_s"] = round(time.monotonic() - t0)
        json.dump(result, open(f"{work}/result.json", "w"), indent=2)
    print(json.dumps(result, indent=2))
    return 0 if result["status"] == "pass" else 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1]))
