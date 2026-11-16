// The headless self-test: no window is ever ordered onto screen. The display
// view sits in a window that is never shown and the SwiftUI views render to
// bitmaps.
//
//   Origami --selftest LIBRARY_DIR OUTDIR
//
// Built with the fake engine (ORIGAMI_FFI_FEATURES=fake, the nightly job) it
// makes its own machine. Built for QEMU it starts the first machine in
// LIBRARY_DIR. It prints one JSON line per step and exits non-zero on any
// failure. Timing budgets only fail the run with ORIGAMI_SELFTEST_TIMING=1,
// since CI runners are noisy.

import AppKit
import Foundation
import OrigamiFrame
import SwiftUI

@MainActor
func snapshot<V: View>(_ view: V, size: CGSize, to path: String) -> (ok: Bool, colours: Int) {
    let host = NSHostingView(rootView: view.frame(width: size.width, height: size.height))
    host.frame = CGRect(origin: .zero, size: size)
    host.layoutSubtreeIfNeeded()
    guard let rep = host.bitmapImageRepForCachingDisplay(in: host.bounds) else { return (false, 0) }
    host.cacheDisplay(in: host.bounds, to: rep)
    guard let png = rep.representation(using: .png, properties: [:]),
          (try? png.write(to: URL(fileURLWithPath: path))) != nil else { return (false, 0) }
    var seen = Set<UInt32>()
    if let data = rep.bitmapData {
        let n = rep.pixelsWide * rep.pixelsHigh, bpp = rep.bitsPerPixel / 8
        for i in stride(from: 0, to: n, by: 7) {
            let o = i * bpp
            seen.insert(UInt32(data[o]) << 16 | UInt32(data[o + 1]) << 8 | UInt32(data[o + 2]))
        }
    }
    return (true, seen.count)
}

@MainActor
func runSelfTest(library: String, out: String) {
    do { try FileManager.default.createDirectory(atPath: out, withIntermediateDirectories: true) } catch {
        FileHandle.standardError.write(Data("cannot create \(out): \(error)\n".utf8)); exit(2)
    }
    var failures = 0
    func check(_ step: String, _ ok: Bool, _ extra: [String: Any] = [:]) {
        if !ok { failures += 1 }
        var d: [String: Any] = ["step": step, "ok": ok]
        d.merge(extra) { a, _ in a }
        emit(d)
    }
    func finish() -> Never {
        emit(["step": "done", "ok": failures == 0, "failures": failures,
              "windows_ordered_front": NSApp.windows.filter { $0.isVisible }.count])
        exit(failures == 0 ? 0 : 1)
    }
    func keyEvent(_ code: UInt16, down: Bool) -> NSEvent {
        NSEvent.keyEvent(with: down ? .keyDown : .keyUp, location: .zero, modifierFlags: [], timestamp: 0,
                         windowNumber: 0, context: nil, characters: "", charactersIgnoringModifiers: "",
                         isARepeat: false, keyCode: code)!
    }

    let timing = ProcessInfo.processInfo.environment["ORIGAMI_SELFTEST_TIMING"] == "1"
    let libraryURL = URL(fileURLWithPath: library)
    let app = AppModel(root: libraryURL)
    guard let host = app.host else { check("core", false, ["error": app.launchError ?? "?"]); finish() }
    let engine = host.core.engines().first?.id ?? "?"
    let fake = engine == "fake"
    check("core", true, ["engine": engine, "library": library, "frame_plane_abi": origami_frame_abi_version()])
    check("frame-plane-version", origami_frame_abi_version() == UInt32(ORIGAMI_FRAME_ABI_VERSION), [:])

    // 1. The library. With the fake engine we make our own machine.
    #if FAKE
    if fake {
        let prom = library + "/prom.bin"
        FileManager.default.createFile(atPath: prom, contents: Data(count: 16))
        do {
            let op = try host.core.createMachine(request: OrigamiCreateRequest(
                name: "alpha", offering: "fake.1", memoryPerNodeMib: nil, graphics: nil, bootProm: prom, ioProm: nil))
            check("create-machine", op.wait(timeoutMs: 10_000) == .succeeded, [:])
        } catch { check("create-machine", false, ["error": "\(error)"]) }
        app.rescan()
    }
    #endif
    let wanted = ProcessInfo.processInfo.environment["ORIGAMI_TEST_MACHINE"]
    guard let model = (wanted.flatMap { app.model(named: $0) } ?? app.models.first) else {
        check("library", false, ["error": "no machines in \(library)"]); finish()
    }
    check("library-scan", !model.record.productName.isEmpty,
          ["machines": app.models.map(\.name), "chosen": model.name, "product": model.record.productName,
           "memory": model.record.memory, "nodes": model.record.nodes])
    check("library-not-running", model.state == .stopped, ["state": model.state.pill])

    // 2. The command table.
    var keys: [String: CommandID] = [:]
    var clash: [String] = []
    for c in CommandID.allCases {
        guard let k = c.info.key else { continue }
        let tag = "\(k.character)-\(c.info.modifiers.rawValue)"
        if let other = keys[tag] { clash.append("\(c.rawValue)/\(other.rawValue)") } else { keys[tag] = c }
    }
    check("command-table", clash.isEmpty && Set(CommandID.allCases.map(\.info.menu)) == [.file, .machine, .view, .window],
          ["commands": CommandID.allCases.count, "clashes": clash])
    let coreCommands = Set(host.core.commands().map(\.id))
    check("core-command-table", coreCommands.contains("machine.start"), ["core_commands": coreCommands.sorted()])
    app.select(model.id)
    check("commands-enabled-at-rest", app.isEnabled(.start) && !app.isEnabled(.powerOff) && !app.isEnabled(.reset)
          && !app.isEnabled(.forceQuit), [:])
    check("window-never-shown", NSApp.windows.allSatisfy { !$0.isVisible }, [:])

    // 3. The SwiftUI views, rendered with no window.
    let hero = snapshot(Hero(model: model).environmentObject(app), size: CGSize(width: 760, height: 560), to: "\(out)/ui-hero.png")
    check("ui-hero-renders", hero.ok && hero.colours > 20, ["distinct_colours_sampled": hero.colours])
    let sidebar = snapshot(LibrarySidebar().environmentObject(app), size: CGSize(width: 260, height: 420), to: "\(out)/ui-sidebar.png")
    check("ui-sidebar-renders", sidebar.ok && sidebar.colours > 5, ["distinct_colours_sampled": sidebar.colours])
    let startingUI = snapshot(StartingCard(model: model).environmentObject(app), size: CGSize(width: 760, height: 560), to: "\(out)/ui-starting.png")
    check("ui-starting-renders", startingUI.ok && startingUI.colours > 20, ["distinct_colours_sampled": startingUI.colours])
    let whole = snapshot(MainView().environmentObject(app), size: CGSize(width: 1000, height: 640), to: "\(out)/ui-main-stopped.png")
    check("ui-main-renders", whole.ok && whole.colours > 20, ["distinct_colours_sampled": whole.colours])

    // 4. Start it the way the Start command does. Events drive every state change.
    var startError: Error?? = .none
    model.start { e in startError = .some(e) }
    check("state-starting", model.state == .starting && model.busy == "Starting…", ["state": model.state.pill])
    check("start-returns", waitFor(180) { startError != nil } && (startError ?? nil) == nil, ["error": "\(String(describing: startError))"])
    guard let session = model.session else { check("session", false, ["error": "no session after start"]); finish() }
    check("emulator-up-by-event", waitFor(60) { model.emulatorUp }, ["state": model.state.pill])

    if !fake {
        // The QEMU engine: what the FFI wires so far.
        let picture = waitFor(30) { model.hasPicture }
        check("qemu-picture-reported", picture, ["heads": session.machine.view().heads.count,
              "picture": "\(String(describing: model.picture))", "state": model.state.pill])
        // Pixel compare: what the core's renderer drew into our layer, read back from the
        // GPU, against QEMU's own screendump once the guest holds still.
        if let h = session.head, picture {
            let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: h.width, height: h.height),
                                  styleMask: [.titled, .resizable], backing: .buffered, defer: false)
            window.isReleasedWhenClosed = false
            let view = model.makeView()
            window.contentView = view
            view.frame = NSRect(x: 0, y: 0, width: h.width, height: h.height)
            check("qemu-core-draws-in-unshown-window", waitFor(30) { view.redraw(); return view.presented > 0 },
                  ["presented": view.presented, "last_drawn": view.lastDrawn, "view_error": view.lastError])
            // Wait until the guest holds still: the head's sequence unchanged for 3 s.
            var lastSeq = session.head?.seq ?? 0, since = Date()
            let still = waitFor(90) {
                model.refresh(); session.updateHead()
                let q = session.cachedHead?.seq ?? 0
                if q != lastSeq { lastSeq = q; since = Date() }
                return Date().timeIntervalSince(since) >= 3
            }
            check("qemu-guest-settled", still, ["seq": lastSeq])
            view.redraw()
            let ppm = "\(out)/qemu-screendump.ppm"
            try? FileManager.default.removeItem(atPath: ppm)
            var err = ""
            do { try session.machine.screendump(head: 0, path: ppm) } catch { err = "core screendump: \(error)" }
            check("qemu-screendump", FileManager.default.fileExists(atPath: ppm), ["error": err])
            if let pic = readPPM(ppm), let rb = view.readback() {
                // The view may be drawn at the backing scale: every readback pixel must equal
                // the screendump pixel it scales from.
                let k = rb.width / max(pic.w, 1)
                if k >= 1, rb.width == pic.w * k, rb.height == pic.h * k {
                    var bad = 0
                    for y in 0..<rb.height { for x in 0..<rb.width {
                        let o = ((y / k) * pic.w + x / k) * 3, c = (y * rb.width + x) * 4
                        if pic.rgb[o] != rb.bgra[c + 2] || pic.rgb[o + 1] != rb.bgra[c + 1] || pic.rgb[o + 2] != rb.bgra[c] { bad += 1 }
                    } }
                    check("qemu-pixel-compare", bad == 0, ["pixels_compared": rb.width * rb.height, "mismatches": bad,
                          "readback": "\(rb.width)x\(rb.height)", "screendump": "\(pic.w)x\(pic.h)", "scale": k])
                } else {
                    check("qemu-pixel-compare", false, ["error": "sizes differ", "readback": "\(rb.width)x\(rb.height)", "screendump": "\(pic.w)x\(pic.h)"])
                }
            } else {
                check("qemu-pixel-compare", false, ["error": "no screendump or no readback", "view_error": view.lastError])
            }
            view.detach()
        }
        for code: UInt16 in [4, 34, 45, 9] {
            session.key(MacKeys.table[code]!.rawValue, down: true); session.key(MacKeys.table[code]!.rawValue, down: false)
        }
        check("qemu-keys-sent-and-released", session.releaseAll() == 0, ["note": "no guest-visible effect is checked"])
        app.confirm = { _, _, _ in true }
        var stopped: Error?? = .none
        model.stop { e in stopped = .some(e) }
        check("qemu-power-off", waitFor(300) { stopped != nil } && (stopped ?? nil) == nil, ["state": model.state.pill])
        finish()
    }

    // 5. Fake engine: the frame path.
    #if FAKE
    let key = model.record.key
    func advance() {
        do { try host.core.fakeAdvanceFrame(key: key) } catch { check("fake-advance-frame", false, ["error": "\(error)"]) }
    }
    let wakes0 = session.wakes
    advance()
    check("wake-callback", waitFor(10) { session.wakes > wakes0 }, ["wakes": session.wakes])
    check("state-running", waitFor(10) { model.state == .running } && model.picture?.width == 64 && model.picture?.height == 48,
          ["state": model.state.pill, "picture": "\(String(describing: model.picture))"])
    check("commands-enabled-running", app.isEnabled(.powerOff) && app.isEnabled(.reset) && !app.isEnabled(.start) && app.isEnabled(.captureInput), [:])

    var pixels: [UInt8] = []
    let taken = session.takeCopy(into: &pixels)
    check("frame-copy-path", taken != nil && taken!.frame.width == 64 && taken!.frame.height == 48 && taken!.frame.bytes_used == 64 * 48 * 4,
          ["bytes_used": taken.map { Int($0.frame.bytes_used) } ?? -1])
    check("frame-copy-path-then-none", session.takeCopy(into: &pixels) == nil, [:])

    // The display view in a window that is never shown.
    let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 640, height: 512),
                          styleMask: [.titled, .resizable], backing: .buffered, defer: false)
    window.isReleasedWhenClosed = false
    window.acceptsMouseMovedEvents = true
    let view = model.makeView()
    window.contentView = view
    view.frame = NSRect(x: 0, y: 0, width: 640, height: 512)
    check("view-in-hidden-window", !window.isVisible && view.window === window && model.view === view
          && String(describing: type(of: view.layer!)) == "CAMetalLayer", [:])
    advance()
    check("core-draws-offscreen-in-unshown-window", waitFor(30) { view.presented > 0 },
          ["presented": view.presented, "view_error": view.lastError, "last_drawn": view.lastDrawn,
           "note": "the window is never on screen, so the core reports 3 (drawn offscreen), not presented"])
    // The pixels: a readback has content and changes when the guest's picture does.
    do {
        let p0 = view.presented
        let first = view.readback()
        advance(); _ = waitFor(10) { view.presented > p0 }
        let second = view.readback()
        var seen = Set<UInt32>()
        if let f = first {
            for i in stride(from: 0, to: f.bgra.count, by: 4) {
                seen.insert(UInt32(f.bgra[i]) | UInt32(f.bgra[i + 1]) << 8 | UInt32(f.bgra[i + 2]) << 16)
            }
        }
        let colours = seen.count
        check("fake-readback-has-pixels", first != nil && colours >= 2 && first?.bgra != second?.bgra,
              ["distinct_colours": colours, "size": first.map { "\($0.width)x\($0.height)" } ?? "none",
               "changed_after_advance": first?.bgra != second?.bgra, "view_error": view.lastError])
    }

    // Idle: nothing changed, so nothing is drawn.
    do {
        spin(0.5)
        let pres = view.presented
        let p0 = processCPU(); spin(5); let p1 = processCPU()
        check("perf-idle", view.presented == pres, ["presents_while_idle": view.presented - pres,
              "process_cpu_ms": ((p1.user - p0.user) + (p1.sys - p0.sys)) * 1000, "wall_s": 5])
    }
    // Frame cost: CPU the app spends per presented frame.
    do {
        let pres0 = view.presented, draw0 = view.drawNanos
        for _ in 0..<60 { advance(); spin(0.02) }
        spin(0.5)
        let n = view.presented - pres0
        let us = Double(view.drawNanos - draw0) / 1000.0 / Double(max(n, 1))
        check("perf-frame", n > 0 && (!timing || us <= 500), ["presented": n, "app_cpu_us_per_presented_frame": us, "budget_us": 500, "budget_enforced": timing])
    }
    // Hidden: a hidden view draws nothing.
    do {
        view.setVisible(false)
        let pres = view.presented
        for _ in 0..<5 { advance(); spin(0.1) }
        check("hidden-tab-draws-nothing", view.presented == pres, ["presents_while_hidden": view.presented - pres])
        view.setVisible(true)
    }

    // 6. Input. Keys go through the view's own handlers.
    view.keyDown(with: keyEvent(0, down: true))   // 'a' down
    view.keyDown(with: keyEvent(56, down: true))  // shift
    check("key-down-reaches-core", session.releaseAll() >= 1, [:])
    view.keyUp(with: keyEvent(0, down: false)); view.keyUp(with: keyEvent(56, down: false))
    view.keyDown(with: keyEvent(0x7f, down: true))
    check("unmapped-key-ignored", session.releaseAll() == 0, [:])

    // Modifiers: flags changes for a non-modifier key code send nothing, a release with
    // nothing held sends no press, and a press and release follow the event's flag bit.
    func flagsEvent(_ code: UInt16, raw: UInt) -> NSEvent {
        NSEvent.keyEvent(with: .flagsChanged, location: .zero, modifierFlags: NSEvent.ModifierFlags(rawValue: raw), timestamp: 0,
                         windowNumber: 0, context: nil, characters: "", charactersIgnoringModifiers: "",
                         isARepeat: false, keyCode: code)!
    }
    view.flagsChanged(with: flagsEvent(0, raw: 0x40001))   // key code A with Control flags
    check("flags-change-for-other-key-sends-nothing", session.releaseAll() == 0, [:])
    view.flagsChanged(with: flagsEvent(59, raw: 0))        // Control released, none held
    check("control-release-without-held-sends-no-press", session.releaseAll() == 0, [:])
    view.flagsChanged(with: flagsEvent(59, raw: 0x40001))  // left Control pressed
    let down = session.releaseAll()
    view.flagsChanged(with: flagsEvent(59, raw: 0))
    check("control-press-follows-flag-bit", down >= 1 && session.releaseAll() == 0, ["held_after_press": down])

    // A letter held through a Command chord is lifted when Command goes up.
    view.keyDown(with: keyEvent(0, down: true))
    view.flagsChanged(with: flagsEvent(55, raw: 0x100008))
    view.flagsChanged(with: flagsEvent(55, raw: 0))
    check("command-release-lifts-held-letters", session.releaseAll() == 0, [:])
    view.keyDown(with: keyEvent(82, down: true))   // keypad 0
    check("keypad-key-reaches-core", session.releaseAll() >= 1, [:])
    check("keypad-keys-are-keypad-keys", MacKeys.table[82] == .numpad0 && MacKeys.table[76] == .numpadEnter
          && Key.numpadEnter.rawValue == UInt32(ORIGAMI_KEY_NUMPAD_ENTER.rawValue)
          && Key.numLock.rawValue == UInt32(ORIGAMI_KEY_NUM_LOCK.rawValue), [:])

    // follow-mouse is the default; absolute positions use the head's pixels.
    let mouse = { (t: NSEvent.EventType, x: Double, y: Double) -> NSEvent in
        NSEvent.mouseEvent(with: t, location: NSPoint(x: x, y: Double(view.bounds.height) - y), modifierFlags: [], timestamp: 0,
                           windowNumber: window.windowNumber, context: nil, eventNumber: 0, clickCount: 1, pressure: 1)!
    }
    check("pointer-mode-default-follow", model.pointerMode == .follow && view.absolutePointer, [:])
    view.mouseMoved(with: mouse(.mouseMoved, 320, 256))
    view.mouseDown(with: mouse(.leftMouseDown, 320, 256))
    check("follow-click-is-the-guests", !view.captured && session.releaseAll() >= 1, ["note": "a click in follow mode presses the guest's button"])

    // capture: a click captures (and is not the guest's), motion goes as relative counts,
    // Control+Option releases, and nothing stays down.
    app.invoke(.followMouse)
    view.absolutePointer = model.pointerMode == .follow
    check("pointer-mode-toggle-command", model.pointerMode == .capture && !view.absolutePointer, [:])
    view.mouseDown(with: mouse(.leftMouseDown, 400, 300)); view.mouseUp(with: mouse(.leftMouseUp, 400, 300))
    let capturedNow = view.captured && model.captured
    let hud = model.hudUntil != nil
    view.applyMouseDelta(6.4, 3.0)
    view.keyDown(with: keyEvent(0, down: true))
    let flags = NSEvent.keyEvent(with: .flagsChanged, location: .zero, modifierFlags: [.control, .option], timestamp: 0,
                                 windowNumber: 0, context: nil, characters: "", charactersIgnoringModifiers: "",
                                 isARepeat: false, keyCode: 61)!
    view.flagsChanged(with: flags)
    check("capture-and-release", capturedNow && hud && !view.captured && !model.captured && session.releaseAll() == 0, [
        "captured_after_click": capturedNow, "hint_shown": hud, "released_by_ctrl_option": !view.captured,
        "note": "releasing lifted the held key, so release-all had nothing left"])
    app.invoke(.followMouse)
    view.absolutePointer = model.pointerMode == .follow
    check("pointer-mode-back-to-follow", model.pointerMode == .follow && view.absolutePointer, [:])

    // Focus loss lifts everything too.
    view.keyDown(with: keyEvent(0, down: true))
    NotificationCenter.default.post(name: NSApplication.didResignActiveNotification, object: nil)
    spin(0.2)
    check("focus-loss-releases-keys", session.releaseAll() == 0, [:])

    // 7. The diagnostics report and the running view's card.
    let report = diagnosticsReport(model)
    check("diagnostics-report", report.contains(model.name) && report.contains("Engine: fake"), ["lines": report.split(separator: "\n").count])
    let running = snapshot(ScreenView(model: model).environmentObject(app), size: CGSize(width: 900, height: 700), to: "\(out)/ui-screen-view.png")
    check("ui-screen-view-renders", running.ok && running.colours > 5, ["distinct_colours_sampled": running.colours])

    // 8. Power: Reset, Power Off (Saving), Start again, and the left-running path.
    app.confirm = { _, _, _ in true }
    var resetErr: Error?? = .none
    model.reset { e in resetErr = .some(e) }
    check("power-reset", waitFor(60) { resetErr != nil } && (resetErr ?? nil) == nil, ["error": "\(String(describing: resetErr))"])

    // A second view takes over the frame handler; detaching the first must not unhook it,
    // and a detached view must not draw again.
    let view2 = model.makeView()
    window.contentView = view2
    view2.frame = NSRect(x: 0, y: 0, width: 640, height: 512)
    view.detach()
    advance()
    check("detach-keeps-the-new-views-handler", waitFor(30) { view2.presented > 0 }, ["presented": view2.presented])
    let before = view.presented
    view.redraw()
    check("detached-view-does-not-redraw", view.presented == before, [:])
    view2.detach()

    app.invoke(.powerOff)
    check("state-saving", model.state == .stopping && model.state.pill == "Saving", ["state": model.state.pill])
    check("power-off", waitFor(60) { model.busy == nil && model.state == .stopped }, ["state": model.state.pill])
    check("state-stopped", model.state == .stopped && app.isEnabled(.start), [:])

    // stop always calls done: on a stopped machine at once, and queued behind a start.
    var doneStopped = false
    model.stop { _ in doneStopped = true }
    check("stop-on-stopped-calls-done", doneStopped, [:])
    app.invoke(.start)
    var doneQueued = false
    model.stop { _ in doneQueued = true }
    check("stop-queued-behind-start-calls-done", waitFor(120) { doneQueued && model.state == .stopped }, ["state": model.state.pill])
    app.invoke(.start)
    check("start-again", waitFor(60) { model.state == .running || model.state == .starting }, ["state": model.state.pill])
    _ = waitFor(30) { model.emulatorUp }

    var stopped = false
    var failedNames: [String] = []
    app.stopAll { failed in failedNames = failed.map(\.name); stopped = true }
    check("stop-all", waitFor(120) { stopped } && failedNames.isEmpty && model.state == .stopped,
          ["state": model.state.pill, "failed": failedNames])
    #else
    check("fake-build", false, ["error": "the core reports the fake engine but this app was built without -D FAKE"])
    #endif
    finish()
}
