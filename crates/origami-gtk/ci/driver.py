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
        try:
            self.f.write(line + "\n")
            self.f.flush()
            answer = self.f.readline()
        except (BrokenPipeError, ConnectionError):
            answer = ""
        if not answer:
            # The app is gone: say how, so a crash is not a mystery.
            try:
                rc = self.proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                rc = "still running"
            raise SystemExit("the app stopped answering %r (exit status %s; a negative one is a signal)" % (line, rc))
        return json.loads(answer)

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


VPTR = None


def start_vptr():
    """A virtual pointer that outlives single commands (see vptr.rs), so the
    seat has a pointer when the app starts."""
    global VPTR
    path = os.environ.get("VPTR")
    if mode == "x11" or not path or not os.path.exists(path):
        return
    VPTR = subprocess.Popen([path], stdin=subprocess.PIPE, text=True, stdout=open(os.path.join(out, "vptr.log"), "w"), stderr=subprocess.STDOUT)
    time.sleep(1)


def vp(line):
    VPTR.stdin.write(line + "\n")
    VPTR.stdin.flush()
    time.sleep(0.05)


def wayland_pointer(a):
    """Follow and capture under a compositor with pointer constraints, the
    pointer injected through vptr. The output is 1280x720 and the window fills
    it."""
    if VPTR is None:
        skip("pointer checks", "no virtual pointer helper")
        return
    vp("abs 700 350 1280 720")
    time.sleep(0.3)
    n0 = a.machine("octane2")["inputs"]
    for i in range(6):
        vp("abs %d %d 1280 720" % (700 + 20 * i, 350 + 10 * i))
    check("follow: motion reached the guest", a.machine("octane2")["inputs"] > n0)
    a.req("invoke machine.pointer-capture octane2")
    vp("abs 700 350 1280 720")
    vp("click")
    check("a click captures", a.wait("capture", lambda: a.machine("octane2")["captured"], 5, 0.2) is not None, a.machine("octane2"))
    time.sleep(0.5)
    check("the window system's pointer lock is held", a.machine("octane2")["pointer_locked"], a.machine("octane2"))
    t0 = a.machine("octane2")["motion_total"]
    vp("move 30 10")
    vp("move -10 -5")
    time.sleep(0.3)
    t1 = a.machine("octane2")["motion_total"]
    got = (t1[0] - t0[0], t1[1] - t0[1])
    check("capture: the guest got the distance the pointer moved", abs(got[0] - 20) <= 2 and abs(got[1] - 5) <= 2, got)
    # A locked pointer has no edge: push far past the output's.
    for _ in range(8):
        vp("move 400 300")
    time.sleep(0.3)
    t2 = a.machine("octane2")["motion_total"]
    far = (t2[0] - t1[0], t2[1] - t1[1])
    check("capture: motion continues past the window edge", far[0] >= 3000 and far[1] >= 2200, far)
    a.req("invoke machine.capture octane2")
    check("the capture command releases the pointer", a.wait("release", lambda: not a.machine("octane2")["captured"], 5, 0.2) is not None)


def first_light(quick=False):
    create_machine("octane2")
    start_vptr()
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
        if quick:
            pass
        elif mode != "x11":
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
            wayland_pointer(a)
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
            check("the window system's pointer lock is held", a.machine("octane2")["pointer_locked"])
            t0 = a.machine("octane2")["motion_total"]
            sh("xdotool", "mousemove_relative", "--", "30", "10")
            time.sleep(0.3)
            sh("xdotool", "mousemove_relative", "--", "-10", "-5")
            time.sleep(0.3)
            check("capture: relative motion reached the guest", a.machine("octane2")["inputs"] > n0)
            t1 = a.machine("octane2")["motion_total"]
            got = (t1[0] - t0[0], t1[1] - t0[1])
            check("capture: the guest got the distance the pointer moved", abs(got[0] - 20) <= 2 and abs(got[1] - 5) <= 2, got)
            # The pointer may not reach a window edge: push far past it.
            for _ in range(8):
                sh("xdotool", "mousemove_relative", "--", "400", "300")
                time.sleep(0.1)
            mx, my = [int(v.split("=")[1]) for v in sh("xdotool", "getmouselocation", "--shell").stdout.split()[:2]]
            g = dict(l.split("=") for l in sh("xdotool", "getwindowgeometry", "--shell", app_window()).stdout.split() if "=" in l)
            wx, wy, ww, wh = int(g["X"]), int(g["Y"]), int(g["WIDTH"]), int(g["HEIGHT"])
            check("capture: the pointer stays inside the window", wx <= mx < wx + ww and wy <= my < wy + wh, [mx, my, wx, wy, ww, wh])
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
        if quick:
            return
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


def settings():
    """Edits go through the core: a stopped machine takes them and its file
    changes; a running one refuses."""
    create_machine("octane2")
    a = App()
    try:
        a.wait("library", lambda: a.state()["library"], 30)
        a.req("open octane2")
        toml_path = os.path.join(lib, "octane2", "machine.toml")
        read = lambda: open(toml_path).read()
        r = a.req("change memory 512")
        check("memory changes on a stopped machine", r.get("ok") and "512" in read(), r)
        r = a.req("change network private")
        check("network mode changes", r.get("ok") and 'mode = "private"' in read(), r)
        r = a.req("change forward ssh 2222 22")
        check("a port forward is added", r.get("ok") and "2222" in read(), r)
        r = a.req("change memory 99999")
        check("a memory size the machine does not take is refused", "error" in r, r)
        a.req("invoke machine.settings octane2")
        time.sleep(3)
        shot("settings")
        key("Escape")
        a.req("invoke machine.start octane2")
        a.wait("up", lambda: (a.machine("octane2") or {}).get("state") in ("Starting", "Running"), 30, 0.2)
        time.sleep(3)
        r = a.req("change memory 256")
        check("a running machine refuses an edit", "error" in r and "512" in read(), r)
        a.req("invoke machine.force-quit octane2")
        a.wait("down", lambda: (a.machine("octane2") or {}).get("state") == "Stopped", 60, 0.5)
        r = a.req("change memory 256")
        check("it takes the edit again once stopped", r.get("ok") and "256" in read(), r)
    finally:
        a.stop()


SCENARIOS = {"settings": settings, "first-light": first_light, "pointer": lambda: first_light(True)}
try:
    SCENARIOS[scenario]()
finally:
    json.dump(results, open(os.path.join(out, "results.json"), "w"), indent=1)
sys.exit(1 if failed else 0)
