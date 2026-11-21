// `OrigamiApp --selftest-display LIBRARY OUTDIR`: the real machine window on screen, driven through the real
// View menu commands, resizes, zoom and tab switches, and read back against the core's behaviour table
// (origami_behaviour_json). It needs the fake engine and a window server. Fake-engine builds only: never a release.

#if FAKE
import AppKit
import Foundation
import OrigamiFrame
import SwiftUI

/// A window the system may make bigger than a small runner's screen, so every row has its room.
final class UnconstrainedWindow: NSWindow {
    override func constrainFrameRect(_ frameRect: NSRect, to screen: NSScreen?) -> NSRect { frameRect }
}

@MainActor
func runDisplaySelfTest(library: String, out: String) {
    var failures = 0, ran = 0
    var skipped: [String] = []
    func emitStep(_ step: String, _ ok: Bool, _ extra: [String: Any] = [:]) {
        if !ok { failures += 1 }
        var d: [String: Any] = ["step": step, "ok": ok]
        d.merge(extra) { a, _ in a }
        emit(d)
    }
    func finish() -> Never {
        emit(["step": "done", "ok": failures == 0, "failures": failures, "rows_run": ran, "rows_skipped": skipped])
        exit(failures == 0 ? 0 : 1)
    }
    func fail(_ why: String) -> Never { emitStep("setup", false, ["error": why]); finish() }
    do { try FileManager.default.createDirectory(atPath: out, withIntermediateDirectories: true) } catch { fail("cannot create \(out)") }

    guard let doc = try? JSONSerialization.jsonObject(with: Data(origamiBehaviourJson().utf8)) as? [String: Any],
          let rows = doc["rows"] as? [[String: Any]], let guestArr = doc["guest"] as? [Int]
    else { fail("the behaviour table does not parse") }
    let guest = (w: guestArr[0], h: guestArr[1])

    let app = AppModel(root: URL(fileURLWithPath: library))
    guard let host = app.host else { fail(app.launchError ?? "the core did not start") }
    for name in ["alpha", "beta"] {
        if let e = makeFakeMachine(host, library: library, name: name, offering: "fakebig.1") { fail("creating \(name): \(e)") }
    }
    app.rescanNow()
    guard waitFor(10, { app.models.count == 2 }), let model = app.models.first(where: { $0.name == "alpha" }),
          let other = app.models.first(where: { $0.name == "beta" }) else { fail("the two machines are not listed") }

    // Every row's room is this pane room, wherever the test runs; a window may be bigger than the screen.
    let roomPoints = CGSize(width: 2800, height: 2000)
    FitView.visibleFrame = { _ in CGRect(x: 0, y: 0, width: roomPoints.width, height: roomPoints.height) }

    NSApp.activate(ignoringOtherApps: true)
    let controller = NSHostingController(rootView: MainView().environmentObject(app))
    let win = UnconstrainedWindow(contentRect: NSRect(x: 0, y: 0, width: 1100, height: 720), styleMask: [.titled, .closable, .resizable, .miniaturizable],
                                  backing: .buffered, defer: false)
    // A hosting controller resizes its window to the content's ideal size when the content changes; the app's own
    // window does not follow it, so the test must not either.
    controller.sizingOptions = []
    win.contentViewController = controller
    win.styleMask = [.titled, .closable, .resizable, .miniaturizable]
    win.title = "Origami"
    win.isReleasedWhenClosed = false
    win.setContentSize(NSSize(width: 1100, height: 720))
    win.center()
    win.makeKeyAndOrderFront(nil)
    let backing = win.backingScaleFactor
    emitStep("window", win.isVisible, ["backing_scale": backing, "screen": win.screen.map { "\(Int($0.frame.width))x\(Int($0.frame.height))" } ?? "none"])

    func views(_ v: NSView) -> [MachineView] { (v as? MachineView).map { [$0] } ?? v.subviews.flatMap(views) }
    func start(_ m: MachineModel) {
        app.select(m.id)
        let fitted = FitView.fittedCount
        var started = false
        m.start { _ in started = true }
        guard waitFor(60, { started }), waitFor(60, { m.state == .running }) else { fail("\(m.name) did not start (\(m.state.pill))") }
        try? host.core.fakeAdvanceFrame(key: m.record.key)
        guard waitFor(30, { m.picture?.width == guest.w && m.view?.lastDraw() != nil }) else { fail("\(m.name) has no picture") }
        // A tab's first picture sizes the window once; let that happen here, not when a row first shows the tab.
        guard waitFor(10, { FitView.fittedCount > fitted }) else { fail("\(m.name) was never fitted") }
    }
    func stop(_ m: MachineModel) {
        var stopped = false
        m.stop { _ in stopped = true }
        guard waitFor(60, { stopped && m.state == .stopped }) else { fail("\(m.name) did not stop (\(m.state.pill))") }
    }
    start(other)
    start(model)
    app.select(model.id)
    spin(1)

    /// What the window shows now, in device pixels.
    struct Seen {
        var frame: CGSize
        var pane: CGSize          // logical
        var surface: (w: Int, h: Int)
        var rendered: (x: Int, y: Int, w: Int, h: Int)   // the renderer's picture in the surface
        var offset: (x: Int, y: Int)                     // the surface in the pane
        var picture: (x: Int, y: Int, w: Int, h: Int)    // the picture in the pane
    }
    /// The runner's window is occluded, and the app deliberately draws nothing then: only there is a draw forced, and
    /// every row says so.
    var forced = false
    func seen() -> Seen? {
        if !win.occlusionState.contains(.visible) { forced = true; model.view?.drawForTest() }
        guard let v = model.view, let fit = model.fitter, let d = v.lastDraw(), v.window != nil else { return nil }
        let inset = Scaling.inset
        let pane = CGSize(width: fit.available.width - 2 * inset, height: fit.available.height - 2 * inset)
        let b = win.backingScaleFactor
        let vr = v.convert(v.bounds, to: nil), fr = fit.convert(fit.bounds, to: nil)
        // The zero-size fitter sits at the middle of the pane's area; the picture's offset is how far its middle is from there.
        let dx = Int(((vr.midX - fr.minX) * b).rounded()), dy = Int(((fr.minY - vr.midY) * b).rounded())
        let (sw, sh) = (Int(d.surface_width), Int(d.surface_height))
        let (pw, ph) = (Int((pane.width * b).rounded()), Int((pane.height * b).rounded()))
        let (ox, oy) = ((pw - sw) / 2 + dx, (ph - sh) / 2 + dy)
        return Seen(frame: win.frame.size, pane: pane, surface: (sw, sh), rendered: (Int(d.x), Int(d.y), Int(d.width), Int(d.height)),
                    offset: (ox, oy), picture: (ox + Int(d.x), oy + Int(d.y), Int(d.width), Int(d.height)))
    }
    /// Wait for the window frame and the last draw to stop changing.
    func settle() -> Seen? {
        var last: Seen?
        var stable = 0
        let end = Date().addingTimeInterval(8)
        while Date() < end {
            spin(0.1)
            guard let s = seen() else { continue }
            if let l = last, l.frame == s.frame, l.surface == s.surface, l.picture == s.picture { stable += 1 } else { stable = 0 }
            last = s
            if stable >= 5 { return s }
        }
        return last
    }
    /// Resize the window until the pane is `pane` points; false if it cannot get there (a minimum size).
    func setPane(_ pane: [Int]) -> Bool {
        for _ in 0..<6 {
            guard let s = settle() else { return false }
            let (dw, dh) = (CGFloat(pane[0]) - s.pane.width, CGFloat(pane[1]) - s.pane.height)
            if abs(dw) < 0.5 && abs(dh) < 0.5 { return true }
            win.setContentSize(NSSize(width: win.contentLayoutRect.width + dw, height: win.contentLayoutRect.height + dh))
        }
        guard let s = settle() else { return false }
        return abs(CGFloat(pane[0]) - s.pane.width) <= 1 && abs(CGFloat(pane[1]) - s.pane.height) <= 1
    }

    let menu: [String: CommandID] = ["view-fit": .fit, "view-actual": .actualSize, "view1x": .pointScale, "view2x": .doubleScale]
    func fitsRoom(_ room: [Int]?) -> Bool {
        // The pane room: the screen's room less the window chrome and the granite margins.
        guard let room, let s = seen() else { return true }
        let chrome = CGSize(width: s.frame.width - s.pane.width, height: s.frame.height - s.pane.height)
        return CGFloat(room[0]) <= roomPoints.width - chrome.width && CGFloat(room[1]) <= roomPoints.height - chrome.height
    }
    /// The core's checker: JSON in, JSON out.
    func evaluate(_ request: [String: Any]) -> [String: Any] {
        let data = try! JSONSerialization.data(withJSONObject: request)
        let out = origamiBehaviourEvaluate(request: String(data: data, encoding: .utf8)!)
        return (try? JSONSerialization.jsonObject(with: Data(out.utf8)) as? [String: Any]) ?? ["problems": ["unreadable answer: \(out)"]]
    }
    func rect(_ r: (x: Int, y: Int, w: Int, h: Int)) -> [Int] { [r.x, r.y, r.w, r.h] }

    for row in rows {
        let id = row["id"] as! String
        let action = row["action"] as! String
        let pane = row["pane"] as? [Int]
        var room: [Int]?
        if let s0 = seen() {
            // The pane room: the screen's room less the window chrome and the granite margins.
            room = [Int(roomPoints.width - (s0.frame.width - s0.pane.width)), Int(roomPoints.height - (s0.frame.height - s0.pane.height))]
        }
        let ask: [String: Any] = ["op": "skip", "id": id, "scale": Double(backing), "room_available": room.map { $0 as Any } ?? NSNull(), "pane_reached": true]
        if let why = evaluate(ask)["skip"] as? String { skipped.append("\(id): \(why)"); continue }
        if let before = row["view_before"] as? String, let c = menu[before] { app.invoke(c) }
        // An action that shrinks the window may be refused by its minimum size: try that pane first.
        if let r = row["room"] as? [Int], let p = pane, r[0] < p[0] || r[1] < p[1], !setPane(r) {
            var late = ask; late["pane_reached"] = false
            skipped.append("\(id): \(evaluate(late)["skip"] as? String ?? "the window cannot get there")"); continue
        }
        if let pane, !setPane(pane) {
            var late = ask; late["pane_reached"] = false
            skipped.append("\(id): \(evaluate(late)["skip"] as? String ?? "the window cannot get there")"); continue
        }
        guard let was = settle() else { emitStep(id, false, ["error": "no picture before the action"]); continue }
        let viewBefore = model.view.map(ObjectIdentifier.init), presentsBefore = model.view?.presented ?? 0
        forced = false
        switch action {
        case "resize": break
        case "zoom": win.zoom(nil)
        case "reattach":
            app.select(other.id); spin(1.5)
            app.select(model.id)
        case "first-window":
            stop(model); start(model)
        default:
            if let c = menu[action] { app.invoke(c) } else { emitStep(id, false, ["error": "unknown action \(action)"]); continue }
        }
        guard let now = settle() else { emitStep(id, false, ["error": "no picture after the action"]); continue }
        ran += 1
        // The app's own redraw path ran if the view presented again, or a new view presented at all.
        let sameView = model.view.map(ObjectIdentifier.init) == viewBefore
        let drew: Any = forced ? NSNull() : ((model.view?.presented ?? 0) > (sameView ? presentsBefore : 0))
        spin(1)
        let later = seen()
        var request: [String: Any] = [
            "op": "check", "id": id, "scale": Double(backing),
            "pane_dev": [Int((now.pane.width * backing).rounded()), Int((now.pane.height * backing).rounded())],
            "picture_before": rect(was.picture), "picture": rect(now.picture),
            "surface": [now.surface.w, now.surface.h], "rendered": rect(now.rendered),
            "frame_before": [Int(was.frame.width), Int(was.frame.height)], "frame": [Int(now.frame.width), Int(now.frame.height)],
            "drew": drew,
        ]
        if let l = later { request["later"] = ["frame": [Int(l.frame.width), Int(l.frame.height)], "picture": rect(l.picture)] }
        let problems = evaluate(request)["problems"] as? [String] ?? ["no answer"]
        emitStep(id, problems.isEmpty, ["problems": problems, "picture": rect(now.picture), "surface": [now.surface.w, now.surface.h],
                                        "frame": [Int(now.frame.width), Int(now.frame.height)], "draw_forced": forced])
    }
    if let png = model.view?.readback() { writePNG(png, to: "\(out)/display-picture.png") }
    // A view attached before the one it replaces detaches, as SwiftUI may do on a tab switch, shows the
    // picture the old one leaves behind: the machine keeps it and wakes the new view.
    if let first = model.view {
        let second = MachineView(session: model.session!)
        second.drawForTest()
        let attachedEmpty = second.lastDraw() == nil
        first.detach()
        let shown = waitFor(5) { second.drawForTest(); return second.lastDraw() != nil }
        emitStep("overlapping-views-hand-the-picture-over", attachedEmpty && shown, ["new_view_empty_first": attachedEmpty, "new_view_has_picture": shown])
        second.detach()
    }
    emitStep("rows-ran", ran >= 6, ["ran": ran, "skipped": skipped.count])
    // The window as the screen shows it, for whoever reads the readbacks; it needs the screen permission and may not be given.
    let shot = Process()
    shot.executableURL = URL(fileURLWithPath: "/usr/sbin/screencapture")
    shot.arguments = ["-x", "-o", "-l\(win.windowNumber)", "\(out)/display-window.png"]
    try? shot.run()
    shot.waitUntilExit()
    emit(["step": "window-capture", "ok": true, "file": "display-window.png", "exists": FileManager.default.fileExists(atPath: "\(out)/display-window.png")])
    var stopped = false
    app.stopAll(force: true) { _ in stopped = true }
    _ = waitFor(60) { stopped }
    finish()
}
#endif
