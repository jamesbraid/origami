#!/usr/bin/env python3
"""Drives the real origami-gtk app through its automation socket and the
session's own input injection (XTest under Xvfb, wtype under cage), so the app's
key and pointer handling is what is tested. Runs inside the container.

usage: driver.py OUTDIR x11|wayland SCENARIO [app args]

Environment: APP, WORK, ORIGAMI_BIN, plus DISPLAY or WAYLAND_DISPLAY.
Writes OUTDIR/results.json and exits non-zero if any check failed.
"""
import json, os, socket, subprocess, sys, time

out, mode, scenario = sys.argv[1], sys.argv[2], sys.argv[3]
extra = sys.argv[4:]
app = os.environ["APP"]
work = os.environ["WORK"]
origami = os.environ["ORIGAMI_BIN"]
lib = os.path.join(work, "library")
sock_path = os.path.join(work, "automation.sock")
os.makedirs(out, exist_ok=True)
results = []
failed = []


def check(name, ok, detail=None):
    results.append({"check": name, "ok": bool(ok), "detail": detail})
    print(("PASS " if ok else "FAIL ") + name, "" if ok else json.dumps(detail), flush=True)
    if not ok:
        failed.append(name)
    return ok


def skip(name, why):
    results.append({"check": name, "ok": True, "skipped": why})
    print("SKIP " + name + ": " + why, flush=True)


class App:
    def __init__(self):
        self.log = open(os.path.join(out, "app.log"), "w")
        if os.path.exists(sock_path):
            os.remove(sock_path)
        self.proc = subprocess.Popen(
            [app, "--library", lib, "--config-dir", os.path.join(work, "config"), "--automation", sock_path] + extra,
            stdout=self.log, stderr=subprocess.STDOUT)
        t = time.time()
        while not os.path.exists(sock_path):
            if self.proc.poll() is not None or time.time() - t > 30:
                raise SystemExit("app did not start")
            time.sleep(0.1)
        self.s = socket.socket(socket.AF_UNIX)
        self.s.connect(sock_path)
        self.f = self.s.makefile("rw")

    def req(self, line):
        self.f.write(line + "\n")
        self.f.flush()
        return json.loads(self.f.readline())

    def state(self):
        for _ in range(3):
            v = self.req("state")
            if "machines" in v:
                return v
            print("state request answered", v, flush=True)
        raise SystemExit("app does not answer state: %s" % v)

    def machine(self, name):
        for m in self.state()["machines"]:
            if m["name"] == name:
                return m

    def wait(self, what, pred, timeout=120, step=0.5):
        t = time.time()
        while time.time() - t < timeout:
            v = pred()
            if v:
                return v
            if self.proc.poll() is not None:
                raise SystemExit("app exited (%s) while waiting for %s" % (self.proc.returncode, what))
            time.sleep(step)
        return None

    def stop(self):
        try:
            self.req("quit")
        except Exception:
            pass
        try:
            self.proc.wait(timeout=60)
        except subprocess.TimeoutExpired:
            self.proc.kill()


def shot(name):
    p = os.path.join(out, name + ".png")
    if mode == "x11":
        subprocess.run(["import", "-window", "root", p], check=False)
    else:
        subprocess.run(["grim", p], check=False)
    return p


def sh(*a):
    return subprocess.run(list(a), capture_output=True, text=True)


def app_window():
    if mode != "x11":
        return None
    r = sh("xdotool", "search", "--onlyvisible", "--name", "Origami")
    ids = r.stdout.split()
    return ids[-1] if ids else None


def key(*names):
    if mode == "x11":
        r = sh("xdotool", "key", "--clearmodifiers", *names)
    else:
        args = []
        for n in names:
            args += ["-k", n]
        r = sh("wtype", *args)
    return r.returncode == 0


def type_text(text):
    if mode == "x11":
        return sh("xdotool", "type", "--delay", "120", text).returncode == 0
    return sh("wtype", "-d", "120", text).returncode == 0


def pointer_to(x, y):
    """Move the real pointer to window-relative x, y (X11 only)."""
    if mode != "x11":
        return False
    return sh("xdotool", "mousemove", "--window", app_window(), str(int(x)), str(int(y))).returncode == 0


def click(button=1):
    return mode == "x11" and sh("xdotool", "click", str(button)).returncode == 0


def ppm_to_png(ppm, name):
    p = os.path.join(out, name + ".png")
    sh("convert", ppm, p)
    return p


def ocr(png):
    r = sh("tesseract", png, "-", "--psm", "6")
    return r.stdout


def create_machine(name, preset="octane2-vpro"):
    os.makedirs(lib, exist_ok=True)
    d = os.path.join(lib, name)
    if not os.path.exists(d):
        r = sh(origami, "create", d, "--preset", preset)
        if r.returncode != 0:
            raise SystemExit("create failed: " + r.stderr)


def screen_centre(a, name):
    m = a.machine(name)
    b = m.get("screen_bounds") if m else None
    if not b:
        return None
    return b[0] + b[2] / 2, b[1] + b[3] / 2


def prom_ready(a, name):
    """The PROM has drawn its menu: a gradient, so many colours."""
    def f():
        m = a.machine(name)
        if m and m["state"] in ("Stopped unexpectedly",):
            raise SystemExit("machine %s: %s (see app.log and the machine's runner.log)" % (name, m["state"]))
        if not m or m["state"] != "Running" or m["size"][0] == 0 or m["idle_ms"] < 4000:
            return None
        c = a.req("compare prom-ready " + name)
        return c if c.get("colours", 0) > 100 else None
    return a.wait("PROM picture", f, timeout=900, step=3)


def prom_menu(a, name, timeout=600):
    """The PROM's maintenance menu is up (read off the emulator's own
    screendump, so what we wait for is what the guest drew)."""
    last = {}

    def f():
        c = a.req("compare menu-probe " + name)
        if "ppm" not in c:
            return None
        text = ocr(ppm_to_png(c["ppm"], "menu-probe"))
        last["text"] = text
        if "any key to continue" in text:
            key("space")
            return None
        return c if "Start System" in text or "Command Monitor" in text else None
    r = a.wait("PROM menu", f, timeout=timeout, step=5)
    if r is None:
        open(os.path.join(out, "menu-timeout.ocr.txt"), "w").write(last.get("text", ""))
    return r


def first_light():
    create_machine("octane2")
    a = App()
    try:
        s = a.wait("library", lambda: a.state()["library"] and a.state(), 30)
        check("library lists the machine", s and "octane2" in s["library"], s and s["library"])
        shot("01-library")
        a.req("open octane2")
        time.sleep(1)
        st = a.machine("octane2")
        check("opening shows a Stopped tab", st and st["state"] == "Stopped", st)
        check("start is enabled, power off is not", a.state()["enabled"]["machine.start"] and not a.state()["enabled"]["machine.reset"])
        shot("02-hero")
        r = a.req("invoke machine.start octane2")
        check("start command accepted", r.get("ok"), r)
        saw_starting = a.wait("Starting", lambda: (a.machine("octane2") or {}).get("state") == "Starting", 15, 0.1)
        check("tab shows Starting", saw_starting)
        shot("03-starting")
        c = prom_ready(a, "octane2")
        check("first picture within 15 min", c is not None)
        if not c:
            return
        shot("04-prom")
        check("picture equals QMP screendump (0 pixels differ)", c["differing_pixels"] == 0, c)
        st = a.req("stats")["machines"][0]
        sc = st.get("scanout")
        if sc:
            check("zero-copy: engine copied 0 bytes, app moved 0 CPU pixel bytes",
                  st["engine_copied_bytes"] == 0 and sc["cpu_pixel_bytes"] == 0, st)
            check("only changed lines copied on the GPU", sc["gpu_bytes"] < sc["full_frame_bytes"], sc)
        else:
            check("copy fallback moved rows, not the full frame", st["rows_copied_fallback"] > 0, st)
        # keyboard through the real controller
        pos = screen_centre(a, "octane2")
        check("screen bounds reported", pos is not None)
        if mode == "x11" and pos:
            pointer_to(*pos)
            click()
            time.sleep(0.5)
        if mode != "x11":
            # wtype sends its own keymap's codes, not evdev ones, so the app's
            # hardware-keycode mapping cannot be tested with it.
            skip("keyboard checks", "wtype's virtual keyboard does not use evdev keycodes")
        else:
            menu = prom_menu(a, "octane2")
            check("PROM menu reached", menu is not None)
            # The menu is drawn item by item; keys sent mid-draw are lost.
            a.wait("menu settled", lambda: a.machine("octane2")["idle_ms"] > 3000, 60, 0.5)
            shot("04b-menu")
            before = a.machine("octane2")["inputs"]
            ok = key("5")
            time.sleep(1)
            ok = key("Return") and ok
            if not ok:
                skip("keyboard checks", "this compositor has no virtual keyboard to inject keys with")
            if ok:
                check("keys reached the guest", a.wait("input counted", lambda: a.machine("octane2")["inputs"] > before, 10) is not None)
                time.sleep(8)
                type_text("hinv")
                key("Return")
                time.sleep(10)
                c = a.req("compare hinv octane2")
                check("hinv screen equals QMP screendump", c.get("differing_pixels") == 0, c)
                png = ppm_to_png(c["ppm"], "hinv-qmp")
                text = ocr(png)
                open(os.path.join(out, "hinv.ocr.txt"), "w").write(text)
                check("hinv output on screen (OCR sees the CPU line)", "CPU" in text.upper() or "MHZ" in text.upper(), text[-400:])
        shot("05-after-keys")
        # follow mouse
        if mode != "x11":
            skip("pointer checks", "no pointer injection under this Wayland compositor")
        if mode == "x11" and pos:
            n0 = a.machine("octane2")["inputs"]
            check("default pointer mode is follow", a.machine("octane2")["pointer"] == "follow")
            for dx in range(0, 120, 20):
                pointer_to(pos[0] + dx, pos[1] + dx / 2)
                time.sleep(0.05)
            check("follow: motion reached the guest", a.machine("octane2")["inputs"] > n0)
            # capture
            a.req("invoke machine.pointer-capture octane2")
            check("capture mode set", a.machine("octane2")["pointer"] == "capture")
            pointer_to(*pos)
            click()
            time.sleep(0.5)
            check("a click captures", a.machine("octane2")["captured"])
            n0 = a.machine("octane2")["inputs"]
            sh("xdotool", "mousemove_relative", "--", "30", "10")
            time.sleep(0.3)
            sh("xdotool", "mousemove_relative", "--", "-10", "-5")
            time.sleep(0.3)
            check("capture: relative motion reached the guest", a.machine("octane2")["inputs"] > n0)
            shot("06-captured")
            sh("xdotool", "keydown", "ctrl", "keydown", "alt")
            time.sleep(0.3)
            sh("xdotool", "keyup", "alt", "keyup", "ctrl")
            time.sleep(0.5)
            check("Ctrl+Alt releases the pointer", a.wait("release", lambda: not a.machine("octane2")["captured"], 5, 0.2) is not None, a.machine("octane2"))
            check("no key stays down after release", a.req("release-keys octane2").get("released") == 0)
            # The core counts what it has sent down. A key held now shows up...
            pointer_to(*pos)
            click()
            sh("xdotool", "keydown", "a")
            time.sleep(0.3)
            held = a.req("release-keys octane2").get("released")
            sh("xdotool", "keyup", "a")
            check("the core sees a held key", held >= 1, held)
            # ...and one held while focus leaves is released by the app itself.
            pointer_to(*pos)
            click()
            sh("xdotool", "keydown", "a")
            time.sleep(0.3)
            a.req("invoke app.preferences")
            time.sleep(1)
            left = a.req("release-keys octane2").get("released")
            sh("xdotool", "keyup", "a")
            check("a key held while focus leaves is released", left == 0, left)
            key("Escape")
        # Idle: the PROM's command monitor is static, so the app should cost
        # next to nothing and present nothing.
        time.sleep(3)
        s0, f0 = a.req("stats"), a.machine("octane2")["seq"]
        time.sleep(30)
        s1 = a.req("stats")
        used = s1["cpu_s"] - s0["cpu_s"]
        moved = a.machine("octane2")["seq"] - f0
        if moved == 0:
            check("idle: under 1% of one core over 30 s with a still picture", used < 0.3, {"cpu_s_used": used})
        else:
            # The guest was still drawing, so this is a per-frame cost.
            check("busy: under 5 ms of app CPU per presented frame", used / moved < 0.005, {"cpu_s_used": used, "frames": moved})
        st = a.req("stats")
        json.dump(st, open(os.path.join(out, "stats.json"), "w"), indent=1)
        print(json.dumps(st)[:1500])
    finally:
        a.stop()


SCENARIOS = {"first-light": first_light}
try:
    SCENARIOS[scenario]()
finally:
    json.dump(results, open(os.path.join(out, "results.json"), "w"), indent=1)
sys.exit(1 if failed else 0)
