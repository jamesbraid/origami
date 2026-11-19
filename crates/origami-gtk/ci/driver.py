#!/usr/bin/env python3
"""Drives the real origami-gtk app through its automation socket and the
session's own input injection (XTest under Xvfb, wtype under cage), so the app's
key and pointer handling is what is tested. Runs inside the container.

usage: driver.py OUTDIR x11|wayland SCENARIO [app args]

Environment: APP, WORK, ORIGAMI_BIN, plus DISPLAY or WAYLAND_DISPLAY.
Writes OUTDIR/results.json and exits non-zero if any check failed.
"""
import difflib, json, os, re, socket, subprocess, sys, time

out, mode, scenario = sys.argv[1], sys.argv[2], sys.argv[3]
extra = sys.argv[4:]
app = os.environ["APP"]
work = os.environ["WORK"]
origami = os.environ["ORIGAMI_BIN"]
lib = os.environ.get("LIBRARY") or os.path.join(work, "library")
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


def click(button=1, times=1):
    return mode == "x11" and sh("xdotool", "click", "--repeat", str(times), "--delay", "80", str(button)).returncode == 0


def ppm_to_png(ppm, name):
    p = os.path.join(out, name + ".png")
    sh("convert", ppm, p)
    return p


def ocr(png):
    r = sh("tesseract", png, "-", "--psm", "6")
    return r.stdout


# What the PROM's hinv prints for the Octane2: the words that name the machine,
# and the words any hinv has.
HINV_IDENTITY = ("IP30", "R12000")
HINV_OTHER = ("MHZ", "PROCESSOR", "MEMORY", "CACHE", "ETHERNET")


def _like(word, want, ratio):
    return (abs(len(word) - len(want)) <= 1 and len(want) >= 3
            and difflib.SequenceMatcher(None, word, want).ratio() >= ratio)


def _identity(word, want):
    """The same length, each position equal but one, which may be an OCR mark: IP3@
    is IP30; IP32 and R10000 are not."""
    marks = sum(w != c for w, c in zip(word, want))
    return len(word) == len(want) and marks <= 1 and all(w == c or not w.isalnum() for w, c in zip(word, want))


def hinv_evidence(text):
    """Whether OCR text shows hinv: IP30 or R12000, read tolerantly (under CPU
    limits tesseract gives IP3@ and R1200@), and one other hinv word."""
    words = re.findall(r"[A-Za-z0-9@]+", text.upper())
    ids = {want for want in HINV_IDENTITY for w in words if len(w) >= 4 and _identity(w, want)}
    other = {want for want in HINV_OTHER for w in words if _like(w, want, 0.8)}
    return ids, other


def ocr_hinv(png):
    """OCR of the picture as it is and enlarged in grey, whose text is joined."""
    big = png[:-4] + "-big.png"
    sh("convert", png, "-colorspace", "Gray", "-resize", "200%", "-normalize", big)
    return ocr(png) + "\n" + ocr(big)


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
    if os.environ.get("COMPOSITOR") == "cage":
        # Cage has no pointer constraints: capture falls back to differencing.
        check("a click captures", a.wait("capture", lambda: a.machine("octane2")["captured"], 5, 0.2) is not None)
        skip("pointer lock", "cage offers no pointer constraints")
        a.req("invoke machine.capture octane2")
        return
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


def diagnostics_window(a, name):
    """The open Diagnostics window's report, its Copy Report button clicked
    with a real click, and what lands on the clipboard (X11 only)."""
    got = a.wait("diagnostics window", lambda: (a.machine(name) or {}).get("diagnostics"), 10, 0.2)
    if not check("the Diagnostics window opens", got):
        return None
    # The button has a size once the window has been laid out.
    got = a.wait("laid out", lambda: ((a.machine(name) or {}).get("diagnostics") or {}).get("copy_bounds", [0, 0, 0])[2] > 60 and a.machine(name)["diagnostics"], 5, 0.2) or got
    wins = sh("xdotool", "search", "--onlyvisible", "--name", "Diagnostics").stdout.split()
    if wins and got.get("copy_bounds"):
        b = got["copy_bounds"]
        sh("xdotool", "mousemove", "--window", wins[-1], str(int(b[0] + b[2] / 2)), str(int(b[1] + b[3] / 2)))
        click()
        time.sleep(0.5)
        text = a.req("clipboard").get("text")
        check("Copy Report puts the report on the clipboard", text and text.strip() == got["report"].strip(), text)
    else:
        skip("Copy Report", "no Diagnostics window or button to click")
    return got["report"]


def diagnostics():
    """The Diagnostics window from the library's row menu, on a stopped machine."""
    if mode != "x11":
        skip("diagnostics", "needs XTest")
        return
    create_machine("octane2")
    a = App()
    try:
        a.wait("library", lambda: a.state()["library"], 30)
        b = a.state()["chrome"]["rows"][0]["bounds"]
        pointer_to(b[0] + b[2] / 2, b[1] + b[3] / 2)
        click(3)
        a.wait("row menu", lambda: a.state()["chrome"]["row_menu"], 5, 0.2)
        check("the row menu has Diagnostics", "Diagnostics" in a.state()["chrome"]["row_menu_items"])
        key("Down")
        key("Down")
        key("Return")
        report = diagnostics_window(a, "octane2")
        if report:
            shot("diagnostics")
            for want in ("Machine:      octane2", "Engine:       ", "State:        Stopped (NotRunning)", "Last error:   none", "Heads:", "Console:      ", "Screen:       "):
                check("report has " + want.strip(), want in report, report)
        a.req("invoke window.diagnostics octane2")
        time.sleep(0.5)
        n = len(sh("xdotool", "search", "--onlyvisible", "--name", "Diagnostics").stdout.split())
        check("the command raises the same window, one per machine", n == 1, n)
        sh("wmctrl", "-c", "Diagnostics")
        check("closing it forgets it", a.wait("closed", lambda: not a.machine("octane2")["diagnostics"], 5, 0.2) is not None)
    finally:
        a.stop()


def console():
    """The console window under real input: mouse selection, copy, scrollback,
    and what a key or a paste sends, read back from the window. The PROM's
    power-on diagnostics on the serial line are the output."""
    if mode != "x11":
        skip("console", "needs XTest")
        return
    create_machine("octane2")
    a = App()
    try:
        a.wait("library", lambda: a.state()["library"], 30)
        a.req("open octane2")
        # The first boot sets up the PROM's NVRAM. Changing it then leaves a
        # checksum the PROM rejects, so the next boot prints its power-on
        # diagnostics over the serial line.
        a.req("invoke machine.start octane2")
        check("the first boot reaches the PROM", prom_ready(a, "octane2") is not None)
        a.req("invoke machine.force-quit octane2")
        a.wait("stopped", lambda: (a.machine("octane2") or {}).get("state") == "Stopped", 60, 0.5)
        r = sh(origami, "nvram", os.path.join(lib, "octane2"), "set", "console=d")
        check("the PROM's NVRAM is changed", r.returncode == 0, r.stderr)
        a.req("invoke window.console octane2")
        probe = lambda: (a.machine("octane2") or {}).get("console")
        win = lambda: (sh("xdotool", "search", "--onlyvisible", "--name", "Console").stdout.split() or [None])[-1]
        check("the console window opens", a.wait("console window", lambda: probe() and win(), 15, 0.3) is not None)
        a.req("invoke window.console octane2")
        time.sleep(0.5)
        n = len(sh("xdotool", "search", "--onlyvisible", "--name", "Console").stdout.split())
        check("the command raises the same window, one per machine", n == 1, n)
        # A few rows tall, so the diagnostics overflow into scrollback.
        sh("xdotool", "windowsize", "--sync", win(), "700", "90")
        a.req("invoke machine.start octane2")
        up = a.wait("the diagnostics", lambda: probe() and "Press any key" in probe()["screen"] and probe(), 600, 2)
        check("the guest's output shows in the console", up is not None, probe() and probe()["screen"])
        if not up:
            return

        def settled(secs=5):
            """The screen has stopped changing: output clears a selection."""
            last = [None, time.time()]

            def f():
                t = probe()["screen"]
                if t != last[0]:
                    last[0], last[1] = t, time.time()
                return probe() if time.time() - last[1] >= secs else None
            return a.wait("console to settle", f, 120, 1)

        def at(row, col):
            p = probe()
            b, (cw, ch) = p["bounds"], p["cell"]
            return win(), int(b[0] + (col + 0.5) * cw), int(b[1] + (row + 0.5) * ch)

        p = settled()
        shot("console")
        check("output past the screen builds scrollback", p["history"] > 0, [p["rows"], p["history"], p["bounds"]])
        sh("xdotool", "mousemove", "--window", at(1, 5)[0], *map(str, at(1, 5)[1:]))
        sh("xdotool", "click", "--repeat", "3", "--delay", "100", "4")
        time.sleep(0.3)
        check("the mouse wheel scrolls back", probe()["scrolled"] > 0, probe()["scrolled"])
        sh("xdotool", "click", "--repeat", "20", "--delay", "50", "5")
        time.sleep(0.3)
        check("and forward again", probe()["scrolled"] == 0, probe()["scrolled"])
        # A click gives the window the keyboard (the wheel does not).
        w, x, y = at(1, 5)
        sh("xdotool", "mousemove", "--window", w, str(x), str(y))
        click()
        key("shift+Prior")
        time.sleep(0.3)
        check("Shift+Page Up scrolls back", probe()["scrolled"] > 0, probe()["scrolled"])
        key("shift+Next")
        time.sleep(0.3)
        check("Shift+Page Down returns", probe()["scrolled"] == 0, probe()["scrolled"])

        # Select most of the last line of text with the mouse and copy it. The
        # drag starts inside the line: a press at the window's edge resizes it,
        # and the window's border shifts the pointer by a few pixels.
        lines = probe()["screen"].split("\n")
        row = max(i for i, l in enumerate(lines) if l.strip())
        text = lines[row].rstrip()
        w, x0, y0 = at(row, 2)
        _, x1, y1 = at(row, len(text) - 1)
        sh("xdotool", "mousemove", "--window", w, str(x0), str(y0))
        sh("xdotool", "mousedown", "1")
        time.sleep(0.2)
        for i in range(1, 6):
            sh("xdotool", "mousemove", "--window", w, str(x0 + (x1 - x0) * i // 5), str(y1))
            time.sleep(0.1)
        sh("xdotool", "mouseup", "1")
        time.sleep(0.3)
        shot("console-selected")
        sel = probe()["selected"]
        check("a mouse drag selects the line", bool(sel) and sel in text and len(sel) >= len(text) - 4, [sel, text])
        key("ctrl+shift+c")
        time.sleep(0.3)
        check("Ctrl+Shift+C copies the selection", a.req("clipboard").get("text") == sel, a.req("clipboard"))

        # Keys and paste reach the guest; a pasted line end is one carriage return.
        key("a")
        time.sleep(0.3)
        check("a typed key is sent", probe()["sent"] == "a", probe()["sent"])
        a.req("clipboard-set " + json.dumps("ls\r\nls\nls\r"))
        key("ctrl+shift+v")
        time.sleep(0.5)
        check("Ctrl+Shift+V pastes with each line end as one carriage return", probe()["sent"] == "als\rls\rls\r", repr(probe()["sent"]))
        key("shift+Insert")
        time.sleep(0.5)
        check("Shift+Insert pastes too", probe()["sent"] == "a" + "ls\rls\rls\r" * 2, repr(probe()["sent"]))
    finally:
        a.stop()


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
        check("the toolbar offers Pause", a.state()["pause_tip"] == "Pause", a.state()["pause_tip"])
        a.req("invoke machine.pause octane2")
        check("Pause pauses the machine", a.wait("Paused", lambda: a.machine("octane2")["state"] == "Paused", 15, 0.2) is not None, a.machine("octane2")["state"])
        check("and the button becomes Resume", a.state()["pause_tip"] == "Resume", a.state()["pause_tip"])
        a.req("invoke machine.resume octane2")
        check("Resume runs it again", a.wait("Running", lambda: a.machine("octane2")["state"] == "Running", 15, 0.2) is not None, a.machine("octane2")["state"])
        if mode == "x11":
            # Closing the window needs wmctrl; under a compositor it would
            # stay open over the app and take the pointer checks' clicks.
            a.req("invoke window.diagnostics octane2")
            w, h = c["width"], c["height"]
            report = diagnostics_window(a, "octane2")
            check("diagnostics: a running machine shows its head and state", report and "State:        Running\n" in report and "%dx%d  picture yes" % (w, h) in report, report)
            sh("wmctrl", "-c", "Diagnostics")
        st = a.req("stats")["machines"][0]
        sc = st.get("scanout")
        if sc:
            check("zero-copy: engine copied 0 bytes, app moved 0 CPU pixel bytes",
                  st["engine_copied_bytes"] == 0 and sc["cpu_pixel_bytes"] == 0, st)
            check("zero-copy: the display backend imports",
                  st["backend"] in ("vulkan import", "imported, zero-copy (vulkan import)"), st["backend"])
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
                text = ocr_hinv(png)
                open(os.path.join(out, "hinv.ocr.txt"), "w").write(text)
                ids, other = hinv_evidence(text)
                check("hinv output on screen (OCR reads IP30 or R12000 and another hinv word)", ids and other, {"ids": sorted(ids), "other": sorted(other), "text": text[-400:]})
        shot("05-after-keys")
        # follow mouse
        if mode != "x11":
            wayland_pointer(a)
        if mode == "x11" and pos:
            n0 = a.machine("octane2")["inputs"]
            check("default pointer mode is follow", a.machine("octane2")["pointer"] == "follow", a.machine("octane2"))
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
            # Fast motion: forty moves in one xdotool call arrive back to back,
            # before each warp lands. Each must be counted once.
            t0 = a.machine("octane2")["motion_total"]
            burst = ["mousemove_relative", "--", "25", "0"] * 40
            sh("xdotool", *burst)
            time.sleep(0.5)
            t1 = a.machine("octane2")["motion_total"]
            got = (t1[0] - t0[0], t1[1] - t0[1])
            check("capture: a burst of fast motion is counted once (1000 across)", abs(got[0] - 1000) <= 5 and abs(got[1]) <= 3, got)
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
        if mode == "x11":
            # The CPU panel: a real click on the toolbar button shows it, and the
            # numbers arrive twice a second once two readings exist.
            b = a.state()["cpu_btn"]
            pointer_to(b[0] + b[2] / 2, b[1] + b[3] / 2)
            click()
            got = a.wait("CPU rows", lambda: (a.machine("octane2")["cpu_panel"]["rows"] or None), 20, 0.5)
            panel = a.machine("octane2")["cpu_panel"]
            check("the CPU button shows one bar per CPU", panel["visible"] and got and [r[0] for r in got] == [0], panel)
            check("each bar's user and system time are percentages", all(0 <= r[1] <= 100 and 0 <= r[2] <= 100 and r[1] + r[2] <= 100.01 for r in got or []), panel)
            check("the panel is titled as measured on the host", panel["title"] == "Emulated CPU load (measured on the host)")
            shot("cpu-panel")
            click()
            time.sleep(0.5)
            check("the button hides it again", not a.machine("octane2")["cpu_panel"]["visible"])
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
            # The guest was still drawing, so judge the cost of a frame: the
            # plan's floor is 0.5 ms of app CPU per presented frame at p95.
            p95 = s1["machines"][0]["pump_us"]["p95"]
            check("busy: app CPU per presented frame p95 within 0.5 ms", p95 <= 500, {"p95_us": p95, "frames": moved})
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
        a.req("invoke machine.new")
        time.sleep(2)
        png = shot("new-machine")
        # A single engine's row is greyed out: darken light text for the OCR.
        sh("convert", png, "-colorspace", "Gray", "-threshold", "75%", os.path.join(out, "new-machine-bw.png"))
        text = ocr(os.path.join(out, "new-machine-bw.png"))
        check("New Machine asks for an engine", "Engine" in text, text[-300:])
        # Pick iris from the Engine list and create a machine on it.
        key("Return")
        time.sleep(0.5)
        key("Down")
        key("Return")
        time.sleep(1)
        key("Tab")
        key("Tab")
        type_text("irisbox")
        time.sleep(0.5)
        shot("new-machine-iris")
        key("Return")
        listed = a.wait("created", lambda: "irisbox" in a.state()["library"], 120, 1)
        check("a machine is created on the iris engine", listed is not None, a.state()["library"])
        text = open(os.path.join(lib, "irisbox", "machine.toml")).read() if listed else ""
        check("its machine file names the iris engine", "iris" in text, text[:300])
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


def popout():
    """A tab moved to its own window, and that window closed: the machine
    goes back to the library with nothing orphaned, and opens again."""
    if mode != "x11":
        skip("pop-out", "needs a window manager to close a window")
        return
    create_machine("octane2")
    a = App()
    try:
        a.wait("library", lambda: a.state()["library"], 30)
        a.req("open octane2")
        check("one window to start with", a.state()["windows"] == 1)
        a.req("invoke window.move-to-new-window octane2")
        time.sleep(1.5)
        check("the tab moved to a window of its own", a.state()["windows"] == 2 and a.machine("octane2")["visible"], a.state()["windows"])
        shot("popped-out")
        sh("wmctrl", "-c", ":ACTIVE:")
        gone = a.wait("pop-out closed", lambda: a.state()["windows"] == 1, 10, 0.3)
        check("closing the pop-out closes the window", gone is not None, [a.state()["windows"], sh("wmctrl", "-l").stdout])
        check("the machine is forgotten, not orphaned", a.machine("octane2") is None, a.state()["machines"])
        a.req("open octane2")
        time.sleep(1)
        m = a.machine("octane2")
        check("the library opens it again in the main window", m is not None and m["visible"], m)
        # Cycle it out and in: no error, nothing left behind.
        for _ in range(5):
            a.req("invoke window.move-to-new-window octane2")
            time.sleep(0.6)
            sh("wmctrl", "-c", ":ACTIVE:")
            time.sleep(0.6)
            a.req("open octane2")
            time.sleep(0.4)
        check("five more pop-out and close cycles leave one window and one tab", a.state()["windows"] == 1 and len(a.state()["machines"]) == 1, a.state()["windows"])
    finally:
        a.stop()


def hidpi():
    """Two machines of one product are told apart by real clicks on the
    library, and the guest picture sits at the right scale for the display
    (GDK_SCALE, or the compositor's scale), with the pointer mapped through it."""
    factor = float(os.environ.get("GDK_SCALE") or 1)
    create_machine("octane2")
    create_machine("octane2b")
    start_vptr()
    a = App()
    try:
        s = a.wait("library", lambda: len(a.state()["library_rows"]) == 2 and a.state(), 30)
        check("both machines are listed", bool(s), s and s["library"])
        if s and mode == "x11":
            # The rows are listed before the sidebar is laid out; their bounds read
            # as a 16x0 box at x=-8 until then, and a click there misses.
            a.wait("laid out", lambda: all(r["bounds"][2] > 50 and r["bounds"][3] > 10 for r in a.state()["library_rows"]), 10, 0.1)
            rows = {os.path.basename(r["dir"]): r["bounds"] for r in a.state()["library_rows"]}
            for name, tabs in [("octane2b", ["octane2b"]), ("octane2", ["octane2", "octane2b"]), ("octane2b", ["octane2", "octane2b"])]:
                b = rows[name]
                pointer_to((b[0] + b[2] / 2) * factor, (b[1] + b[3] / 2) * factor)
                click(1, 2)
                time.sleep(0.6)
                got = sorted(m["name"] for m in a.state()["machines"])
                check("double-clicking the %s row opens %s, once" % (name, name), got == tabs, [got, b])
        a.req("open octane2")
        r = a.req("invoke window.console octane2")
        check("the Console button's command opens the console", r.get("ok"), r)
        time.sleep(1)
        shot("console")
        sh("wmctrl", "-c", ":ACTIVE:")
        time.sleep(0.5)
        a.req("invoke machine.start octane2")
        c = prom_ready(a, "octane2")
        check("first picture within 15 min", c is not None)
        if not c:
            return
        win = app_window()
        if win:
            # run.sh passes an unset HIDPI_WINDOW through as empty.
            size = (os.environ.get("HIDPI_WINDOW") or "1500 1200").split()
            sh("xdotool", "windowsize", win, size[0], size[1])
            time.sleep(1.5)
        a.req("invoke view.fit octane2")
        time.sleep(1)
        m = a.machine("octane2")
        pic = m["picture"]
        env_factor = os.environ.get("GDK_SCALE")
        if env_factor:
            check("scale factor is seen", abs(m["scale_factor"] - float(env_factor)) < 0.01, m["scale_factor"])
        factor = m["scale_factor"]
        view_w, view_h = m["screen_bounds"][2], m["screen_bounds"][3]
        guest = m["size"]
        fit = min(view_w * factor / guest[0], view_h * factor / guest[1])
        want = int(fit) if int(fit) >= factor else fit
        check("fit takes the largest whole step (%s)" % want, abs(pic["step"] - want) < 1e-6 and pic["nearest"] == (want == int(want)), [pic, fit])
        check("the picture is guest size x step / factor logical px", pic["rect"][2] == round(guest[0] * want / factor), pic)
        shot("hidpi-fit")
        json.dump(a.machine("octane2"), open(os.path.join(out, "geometry.json"), "w"))
        for cmd, step in [("view.scale-1x", factor), ("view.actual-size", 1.0)]:
            a.req("invoke %s octane2" % cmd)
            time.sleep(0.8)
            pic = a.machine("octane2")["picture"]
            fits = guest[0] * step <= view_w * factor and guest[1] * step <= view_h * factor
            check("%s: %s device px per guest px%s" % (cmd, step, "" if fits else " (does not fit: falls back to fit)"),
                  abs(pic["step"] - (step if fits else want)) < 1e-6, pic)
            shot("hidpi-" + cmd.split(".")[1])
            if mode == "x11":
                # Follow: the host pointer a quarter and three quarters across the
                # picture lands on the same fraction of the guest's screen.
                m = a.machine("octane2")
                bx, by = m["screen_bounds"][0], m["screen_bounds"][1]
                r = pic["rect"]
                seen = []
                for fx, fy in [(0.25, 0.25), (0.75, 0.6)]:
                    pointer_to((bx + r[0] + fx * r[2]) * factor, (by + r[1] + fy * r[3]) * factor)
                    time.sleep(0.4)
                    pointer_to((bx + r[0] + fx * r[2]) * factor + 1, (by + r[1] + fy * r[3]) * factor)
                    time.sleep(0.6)
                    seen.append(a.machine("octane2")["pointer_at"])
                # The window frame offsets both points alike, so compare the distance
                # between them: half the guest's width and 35% of its height.
                d = (seen[1][0] - seen[0][0], seen[1][1] - seen[0][1])
                check("%s: moving across half the picture moves the guest pointer half its screen" % cmd,
                      abs(d[0] - 0.5 * guest[0]) <= 3 and abs(d[1] - 0.35 * guest[1]) <= 3, [d, seen])
        a.req("invoke view.fit octane2")
        time.sleep(1)
        st = a.req("stats")
        json.dump(st, open(os.path.join(out, "stats.json"), "w"), indent=1)
        print(json.dumps(st["machines"][0].get("pump_us")), json.dumps(st["machines"][0].get("gpu_copy_us")))
    finally:
        a.stop()


def frames():
    """Boot to the PROM and record what presenting cost, for comparing builds."""
    create_machine("octane2")
    a = App()
    try:
        a.wait("library", lambda: a.state()["library"], 30)
        a.req("open octane2")
        a.req("invoke machine.start octane2")
        c = prom_ready(a, "octane2")
        check("first picture within 15 min", c is not None)
        time.sleep(5)
        st = a.req("stats")
        json.dump(st, open(os.path.join(out, "stats.json"), "w"), indent=1)
        m = st["machines"][0]
        print("frames", m["frames_imported"], "rows_copied", m["rows_copied_fallback"], "pump_us", m["pump_us"], "gpu_copy_us", m["gpu_copy_us"], "cpu_s", st["cpu_s"])
        shot("frames")
    finally:
        a.stop()


def sidebar():
    """The library's rows and the tab bar under real clicks: one click selects,
    two open a tab and keep the others, the row menu offers its items, and the
    tabs and summary cards have their sizes."""
    if mode != "x11":
        skip("sidebar", "needs XTest")
        return
    for n in ("octane2", "octane2-b", "octane2-c"):
        create_machine(n)
    a = App()
    try:
        a.wait("library", lambda: len(a.state()["library"]) == 3, 30)
        chrome = lambda: a.state()["chrome"]
        opened = lambda: sorted(m["name"] for m in a.state()["machines"])

        def row(name):
            # Past the double-click time, so one sequence's clicks never join
            # the last one's, and after the pointer has settled on the row.
            time.sleep(0.7)
            b = {r["name"]: r["bounds"] for r in chrome()["rows"]}[name]
            pointer_to(b[0] + b[2] / 2, b[1] + b[3] / 2)
            time.sleep(0.3)

        time.sleep(1)
        shot("sidebar-empty")
        row("octane2")
        click()
        time.sleep(0.5)
        check("a single click selects the row and opens nothing", chrome()["selected"] == "octane2" and opened() == [], [chrome()["selected"], opened()])
        row("octane2-b")
        click(1, 2)
        a.wait("tab", lambda: opened() == ["octane2-b"], 20, 0.2)
        check("a double click opens the machine in a tab", opened() == ["octane2-b"], opened())
        row("octane2")
        click(1, 2)
        a.wait("tab", lambda: len(opened()) == 2, 20, 0.2)
        check("a second double click keeps the first tab", opened() == ["octane2", "octane2-b"], opened())
        time.sleep(0.5)
        shot("sidebar-tabs")
        c = chrome()
        # The tab widget is the tab's outer frame.
        check("the tab bar is 34 px and its tabs 26", c["tab_bar"][1] == 34 and [t[1] for t in c["tabs"]] == [26, 26], [c["tab_bar"], c["tabs"]])
        check("the summary cards are 140 by 104", a.machine("octane2")["cards"] and all(s == [140, 104] for s in a.machine("octane2")["cards"]), a.machine("octane2")["cards"])
        row("octane2-c")
        click(3)
        shown = a.wait("row menu", lambda: chrome()["row_menu"], 20, 0.2)
        check("a right click shows the row menu with its items", shown and chrome()["row_menu_items"] == ["Open in New Tab", "Show in Files", "Diagnostics"], chrome())
        shot("sidebar-menu")
        key("Return")
        a.wait("tab", lambda: len(opened()) == 3, 20, 0.2)
        check("Open in New Tab opens that machine", "octane2-c" in opened() and chrome()["selected"] == "octane2-c", opened())
        # The keyboard route: focus the selected row, then the Menu key and Shift+F10.
        row("octane2-b")
        click()
        for k in ("Menu", "shift+F10"):
            key(k)
            check(k + " opens the row menu", a.wait("row menu", lambda: chrome()["row_menu"], 20, 0.2) is not None, chrome())
            key("Escape")
            a.wait("menu closed", lambda: not chrome()["row_menu"], 20, 0.2)
    finally:
        a.stop()


def about():
    """About's cube steps to another photograph on the right arrow and on a
    Shift-click. The cube needs a hardware GPU; without one About is the stock
    dialog and there is nothing to step."""
    if mode != "x11":
        skip("about", "needs XTest")
        return
    create_machine("octane2")
    a = App()
    try:
        a.wait("library", lambda: a.state()["library"], 30)
        a.req("invoke app.about")
        time.sleep(3)
        if "no cube" in open(os.path.join(out, "app.log")).read():
            skip("About's cube steps through the scenes", "no hardware GPU for the cube")
            return
        before = shot("about-0")
        key("Right")
        time.sleep(3)
        after = shot("about-1")
        r = sh("compare", "-metric", "AE", before, after, os.path.join(out, "about-diff.png"))
        check("the right arrow changes the cube's scene", int(float((r.stderr or "0").split()[0])) > 1000, r.stderr)
    finally:
        a.stop()



def hinv():
    """The About egg: typing hinv slides the host inventory over the credits,
    Escape puts the credits back and leaves About open. The custom About needs
    a hardware GPU for its cube."""
    if mode != "x11":
        skip("hinv", "needs XTest")
        return
    create_machine("octane2")
    a = App()
    try:
        a.wait("library", lambda: a.state()["library"], 30)
        a.req("invoke app.about")
        time.sleep(3)
        if "no cube" in open(os.path.join(out, "app.log")).read():
            skip("the hinv egg", "no hardware GPU, so About is the stock dialog")
            return
        shot("about-credits")
        type_text("hinv")
        time.sleep(3)
        text = ocr(shot("about-hinv"))
        check("typing hinv shows the host inventory", "Main memory" in text or "Processor" in text, text[-300:])
        key("Escape")
        time.sleep(2)
        text = ocr(shot("about-back"))
        check("Escape hides the inventory and leaves About open", "About Origami" in text and "Main memory" not in text, text[-300:])
    finally:
        a.stop()


def cube():
    """The empty-window cube and About's cube draw, and draw again after the
    renderer's device is dropped (when the tab hides the empty window's cube)
    and made anew. Needs a hardware GPU for the cube."""
    if mode != "x11":
        skip("cube", "needs XTest")
        return
    create_machine("octane2")
    a = App()

    def spread(png, box):
        """The standard deviation of a crop: near 0 for a flat fill."""
        r = sh("convert", png, "-crop", box, "+repage", "-colorspace", "Gray", "-format", "%[fx:standard_deviation]", "info:")
        return float(r.stdout or 0)

    try:
        a.wait("library", lambda: a.state()["library"], 30)
        time.sleep(3)
        if "no cube" in open(os.path.join(out, "app.log")).read():
            skip("the cubes draw", "no hardware GPU for the cube")
            return
        empty = "200x200+840+553"
        check("the empty window's cube draws", spread(shot("cube-empty-1"), empty) > 0.03)
        a.req("open octane2")
        time.sleep(2)
        a.req("invoke window.close-tab octane2")
        time.sleep(3)
        check("and again after its device was dropped and made anew", spread(shot("cube-empty-2"), empty) > 0.03)
        a.req("invoke app.about")
        time.sleep(3)
        check("About's cube draws on the recreated device", spread(shot("cube-about"), "160x160+720+324") > 0.03)
    finally:
        a.stop()


def headless():
    """A machine with no display opens to its console: the running tab shows
    the console page, with the guest's serial output on it."""
    create_machine("origin200", "origin200-1")
    a = App()
    try:
        a.wait("library", lambda: a.state()["library"], 30)
        a.req("open origin200")
        r = a.req("invoke machine.start origin200")
        check("start accepted", r.get("ok"), r)
        up = a.wait("Running", lambda: (a.machine("origin200") or {}).get("state") == "Running", 120, 0.5)
        check("a machine with no display runs without waiting for a picture", up is not None, a.machine("origin200"))
        m = a.machine("origin200")
        check("its tab shows the console page", m and m["page"] == "console", m and m["page"])
        got = a.wait("serial output", lambda: (a.machine("origin200")["console_tab"] or {}).get("received", 0) > 0, 600, 3)
        check("the guest's serial output is on it", got is not None, (a.machine("origin200")["console_tab"] or {}).get("screen"))
        shot("headless")
    finally:
        a.stop()


# The named set `tools/screenshots linux` collects into docs/images/app/.
DOCS_SHOTS = ["linux-library", "linux-engine", "linux-new-machine", "linux-starting", "linux-running", "linux-paused", "linux-cpu-panel",
              "linux-diagnostics", "linux-console", "linux-settings", "linux-about"]


def window_ids(title):
    return sh("xdotool", "search", "--onlyvisible", "--name", title).stdout.split()


def doc_shot(name, title="Origami"):
    """One window, cropped out of the screen, as the documentation's picture."""
    ids = window_ids(title)
    if not ids:
        check("a window titled %s to photograph for %s" % (title, name), False, sh("wmctrl", "-l").stdout)
        return
    # A window without focus draws in its backdrop state (dimmed header): raise it and check it has focus.
    sh("xdotool", "windowactivate", "--sync", ids[-1])
    time.sleep(0.5)
    focused = sh("xdotool", "getwindowfocus").stdout.strip()
    check("%s has focus when photographed" % name, focused == ids[-1], [focused, ids[-1]])
    g = dict(l.split("=") for l in sh("xdotool", "getwindowgeometry", "--shell", ids[-1]).stdout.split())
    root = shot("_root")
    p = os.path.join(out, name + ".png")
    sh("convert", root, "-crop", "%sx%s+%s+%s" % (g["WIDTH"], g["HEIGHT"], g["X"], g["Y"]), "+repage", "-strip", p)
    os.remove(root)
    check("saved " + name, os.path.exists(p), g)


def docs():
    """The walkthrough in docs/app.md: one real session, each step photographed under its name.
    The console shot needs a second boot, after the first has set up NVRAM."""
    if mode != "x11":
        skip("docs", "needs XTest")
        return
    create_machine("octane2")
    a = App()
    try:
        a.wait("library", lambda: a.state()["library"], 30)
        a.req("open octane2")
        time.sleep(2)
        doc_shot("linux-library")
        # New Machine: the Engine list open, then iris chosen and a name typed.
        a.req("invoke machine.new")
        time.sleep(2)
        key("Return")
        time.sleep(1)
        doc_shot("linux-engine")
        key("Down")
        key("Return")
        time.sleep(1)
        key("Tab")
        key("Tab")
        type_text("indy")
        time.sleep(1)
        doc_shot("linux-new-machine")
        key("Escape")
        time.sleep(1)
        # The dialog's close leaves a focus ring on the + button; a click on the empty page takes it away.
        pointer_to(900, 700)
        click()
        a.req("invoke machine.start octane2")
        a.wait("Starting", lambda: (a.machine("octane2") or {}).get("state") == "Starting", 15, 0.1)
        time.sleep(0.5)
        doc_shot("linux-starting")
        check("the PROM draws its menu", prom_menu(a, "octane2") is not None)
        a.wait("menu settled", lambda: a.machine("octane2")["idle_ms"] > 3000, 60, 0.5)
        doc_shot("linux-running")
        a.req("invoke machine.pause octane2")
        a.wait("Paused", lambda: a.machine("octane2")["state"] == "Paused", 15, 0.2)
        time.sleep(1)
        doc_shot("linux-paused")
        a.req("invoke machine.resume octane2")
        a.wait("Running", lambda: a.machine("octane2")["state"] == "Running", 15, 0.2)
        b = a.state()["cpu_btn"]
        pointer_to(b[0] + b[2] / 2, b[1] + b[3] / 2)
        click()
        a.wait("CPU rows", lambda: (a.machine("octane2")["cpu_panel"]["rows"] or None), 20, 0.5)
        time.sleep(2)
        doc_shot("linux-cpu-panel")
        click()
        a.req("invoke window.diagnostics octane2")
        diagnostics_window(a, "octane2")
        doc_shot("linux-diagnostics", "Diagnostics")
        sh("wmctrl", "-c", "Diagnostics")
        a.req("invoke machine.force-quit octane2")
        a.wait("stopped", lambda: (a.machine("octane2") or {}).get("state") == "Stopped", 60, 0.5)
        sh(origami, "nvram", os.path.join(lib, "octane2"), "set", "console=d")
        a.req("invoke window.console octane2")
        a.wait("console window", lambda: window_ids("Console"), 15, 0.3)
        a.req("invoke machine.start octane2")
        probe = lambda: (a.machine("octane2") or {}).get("console")
        check("the console shows the PROM's output", a.wait("diagnostics", lambda: probe() and "Press any key" in probe()["screen"], 600, 2) is not None)
        time.sleep(3)
        doc_shot("linux-console", "Console")
        sh("wmctrl", "-c", "Console")
        a.req("invoke machine.force-quit octane2")
        a.wait("stopped", lambda: (a.machine("octane2") or {}).get("state") == "Stopped", 60, 0.5)
        a.req("invoke machine.settings octane2")
        time.sleep(3)
        doc_shot("linux-settings")
        key("Escape")
        time.sleep(1)
        a.req("invoke app.about")
        time.sleep(3)
        doc_shot("linux-about")
        for n in DOCS_SHOTS:
            check("%s.png exists" % n, os.path.exists(os.path.join(out, n + ".png")))
    finally:
        a.stop()


SCENARIOS = {"docs": docs, "headless": headless, "cube": cube, "hinv": hinv, "about": about, "console": console, "diagnostics": diagnostics, "sidebar": sidebar, "frames": frames, "popout": popout, "settings": settings, "hidpi": hidpi, "first-light": first_light, "pointer": lambda: first_light(True)}
try:
    SCENARIOS[scenario]()
finally:
    json.dump(results, open(os.path.join(out, "results.json"), "w"), indent=1)
sys.exit(1 if failed else 0)
