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
import Combine
import Foundation
import OrigamiFrame
import SwiftUI

/// The view at 2x in the given appearance, as a PNG. The bitmap's pixel size is twice its point size, so
/// the drawing is done at device scale 2 as on a Retina display.
@MainActor
func snapshotRetina<V: View>(_ view: V, size: CGSize, dark: Bool, to path: String) -> Bool {
    let host = NSHostingView(rootView: view.frame(width: size.width, height: size.height))
    host.appearance = NSAppearance(named: dark ? .darkAqua : .aqua)
    host.frame = CGRect(origin: .zero, size: size)
    // A list draws its rows only inside a window; this one is never ordered front.
    let window = NSWindow(contentRect: CGRect(origin: .zero, size: size), styleMask: [.borderless], backing: .buffered, defer: false)
    window.isReleasedWhenClosed = false
    window.contentView = host
    host.layoutSubtreeIfNeeded()
    spin(0.5)
    host.layoutSubtreeIfNeeded()
    guard let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: Int(size.width * 2), pixelsHigh: Int(size.height * 2),
                                     bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
                                     colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0) else { return false }
    rep.size = size
    host.cacheDisplay(in: host.bounds, to: rep)
    guard let png = rep.representation(using: .png, properties: [:]) else { return false }
    return (try? png.write(to: URL(fileURLWithPath: path))) != nil
}

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
        let made = makeFakeMachine(host, library: library, name: "alpha")
        check("create-machine", made == nil, made.map { ["error": "\($0)"] } ?? [:])
        app.rescanNow()
    }
    #endif
    // Picture sizing: 1 guest pixel per point where it fits, else the largest whole-number scale.
    do {
        let g = CGSize(width: 1280, height: 1024)
        func size(_ w: CGFloat, _ h: CGFloat, _ b: CGFloat, _ m: ScaleMode) -> CGSize {
            Scaling.viewSize(guest: g, available: CGSize(width: w, height: h), backing: b, mode: m)
        }
        let aspect = size(500, 400, 2, .points)
        // Pointer mapping follows the picture's rectangle at 1x points, 2x points and the whole-scale fit.
        func mapped(_ m: ScaleMode, _ w: CGFloat, _ h: CGFloat) -> Bool {
            let v = Scaling.viewSize(guest: g, available: CGSize(width: w, height: h), backing: 2, mode: m)
            let a = MachineView.guestPoint(CGPoint(x: v.width / 2, y: v.height / 2), view: v, head: g)
            let b = MachineView.guestPoint(CGPoint(x: 0, y: 0), view: v, head: g)
            let c = MachineView.guestPoint(CGPoint(x: v.width - 0.01, y: v.height - 0.01), view: v, head: g)
            return a == (640, 512) && b == (0, 0) && c == (1279, 1023)
        }
        // The picture's origin lands on a whole device pixel at 1x and 2x, with fractional or odd sizes.
        check("origin-on-a-device-pixel", [(CGFloat(1), CGSize(width: 1001, height: 700)), (2, CGSize(width: 1000.5, height: 733))].allSatisfy { b, c in
            let v = size(c.width - 60, c.height - 60, b, .fit)
            let d = Scaling.snap(container: c, size: v, backing: b)
            let x = ((c.width - v.width) / 2 + d.width) * b, y = ((c.height - v.height) / 2 + d.height) * b
            return abs(x - x.rounded()) < 1e-6 && abs(y - y.rounded()) < 1e-6
        }, [:])
        check("scale-label", Scaling.label(mode: .fit, size: size(1000, 700, 2, .fit), guest: g, backing: 2) == "Fit 100%"
              && Scaling.label(mode: .points, size: size(1500, 1100, 2, .points), guest: g, backing: 2) == "200%"
              && Scaling.label(mode: .actual, size: size(2600, 2100, 2, .actual), guest: g, backing: 2) == "100%", [:])
        check("pointer-mapping-follows-the-scale", mapped(.points, 1500, 1100) && mapped(.double, 2600, 2100) && mapped(.fit, 1000, 700) && mapped(.points, 500, 400), [:])
        check("scaling-modes", size(1500, 1100, 2, .points) == CGSize(width: 1280, height: 1024)
              && size(1000, 700, 2, .fit) == CGSize(width: 640, height: 512)   // 1.37 device px steps down to 1, never blurry
              && abs(size(500, 400, 2, .fit).height - 400) < 0.5   // below 1 device pixel it fills
              && size(2600, 2100, 2, .double) == CGSize(width: 2560, height: 2048)
              && size(2600, 2100, 2, .actual) == CGSize(width: 640, height: 512)
              && size(2600, 2100, 1, .fit) == CGSize(width: 2560, height: 2048)
              && abs(aspect.width / aspect.height - 1.25) < 0.001 && aspect.height <= 400,
              ["aspect_fit_when_too_small": "\(aspect.width)x\(aspect.height)"])
    }

    // A second machine of the same product, to check selection keys rows by path.
    if fake {
        do {
            let op = try host.core.createMachine(request: OrigamiCreateRequest(
                name: "beta", offering: "fake.1", memoryPerNodeMib: nil, graphics: nil, bootProm: library + "/prom.bin", ioProm: nil, engine: nil))
            check("create-second-machine", op.wait(timeoutMs: 10_000) == .succeeded, [:])
            app.rescanNow()
        } catch { check("create-second-machine", false, ["error": "\(error)"]) }
    }
    // Settings: read through the core off the main thread, edit while stopped, refused while running. A machine of its
    // own, because editing the file must not change the one the rest of the run starts.
    if fake {
        do {
            let op = try host.core.createMachine(request: OrigamiCreateRequest(
                name: "settingsvic", offering: "fake.1", memoryPerNodeMib: nil, graphics: nil, bootProm: library + "/prom.bin", ioProm: nil, engine: nil))
            _ = op.wait(timeoutMs: 10_000)
            app.rescanNow()
        } catch { check("create-settings-machine", false, ["error": "\(error)"]) }
        if let victim = app.model(named: "settingsvic") {
            let sm = SettingsModel(machine: victim)
            sm.load()
            let loaded = waitFor(30) { sm.settings != nil || sm.error != nil }
            let s = sm.settings
            check("settings-load-through-the-core", loaded && s != nil && (s?.memoryPerNodeMib ?? 0) > 0,
                  ["memory_mib": Int(s?.memoryPerNodeMib ?? 0), "allowed": (s?.memoryAllowedMib ?? []).map { Int($0) }, "error": sm.error ?? "none"])
            if let s, let other = s.memoryAllowedMib.first(where: { $0 != s.memoryPerNodeMib }) {
                sm.apply(.setMemory(mib: other))
                let changed = waitFor(30) { sm.settings?.memoryPerNodeMib == other && !sm.working }
                check("settings-edit-while-stopped", changed, ["wanted_mib": Int(other), "error": sm.error ?? "none"])
                sm.apply(.setMemory(mib: s.memoryPerNodeMib))
                let back = waitFor(30) { sm.settings?.memoryPerNodeMib == s.memoryPerNodeMib && !sm.working }
                check("settings-edit-can-be-put-back", back, ["now_mib": Int(sm.settings?.memoryPerNodeMib ?? 0), "error": sm.error ?? "none"])
            } else { check("settings-edit-while-stopped", false, ["error": "the machine offers one memory size"]) }
            // While the machine runs an edit is refused before it reaches the core.
            let wasLocked = sm.locked
            check("settings-locked-follows-the-machine", !wasLocked && victim.state == .stopped, ["locked_at_rest": wasLocked])
            let said = SettingsModel.say(CoreError.Busy(message: "x", detail: nil))
            check("settings-busy-is-told-as-what-to-do", said.contains("Power off"), ["said": said])
        }
    }
    // The About egg: h-i-n-v, however it is broken up, completes the word once; the core's inventory comes back
    // off the main thread and says what the host is.
    do {
        var egg = EggBuffer()
        let typed = Array("xhinv").map { egg.feed($0) }
        var again = EggBuffer()
        let broken = Array("hihinv").map { again.feed($0) }
        var upper = EggBuffer()
        let caps = Array("HINV").map { upper.feed($0) }
        check("about-egg-word-matches-only-hinv", typed == [false, false, false, false, true] && broken.last == true
              && !broken.dropLast().contains(true) && caps.last == true, [:])
        var text = ""
        let done = DispatchSemaphore(value: 0)
        let core = host.core
        DispatchQueue.global().async { text = core.hinv(); done.signal() }
        let got = waitFor(30) { done.wait(timeout: .now()) == .success }
        check("about-egg-hinv-comes-from-the-core", got && !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty,
              ["first_line": String(text.split(separator: "\n").first ?? "")])
    }

    // About's Acknowledgements and Licences: the core's credits document reads back with a credit and a link for
    // every photograph and QEMU's GPL text, and both sheets open and lay out.
    do {
        let doc = CreditsDocument.load()
        let scenes = Int(origami_cube_scene_count())
        let credited = doc.map { d in d.photographs.count == scenes && d.photographs.allSatisfy {
            !$0.author.isEmpty && !$0.year.isEmpty && !$0.licence.isEmpty && URL(string: $0.licenceUrl) != nil && URL(string: $0.source) != nil
        } } ?? false
        let gpl = doc?.licences.first { $0.name == "GNU General Public License" }
        check("credits-every-photograph-has-a-credit-and-a-link", credited, ["scenes": scenes, "photographs": doc?.photographs.count ?? -1])
        check("credits-licences-carry-qemus-gpl-and-source", gpl?.entries.contains { $0.components == ["QEMU"] && $0.text.contains("Version 2") } == true
              && doc?.qemu.url.contains("/tree/sgi-origami-") == true, ["licences": doc?.licences.map(\.name) ?? []])
        func opens<V: View>(_ view: V) -> NSSize {
            let hosting = NSHostingView(rootView: view)
            hosting.layoutSubtreeIfNeeded()
            return hosting.fittingSize
        }
        let ack = opens(AcknowledgementsView()), lic = opens(LicencesView())
        check("about-acknowledgements-and-licences-open", ack.width > 100 && ack.height > 100 && lic.width > 100 && lic.height > 100,
              ["acknowledgements": "\(ack)", "licences": "\(lic)"])
    }

    // Console keys: what a serial terminal is sent, and Option as Meta only when asked for.
    do {
        func k(_ code: UInt16, _ ch: String, _ ign: String? = nil, _ flags: NSEvent.ModifierFlags = [], meta: Bool = false) -> ConsoleKeys.Mapped? {
            ConsoleKeys.key(keyCode: code, chars: ch, ignoring: ign ?? ch, flags: flags, optionAsMeta: meta)
        }
        let plain = k(0, "a") == .init(key: .text(text: "a"))
        let special = k(126, "")?.key == .up && k(36, "\r")?.key == .enter && k(51, "")?.key == .backspace && k(122, "")?.key == .function(number: 1)
            && k(48, "\t", nil, .shift)?.key == .backTab && k(0, "a", nil, .command) == nil
        let ctrl = k(8, "\u{3}", "c", .control) == .init(key: .text(text: "c"), ctrl: true)
        let optionOff = k(3, "ƒ", "f", .option) == .init(key: .text(text: "ƒ"))
        let optionOn = k(3, "ƒ", "f", .option, meta: true) == .init(key: .text(text: "f"), alt: true)
        let commandKept = k(126, "", nil, .command) == nil && k(51, "", nil, .command) == nil
        let wholeString = k(0, "ab") == .init(key: .text(text: "ab")) && k(0, "\u{F700}") == nil
        let arrowMeta = k(126, "", nil, .option, meta: true) == .init(key: .up, alt: true) && k(126, "", nil, .option) == .init(key: .up)
            && k(126, "", nil, [.shift, .control]) == .init(key: .up, shift: true, ctrl: true) && k(48, "\t", nil, .shift)?.shift == false
        check("console-keys", plain && special && ctrl && optionOff && optionOn && commandKept && wholeString && arrowMeta
              && !UserDefaults.standard.bool(forKey: ConsoleKeys.optionAsMetaKey),
              ["plain": plain, "special": special, "ctrl": ctrl, "option_composes_when_off": optionOff, "option_is_meta_when_on": optionOn,
               "command_chords_left_to_the_app": commandKept, "whole_typed_string": wholeString, "arrow_meta": arrowMeta])
    }

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
    // Every row of a library with machines of one product selects its own machine.
    do {
        let ids = app.models.map(\.id)
        var picked: [String?] = []
        for id in ids.reversed() + ids { app.select(id); picked.append(app.active) }
        check("select-each-row-by-key", Set(ids).count == ids.count && picked == Array(ids.reversed() + ids),
              ["machines": app.models.count, "same_product": Set(app.models.map(\.record.productName)).count == 1 && app.models.count > 1])
    }
    // Tabs: a double-click (select, then open in new tab) adds a tab and keeps the old one; the
    // context menu's Open in New Tab adds another without a click.
    do {
        let ids = app.models.map(\.id)
        if ids.count >= 2 {
            app.closeAllTabsForTest()
            app.select(ids[0])
            app.select(ids[1]); app.openInNewTab(ids[1])
            check("double-click-adds-a-tab-and-keeps-the-old-one", app.tabs == [ids[0], ids[1]] && app.active == ids[1], ["tabs": app.tabs.count])
            app.closeAllTabsForTest()
            app.select(ids[0]); app.openInNewTab(ids[1])
            check("open-in-new-tab-adds-a-tab", app.tabs == [ids[0], ids[1]] && app.active == ids[1], ["tabs": app.tabs.count])
        }
    }
    // Parity: the tab bar is up with one tab, and the cards are Memory, Nodes and Topology.
    do {
        app.closeAllTabsForTest()
        let bare = app.showsTabBar
        app.select(model.id)
        check("tab-bar-shows-with-one-tab", !bare && app.tabs.count == 1 && app.showsTabBar, [:])
        check("summary-cards-are-memory-nodes-topology", model.record.cards.map(\.title) == ["Memory", "Nodes", "Topology"], [:])
    }
    // A start that fails stays "Stopped unexpectedly" with its reason, through later refreshes.
    do {
        let victim = app.models.last!
        victim.openForTest()   // opened, so a refresh runs the real branches
        victim.failStartForTest("boom: no firmware")
        victim.refresh()
        _ = waitFor(10) { !victim.isRefreshing }
        spin(0.5)
        check("start-error-is-not-wiped-to-stopped", victim.state == .stoppedUnexpectedly && (victim.failure ?? "").contains("boom"),
              ["state": victim.state.pill, "failure": victim.failure ?? "none"])
    }
    app.select(model.id)
    check("commands-enabled-at-rest", app.isEnabled(.start) && !app.isEnabled(.powerOff) && !app.isEnabled(.reset)
          && !app.isEnabled(.forceQuit), [:])
    check("window-never-shown", NSApp.windows.allSatisfy { !$0.isVisible }, [:])

    // 3. The SwiftUI views, rendered with no window.
    let hero = snapshot(Hero(model: model).environmentObject(app), size: CGSize(width: 760, height: 560), to: "\(out)/ui-hero.png")
    check("ui-hero-renders", hero.ok && hero.colours > 20, ["distinct_colours_sampled": hero.colours])
    // An O2's summary pane in dark and light, at 2x, for looking at the tint fade by eye.
    if let tint = ProcessInfo.processInfo.environment["ORIGAMI_TINT_PNG"] {
        let o2 = MachineModel(summary: OrigamiMachineSummary(key: "/tmp/o2-tint", name: "o2-tint", product: "o2", productLabel: "O2", topology: "o2", topologyName: "O2", hasDisplay: true, nodes: 1,
                                                             memoryPerNodeMib: 512, state: .notRunning, problem: nil),
                              host: host, defaultPointer: .follow)
        let size = CGSize(width: 800, height: 600)
        let dark = snapshotRetina(Hero(model: o2).environmentObject(app), size: size, dark: true, to: tint)
        let light = snapshotRetina(Hero(model: o2).environmentObject(app), size: size, dark: false, to: tint.replacingOccurrences(of: ".png", with: "-light.png"))
        check("tint-pngs-written", dark && light, ["dark": tint])
    }
    let sidebar = snapshot(LibrarySidebar().environmentObject(app), size: CGSize(width: 260, height: 420), to: "\(out)/ui-sidebar.png")
    check("ui-sidebar-renders", sidebar.ok && sidebar.colours > 5, ["distinct_colours_sampled": sidebar.colours])
    let startingUI = snapshot(StartingCard(model: model).environmentObject(app), size: CGSize(width: 760, height: 560), to: "\(out)/ui-starting.png")
    check("ui-starting-renders", startingUI.ok && startingUI.colours > 20, ["distinct_colours_sampled": startingUI.colours])
    let whole = snapshot(MainView().environmentObject(app), size: CGSize(width: 1000, height: 640), to: "\(out)/ui-main-stopped.png")
    check("ui-main-renders", whole.ok && whole.colours > 20, ["distinct_colours_sampled": whole.colours])

    // 3b. The cube: attach to a layer in an unshown window, draw, read back, restyle, cycle.
    // The window is never on screen, so the core draws offscreen: this tests the cube
    // and our hosting, not a display refresh.
    do {
        let cubeWindow = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 108, height: 108),
                                  styleMask: [.titled], backing: .buffered, defer: false)
        cubeWindow.isReleasedWhenClosed = false
        let cube = CubeNSView(frame: NSRect(x: 0, y: 0, width: 108, height: 108))
        cubeWindow.contentView = cube
        cube.countsOffscreen = true
        cube.configure(look: .starting(chassis: 0x2e354e), dark: true, machine: nil)
        let first = cube.present()
        func colours(_ f: (width: Int, height: Int, bgra: [UInt8])?) -> Int {
            guard let f else { return 0 }
            var seen = Set<UInt32>()
            for i in stride(from: 0, to: f.bgra.count, by: 4) {
                seen.insert(UInt32(f.bgra[i]) | UInt32(f.bgra[i + 1]) << 8 | UInt32(f.bgra[i + 2]) << 16)
            }
            return seen.count
        }
        let studio = cube.readback()
        // With Reduce Motion on, as it is on some CI machines, the cube draws its still pose.
        let reduceMotion = NSWorkspace.shared.accessibilityDisplayShouldReduceMotion
        check("cube-starting-draws", cube.cube != nil && studio != nil && colours(studio) > 8 && first.moving == !reduceMotion,
              ["error": cube.lastError, "size": studio.map { "\($0.width)x\($0.height)" } ?? "none",
               "colours": colours(studio), "spinning": first.moving, "reduce_motion": reduceMotion])
        cube.configure(look: .empty, dark: true, machine: nil)
        let emptyView = cube.present()
        let emptyPic = cube.readback()
        // No backdrop: the view's corner is either clear or exactly the window's colour, so no square shows.
        let win = CubeNSView.windowColour(dark: true)
        let px = emptyPic.map { Array($0.bgra[0..<4]) } ?? [1, 2, 3, 255]
        let clear = px[3] == 0
        let window = abs(Int(px[2]) - Int((win >> 16) & 255)) <= 1 && abs(Int(px[1]) - Int((win >> 8) & 255)) <= 1 && abs(Int(px[0]) - Int(win & 255)) <= 1
        check("empty-library-cube-is-studio-turning-and-has-no-square", emptyView.moving == !reduceMotion && emptyPic != nil && colours(emptyPic) > 8 && (clear || window),
              ["turning": emptyView.moving, "reduce_motion": reduceMotion, "corner_bgra": px, "clear": clear, "window_colour": window, "opaque_layer": cube.metalLayer.isOpaque])
        cube.configure(look: .about, dark: true, machine: nil)
        let dusk = cube.present()
        let duskPic = cube.readback()
        cube.configure(look: .about, dark: false, machine: nil)
        _ = cube.present()
        let light = cube.readback()
        check("cube-about-scene-follows-time-and-appearance", !dusk.moving && duskPic != nil && light != nil && duskPic?.bgra != studio?.bgra,
              ["at_rest_in_about": !dusk.moving, "noon_scene": origami_cube_scene_for_time(12, 0), "evening_scene": origami_cube_scene_for_time(20, 0)])
        // Stepping: each step moves to a different scene, and the loop comes back round.
        cube.configure(look: .about, dark: true, machine: nil)
        _ = cube.present()
        let firstScene = cube.currentScene
        var seen = [firstScene]
        for _ in 0..<Int(origami_cube_scene_count()) { cube.stepScene(); seen.append(cube.currentScene) }
        check("about-scene-steps-and-comes-back-round", firstScene >= 0 && seen[1] != firstScene && Set(seen).count > 1 && seen.last == firstScene, ["scenes": seen])
        let count = origami_cube_scene_count()
        let credits = (0..<count).map { sceneText($0, Int32(ORIGAMI_CUBE_SCENE_CREDIT)) }
        let titles = (0..<count).map { sceneText($0, Int32(ORIGAMI_CUBE_SCENE_TITLE)) }
        check("cube-credits-from-manifest", count > 0 && Set(zip(titles, credits).map { "\($0)|\($1)" }).count == Int(count) && titles.allSatisfy { !$0.isEmpty }
              && credits.allSatisfy { $0.contains("CC") && $0.contains("Wikimedia Commons") && $0.contains(", 20") },
              ["scenes": count, "credits": credits])
        // Machine art, as the app does: the colour comes from the manifest, and a build
        // without stills passes with the glyph fallback.
        if let id = Art.id(topology: "octane2", product: "octane2") {
            let url = Bundle.main.resourceURL!.appendingPathComponent("stills/manifest.json")
            let json = (try? JSONSerialization.jsonObject(with: Data(contentsOf: url))) as? [String: Any]
            let hex = ((json?["stills"] as? [String: Any])?[id] as? [String: Any])?["colour"] as? String
            check("art-resolves-and-loads", hex != nil && Art.colour(id) == UInt32(hex!.dropFirst(), radix: 16)
                  && Art.image(id, "34-dark", points: NSSize(width: 34, height: 34)) != nil
                  && Art.image(id, "hero", points: NSSize(width: 240, height: 180)) != nil, ["art": id, "colour": hex ?? "none"])
        } else {
            check("art-resolves-and-loads", true, ["note": "no stills in this build: glyph fallback"])
        }
        check("granite-tile-loads", Granite.tile(Granite.neutral, factor: 2) != nil, [:])

        // CPU while turning: driven at 60 Hz by a timer, as the display link would, for 10 s.
        cube.configure(look: .starting(chassis: 0x2e354e), dark: true, machine: nil)
        // The same loop with no cube drawing, to subtract the test's own timer cost.
        let b0 = processCPU(), bt = Date()
        while Date().timeIntervalSince(bt) < 3 { spin(1.0 / 60) }
        let b1 = processCPU()
        let baseline = ((b1.user - b0.user) + (b1.sys - b0.sys)) / Date().timeIntervalSince(bt) * 100
        let p0 = processCPU(), s0 = cube.stats(), n0 = cube.presents
        let t0 = Date()
        while Date().timeIntervalSince(t0) < 10 { cube.present(); spin(1.0 / 60) }
        let p1 = processCPU(), s1 = cube.stats()
        let cpu = ((p1.user - p0.user) + (p1.sys - p0.sys)) / Date().timeIntervalSince(t0) * 100
        let n = cube.presents - n0
        check("perf-cube-turning", n > 0 && (!timing || cpu - baseline < 1.0), [
            "process_cpu_percent_of_one_core": cpu, "timer_loop_baseline_percent": baseline, "net_percent": cpu - baseline, "presented": n, "budget_percent": 1.0, "budget_enforced": timing,
            "draw_us_per_frame": Double((s1?.draw_ns ?? 0) - (s0?.draw_ns ?? 0)) / 1000 / Double(max(n, 1)),
            "present_us_per_frame": Double((s1?.present_ns ?? 0) - (s0?.present_ns ?? 0)) / 1000 / Double(max(n, 1)),
            "note": "unshown window: the core drew offscreen, so the display path is not included"])
        cube.configure(look: .about, dark: true, machine: nil)
        // The cube settles after the spin; at rest the display link must pause.
        let settled = waitFor(15) { cube.present(); return cube.linkPaused }
        let q0 = cube.presents, k0 = cube.ticks, r0 = processCPU(); spin(5); let r1 = processCPU()
        check("perf-cube-at-rest", settled && cube.presents == q0 && cube.ticks == k0 && cube.linkPaused,
              ["presents_at_rest": cube.presents - q0, "link_ticks_at_rest": cube.ticks - k0, "link_paused": cube.linkPaused,
              "process_cpu_ms": ((r1.user - r0.user) + (r1.sys - r0.sys)) * 1000])
        cube.detach()
        check("cube-detach-releases-the-core-cube", cube.cube == nil && CubeNSView.attachedCount == 0, ["attached": CubeNSView.attachedCount])
    }

    // A cube hosted the way SwiftUI does (About) detaches when its view goes away.
    do {
        let w = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 200, height: 200), styleMask: [.titled], backing: .buffered, defer: false)
        w.isReleasedWhenClosed = false
        var sharp = false, sizes = ""
        autoreleasepool {
            let host = NSHostingView(rootView: CubeHost(look: .about).frame(width: 200, height: 200))
            w.contentView = host
            _ = waitFor(10) { CubeNSView.attachedCount == 1 }
            func find(_ v: NSView) -> CubeNSView? { (v as? CubeNSView) ?? v.subviews.lazy.compactMap(find).first }
            if let v = find(host) {
                host.layoutSubtreeIfNeeded()
                _ = waitFor(5) { v.metalLayer.drawableSize.width > 0 }
                let s = w.backingScaleFactor
                sharp = v.metalLayer.contentsScale == s && v.metalLayer.drawableSize == CGSize(width: v.bounds.width * s, height: v.bounds.height * s)
                sizes = "drawable \(v.metalLayer.drawableSize) for bounds \(v.bounds.size) at scale \(v.metalLayer.contentsScale), window \(s)"
            }
            w.contentView = NSView()
        }
        check("about-cube-drawable-is-at-the-backing-scale", sharp, ["sizes": sizes])
        check("about-cube-detaches-when-its-view-goes", waitFor(10) { CubeNSView.attachedCount == 0 }, ["attached": CubeNSView.attachedCount])
    }

    // New Machine: default model, name rules, an error that keeps the sheet, and open on success.
    do {
        let nm = NewMachineModel(app: app)
        nm.load()
        check("new-machine-catalogue-loads", waitFor(30) { !nm.loading && !nm.offerings.isEmpty },
              ["offerings": nm.offerings.map(\.id), "selected": nm.offerings.indices.contains(nm.selected) ? nm.offerings[nm.selected].id : "none"])
        let expectedDefault = nm.offerings.firstIndex { $0.id == "octane2-vpro" } ?? 0
        check("new-machine-defaults-to-octane2-vpro-when-offered", nm.selected == expectedDefault, ["selected": nm.selected])
        nm.name = ""; let emptyOff = !nm.canCreate
        nm.name = "  \t "; let blankOff = !nm.canCreate
        nm.name = " gamma "; let namedOn = nm.canCreate
        check("new-machine-create-needs-a-trimmed-name", emptyOff && blankOff && namedOn, [:])
        // Engines: always an explicit one, the catalogue's own; a change keeps the model when the new engine offers it.
        let engines = nm.engines
        let explicit = nm.engine != nil && engines.contains(nm.engine ?? "")
        var kept = true, filtered = true
        for e in engines {
            let before = nm.offerings.indices.contains(nm.selected) ? nm.offerings[nm.selected].id : ""
            let offered = nm.offerings.contains { $0.engine == e && $0.id == before }
            nm.engine = e
            filtered = filtered && !nm.visible.isEmpty && nm.visible.allSatisfy { nm.offerings[$0].engine == e }
                && nm.offerings.indices.contains(nm.selected) && nm.offerings[nm.selected].engine == e
            if offered { kept = kept && nm.offerings[nm.selected].id == before }
        }
        check("new-machine-engine-is-explicit-and-keeps-the-model", !engines.isEmpty && explicit && filtered && kept && nm.canPickEngine == (engines.count > 1),
              ["engines": engines, "offerings": nm.offerings.count, "engine": nm.engine ?? "none"])
        if fake {
            nm.bootProm = library + "/prom.bin"
            var result: Bool?
            nm.create { ok in result = ok }
            check("new-machine-disabled-while-creating", nm.creating && !nm.canCreate, [:])
            check("new-machine-created-and-opened", waitFor(30) { result != nil } && result == true && app.current?.name == "gamma" && app.tabs.contains(app.current?.id ?? ""),
                  ["active": app.current?.name ?? "none"])
            // The real sheet flow on the fake engine has no firmware to offer, so it must explain and stay.
            let bare = NewMachineModel(app: app); bare.load(); _ = waitFor(30) { !bare.loading }
            bare.name = "needs-firmware"
            var bareResult: Bool?
            bare.create { ok in bareResult = ok }
            _ = waitFor(30) { bareResult != nil }
            check("new-machine-without-firmware-explains-and-stays", bareResult == false && !(bare.error ?? "").isEmpty && bare.canCreate,
                  ["error": bare.error ?? "none"])
            // The same name again is refused: the error shows and the sheet stays.
            let again = NewMachineModel(app: app); again.load(); _ = waitFor(30) { !again.loading }
            again.bootProm = library + "/prom.bin"; again.name = "gamma"
            var second: Bool?
            again.create { ok in second = ok }
            _ = waitFor(30) { second != nil }
            check("new-machine-error-stays-on-the-sheet", second == false && (again.error ?? "").count > 0 && !again.creating && again.canCreate,
                  ["error": again.error ?? "none"])
        }
        app.select(model.id)
    }
    // The machine's tab, hosted before the start the way the app has it, so the first picture arrives into a
    // live layout. Its display must fill the fit rect for that picture at once, not after a reset.
    let tabWindow = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 900, height: 700), styleMask: [.titled, .resizable],
                             backing: .buffered, defer: false)
    tabWindow.isReleasedWhenClosed = false
    // The fake engine's frames are for the copy test below, which a hosted display would take; QEMU only.
    let tabHost = NSHostingView(rootView: fake ? AnyView(EmptyView()) : AnyView(MachineTab(model: model).environmentObject(app)))
    tabWindow.contentView = tabHost
    func displayViews(_ v: NSView) -> [MachineView] { (v as? MachineView).map { [$0] } ?? v.subviews.flatMap(displayViews) }
    func checkTabFillsTheFitRect(_ name: String) {
        guard let picture = model.picture else { check(name, false, ["error": "no picture"]); return }
        let ok = waitFor(10) {
            tabHost.layoutSubtreeIfNeeded()
            guard let v = displayViews(tabHost).first else { return false }
            let avail = CGSize(width: max(80, tabHost.bounds.width - 60), height: max(80, tabHost.bounds.height - 60))
            let want = Scaling.viewSize(guest: CGSize(width: picture.width, height: picture.height), available: avail,
                                        backing: tabWindow.backingScaleFactor, mode: app.scaleMode)
            return abs(v.frame.width - want.width) < 1.5 && abs(v.frame.height - want.height) < 1.5
        }
        let got = displayViews(tabHost).first?.frame.size ?? .zero
        check(name, ok, ["view": "\(Int(got.width))x\(Int(got.height))", "host": "\(Int(tabHost.bounds.width))x\(Int(tabHost.bounds.height))",
                        "picture": "\(picture.width)x\(picture.height)", "mode": "\(app.scaleMode)"])
    }

    // 4. Start it the way the Start command does. Events drive every state change.
    var startError: Error?? = .none
    model.start { e in startError = .some(e) }
    check("state-starting", model.state == .starting && model.busy == "Starting…", ["state": model.state.pill])
    check("start-returns", waitFor(180) { startError != nil } && (startError ?? nil) == nil, ["error": "\(String(describing: startError))"])
    guard let session = model.session else { check("session", false, ["error": "no session after start"]); finish() }
    check("emulator-up-by-event", waitFor(60) { model.emulatorUp }, ["state": model.state.pill])

    if !fake {
        // The QEMU engine: what the FFI wires so far.
        _ = waitFor(40) { model.hasPicture || model.headless }
        let picture = model.hasPicture
        check("qemu-picture-reported", picture || model.headless, ["heads": session.machine.view().heads.count,
              "picture": "\(String(describing: model.picture))", "state": model.state.pill])
        _ = waitFor(10) { model.state == .running }
        if picture { checkTabFillsTheFitRect("tab-display-fills-the-fit-rect-on-the-first-picture") }
        else {
            // A machine with no display: its tab is its console, with output and input.
            func consoleViews(_ v: NSView) -> [ConsoleNSView] { (v as? ConsoleNSView).map { [$0] } ?? v.subviews.flatMap(consoleViews) }
            check("headless-machine-runs-with-its-console-in-the-tab", waitFor(30) { model.headless && model.state == .running && !consoleViews(tabHost).isEmpty },
                  ["state": model.state.pill, "headless": model.headless, "console_views": consoleViews(tabHost).count])
            let console = model.console()
            func screen() -> String { (0..<Int(console.terminal?.size().rows ?? 24)).map { r in ((try? console.terminal?.row(index: UInt16(r))) ?? []).map { $0.text.isEmpty ? " " : $0.text }.joined() }.joined(separator: "\n") }
            check("headless-console-has-output", waitFor(60) { !screen().trimmingCharacters(in: .whitespacesAndNewlines).isEmpty },
                  ["screen_head": String(screen().trimmingCharacters(in: .whitespacesAndNewlines).prefix(100))])
            console.send(.enter); console.send(.text(text: "1")); console.drainInput(); spin(0.3)
            check("headless-console-accepts-input", console.error == nil, ["error": console.error ?? "none"])
            // Backspace at the PROM: wait for the menu, open the command monitor, type, erase one, and read the line back.
            _ = waitFor(120) { screen().contains("Option?") }
            console.send(.text(text: "5")); console.send(.enter); console.drainInput()
            _ = waitFor(30) { screen().contains(">>") }
            console.send(.text(text: "xyz")); console.drainInput(); spin(1)
            let before = screen()
            console.send(.backspace); console.drainInput(); spin(2)
            let after = screen()
            func lineWith(_ t: String, _ pat: String) -> String { t.split(separator: "\n").last { $0.contains(pat) }.map(String.init) ?? "" }
            let beforeLine = lineWith(before, "xyz"), afterLine = lineWith(after, "xy")
            check("headless-backspace-erases-at-the-prom", before.contains("xyz") && !afterLine.contains("xyz") && after.contains("xy"),
                  ["before": beforeLine.trimmingCharacters(in: .whitespaces), "after": afterLine.trimmingCharacters(in: .whitespaces), "tail": String(after.suffix(300))])
            check("console-first-attach-has-the-real-size", console.firstAttach.map { $0 == (Int(console.terminal?.size().rows ?? 0), Int(console.terminal?.size().cols ?? 0)) } ?? false
                  && (console.firstAttach?.rows ?? 0) > 20,
                  ["first": "\(String(describing: console.firstAttach))", "now": "\(String(describing: console.terminal?.size()))"])
            if let png = ProcessInfo.processInfo.environment["ORIGAMI_CONSOLE_PNG"], let v = consoleViews(tabHost).first {
                spin(1)
                if let rep = v.bitmapImageRepForCachingDisplay(in: v.bounds) { v.cacheDisplay(in: v.bounds, to: rep)
                    try? rep.representation(using: .png, properties: [:])?.write(to: URL(fileURLWithPath: png)) }
            }
        }
        tabWindow.contentView = NSView()
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
                model.refresh()
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
        // The serial console on the real engine: input is accepted. What comes back depends on the PROM's console setting.
        do {
            let console = model.console()
            console.attach(rows: 24, cols: 80)
            console.send(.enter)
            console.drainInput(); spin(2)
            check("qemu-console-accepts-input", console.terminal != nil && console.error == nil, ["error": console.error ?? "none"])
            console.detach()
        }
        app.confirm = { _, _, _ in true }
        var stopped: Error?? = .none
        model.stop { e in stopped = .some(e) }
        check("qemu-power-off", waitFor(300) { stopped != nil } && (stopped ?? nil) == nil, ["state": model.state.pill])

        // The emulator ending by itself. Each scenario starts the machine in a fresh core, because the
        // core refuses to start a machine again after its emulator ended ("machine is already running").
        let runJSON = libraryURL.appendingPathComponent(model.name).appendingPathComponent("state/run.json")
        func record() -> [String: Any]? {
            (try? JSONSerialization.jsonObject(with: Data(contentsOf: runJSON))) as? [String: Any]
        }
        func freshlyStarted() -> (AppModel, MachineModel)? {
            let host = AppModel(root: libraryURL)
            guard let m = host.model(named: model.name) else { return nil }
            m.start()
            return waitFor(180) { m.state == .running } ? (host, m) : nil
        }
        // A crash, seen by the app that started the machine and by one that found it running.
        if let (host, m) = freshlyStarted(), let pid = record()?["qemu_pid"] as? Int {
            let observer = AppModel(root: libraryURL)
            let seen = observer.model(named: model.name)
            var changes = 0
            let sink = observer.objectWillChange.sink { _ in changes += 1 }
            spin(6)
            // The core lists a machine another core runs as left running; listing it must not churn the UI.
            check("left-running-is-listed-without-churn", changes == 0 && seen?.state == .leftRunning,
                  ["state": seen?.state.pill ?? "none", "objectWillChange_in_6s": changes])
            sink.cancel()
            kill(pid_t(pid), SIGKILL)
            check("qemu-crash-shows-stopped-unexpectedly", waitFor(20) { m.state == .stoppedUnexpectedly && !(m.failure ?? "").isEmpty },
                  ["state": m.state.pill, "reason": m.failure ?? "none"])
            check("left-running-that-dies-is-not-clean", waitFor(20) { seen?.state == .stoppedUnexpectedly },
                  ["state": seen?.state.pill ?? "none", "reason": seen?.failure ?? "none"])
            _ = host
        } else { check("qemu-crash-shows-stopped-unexpectedly", false, ["error": "could not start or read run.json"]) }
        // A QMP quit nobody on the host asked for is the guest powering itself off, which the core calls a clean stop.
        if let (host, m) = freshlyStarted(), let port = record()?["qmp_port"] as? Int {
            let script = """
            import json, socket, sys
            s = socket.create_connection(("127.0.0.1", int(sys.argv[1])), timeout=20); f = s.makefile("rw"); f.readline()
            for c in ("qmp_capabilities", "quit"):
                f.write(json.dumps({"execute": c}) + "\\n"); f.flush(); f.readline()
            """
            let p = Process()
            p.executableURL = URL(fileURLWithPath: "/usr/bin/python3")
            p.arguments = ["-c", script, "\(port)"]
            try? p.run(); p.waitUntilExit()
            check("guest-power-off-is-a-clean-stop", waitFor(20) { m.state == .stopped && m.failure == nil },
                  ["state": m.state.pill, "failure": m.failure ?? "none", "core_state": "\(String(describing: m.coreView?.state))",
                   "exit_reason": "\(String(describing: m.coreView?.exitReason))", "exit_detail": m.coreView?.exitDetail ?? "none", "busy": m.busy ?? "none"])
            _ = host
        } else { check("guest-power-off-is-a-clean-stop", false, ["error": "could not start or read run.json"]) }
        // The real sheet path on the real engine: the default model and a name, no firmware injected.
        do {
            let nm = NewMachineModel(app: app)
            nm.load()
            _ = waitFor(60) { !nm.loading }
            nm.name = "ui-new-machine"
            var result: Bool?
            nm.create { ok in result = ok }
            _ = waitFor(120) { result != nil }
            let opened = result == true && app.current?.name == "ui-new-machine"
            check("new-machine-real-engine-creates-or-explains", opened || (result == false && !(nm.error ?? "").isEmpty && !nm.creating),
                  ["created_and_opened": opened, "error": nm.error ?? "none",
                   "default": nm.offerings.indices.contains(nm.selected) ? nm.offerings[nm.selected].id : "none"])
        }
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
    // A console view takes the size it has been given, not a placeholder, when it first attaches.
    do {
        let own = ConsoleModel(machine: model)
        let w = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 700, height: 440), styleMask: [.titled, .resizable], backing: .buffered, defer: false)
        w.isReleasedWhenClosed = false
        let v = ConsoleNSView(console: own)
        w.contentView = v
        v.frame = NSRect(x: 0, y: 0, width: 700, height: 440)
        _ = waitFor(5) { own.terminal != nil }
        let want = (Int(440 / v.cell.height), Int(700 / v.cell.width))
        check("console-view-attaches-at-its-real-size", own.firstAttach.map { $0 == want } ?? false,
              ["first": "\(String(describing: own.firstAttach))", "want": "\(want)"])
        w.contentView = NSView()
        check("console-ends-with-its-last-view", waitFor(5) { own.terminal == nil }, [:])
    }
    // The console window: output the core holds reaches the terminal, only changed rows are reported, input goes back.
    do {
        let console = model.console()
        console.attach(rows: 24, cols: 80)
        func text(_ r: Int) -> String { ((try? console.terminal?.row(index: UInt16(r))) ?? []).map { $0.text.isEmpty ? " " : $0.text }.joined() }
        func screen() -> String { (0..<24).map(text).joined(separator: "\n") }
        _ = console.damage()
        do { try host.core.fakeSay(key: key, text: "PROM says hello\r\n") } catch { check("fake-say", false, ["error": "\(error)"]) }
        check("console-output-arrives", waitFor(10) { screen().contains("PROM says hello") }, ["screen_head": String(screen().prefix(80))])
        let dirty = console.damage()
        check("console-redraws-only-changed-rows", dirty != nil && !(dirty ?? []).isEmpty && (dirty ?? []).count < 24 && console.damage() == [],
              ["rows": "\(String(describing: dirty))"])
        console.send(.text(text: "h")); console.send(.enter); console.paste("hinv\r")
        console.drainInput(); spin(0.2)
        check("console-send-accepted", console.error == nil, ["error": console.error ?? "none"])
        check("console-command-enabled", app.isEnabled(.console), [:])
        console.detach()
    }
    // The CPU panel: the fake engine's two CPUs at 30/10 and 60/5, polled while shown and not once hidden.
    if fake {
        let load = CpuLoadModel()
        load.start(model)
        let got = waitFor(20) { (load.report?.cpus.count ?? 0) == 2 }
        let c = load.report?.cpus ?? []
        func near(_ a: Float, _ b: Float) -> Bool { abs(a - b) < 6 }
        check("cpu-panel-shows-the-fake-engines-two-cpus", got && near(c[0].userPct, 30) && near(c[0].sysPct, 10) && near(c[1].userPct, 60) && near(c[1].sysPct, 5),
              ["cpus": c.map { "cpu\($0.cpu) \($0.userPct)/\($0.sysPct)" }, "unavailable": load.report?.unavailable ?? "none"])
        load.stop()
        let after = load.samples
        spin(1.5)
        check("cpu-panel-hidden-polls-nothing", load.samples == after && load.report == nil, ["samples_after_stop": load.samples - after])
        check("cpu-placeholder-says-measuring-or-why", cpuLoadPlaceholder(nil) == "measuring…"
              && cpuLoadPlaceholder(OrigamiCpuLoadReport(source: .host, cpus: [], unavailable: "no threads")) == "no threads", [:])
    }

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

    // About does not reflect a machine, presenting or not: the scene stays as it was.
    do {
        let w = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 108, height: 108), styleMask: [.titled], backing: .buffered, defer: false)
        w.isReleasedWhenClosed = false
        let c = CubeNSView(frame: NSRect(x: 0, y: 0, width: 108, height: 108))
        w.contentView = c
        c.countsOffscreen = true
        c.configure(look: .about, dark: true, machine: nil)
        c.present()
        let scene = c.readback()
        let idle = MachineView(session: session)   // never drawn: not presenting
        c.configure(look: .about, dark: true, machine: idle)
        c.present()
        let still = c.readback()
        c.configure(look: .about, dark: true, machine: view)
        advance(); _ = waitFor(10) { view.presented > 1 }
        c.present()
        let screen = c.readback()
        check("cube-about-ignores-a-presenting-machine", view.isPresenting && !idle.isPresenting && scene?.bgra == still?.bgra
              && screen != nil && screen?.bgra == scene?.bgra, ["view_presenting": view.isPresenting, "error": c.lastError])
        idle.detach(); c.detach()
        // Making `idle` took the session's frame handler; give it back to `view`.
        session.setFrameHandler(owner: view) { [weak view] in view?.redraw() }
    }

    // The drawable is the guest's size and the layer fills the view, so the compositor scales it:
    // nearest for a whole-number upscale, linear otherwise.
    do {
        let guest = CGSize(width: model.picture?.width ?? 0, height: model.picture?.height ?? 0)
        let fills = view.metalLayer.frame.size == view.bounds.size
        let whole = view.metalLayer.magnificationFilter == .nearest   // 640 pt x 2 over 64 px: 20x
        view.setFrameSize(NSSize(width: 500, height: 400))
        let smooth = view.metalLayer.magnificationFilter == .linear && view.metalLayer.frame.size == view.bounds.size
        check("layer-fills-the-view-at-guest-resolution", view.metalLayer.drawableSize == guest && fills && whole && smooth,
              ["drawable": "\(view.metalLayer.drawableSize)", "guest": "\(guest)", "layer": "\(view.metalLayer.frame.size)", "view": "\(view.bounds.size)",
               "whole_filter_nearest": whole, "fractional_filter_linear": smooth])
        view.setFrameSize(NSSize(width: 640, height: 512))
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

    // A running machine in a background tab must not make the UI re-render (a storm of
    // redraws would drop sidebar clicks) or touch the host pointer.
    do {
        var changes = 0
        let sink = app.objectWillChange.sink { _ in changes += 1 }
        let pointer0 = NSEvent.mouseLocation
        if let other = app.models.first(where: { $0.id != model.id }) { app.select(other.id) }
        for i in 0..<100 {
            advance()
            do { try host.core.fakeSay(key: key, text: "line \(i)\n") } catch { check("fake-say", false, ["error": "\(error)"]); break }
            spin(0.01)
        }
        spin(0.5)
        // The pointer is the person's real one, so its position is reported, not asserted.
        check("background-machine-causes-no-ui-churn", changes <= 2,
              ["objectWillChange": changes, "events_sent": 200, "host_pointer_moved_by_the_user": NSEvent.mouseLocation != pointer0])
        sink.cancel()
        app.select(model.id)
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

    // 7b. Pause and Resume.
    do {
        var done: Error?? = .none
        model.pause { e in done = .some(e) }
        check("pause", waitFor(30) { done != nil } && model.state == .paused && app.isEnabled(.resume) && !app.isEnabled(.pause),
              ["state": model.state.pill])
        // Save Screenshot writes a PNG named for the machine and time into the folder, with no panel,
        // and says where.
        app.screenshotFolder = URL(fileURLWithPath: out)
        app.invoke(.screenshot)
        check("screenshot-saved-and-announced", waitFor(30) { app.notice?.reveal != nil }, ["notice": app.notice?.text ?? "none"])
        if let url = app.notice?.reveal, let rep = NSBitmapImageRep(data: (try? Data(contentsOf: url)) ?? Data()) {
            check("screenshot-is-a-png-of-the-head", url.pathExtension == "png" && url.lastPathComponent.hasPrefix("\(model.name) ")
                  && rep.pixelsWide == model.picture?.width && rep.pixelsHigh == model.picture?.height,
                  ["file": url.lastPathComponent, "size": "\(rep.pixelsWide)x\(rep.pixelsHigh)"])
        } else { check("screenshot-is-a-png-of-the-head", false, ["notice": app.notice?.text ?? "none"]) }
        done = .none
        model.resume { e in done = .some(e) }
        check("resume", waitFor(30) { done != nil } && model.state == .running, ["state": model.state.pill])
    }

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

    // Quit: no prompt when none runs, one naming the live ones otherwise; a stale entry for a
    // stopped machine is not counted.
    do {
        // A machine nothing has opened, so a stale state on it can only be corrected by the listing.
        do {
            let op = try host.core.createMachine(request: OrigamiCreateRequest(
                name: "delta", offering: "fake.1", memoryPerNodeMib: nil, graphics: nil, bootProm: library + "/prom.bin", ioProm: nil, engine: nil))
            check("create-quit-test-machine", op.wait(timeoutMs: 10_000) == .succeeded, [:])
            app.rescanNow()
        } catch { check("create-quit-test-machine", false, ["error": "\(error)"]) }
        let beta = app.model(named: "delta")
        func plan() -> AppModel.QuitPlan {
            var r: AppModel.QuitPlan?
            app.quitPlan { r = $0 }
            _ = waitFor(30) { r != nil }
            return r ?? .ask(["no answer"])
        }
        beta?.injectStaleState(.running)            // stale: the core says it is stopped
        let one = plan()                            // one running (alpha) plus a stale beta
        check("quit-one-running-ignores-a-stale-entry", one == .ask([model.name]) && beta?.state == .stopped,
              ["plan": "\(one)", "stale_state_after": beta?.state.pill ?? "none"])
        if let beta {
            beta.start()
            _ = waitFor(60) { beta.state == .running }
            let order = [model.name, beta.name].sorted { $0.localizedStandardCompare($1) == .orderedAscending }
            let two = plan()
            check("quit-two-running-one-prompt-in-library-order", two == .ask(order), ["plan": "\(two)", "expected": order])
        }
    }

    var stopped = false
    var failedNames: [String] = []
    app.stopAll { failed in failedNames = failed.map(\.name); stopped = true }
    check("stop-all", waitFor(120) { stopped } && failedNames.isEmpty && model.state == .stopped,
          ["state": model.state.pill, "failed": failedNames])
    var final: AppModel.QuitPlan?
    app.quitPlan { final = $0 }
    _ = waitFor(30) { final != nil }
    check("quit-none-running-quits-silently", final == .quitNow, ["plan": "\(String(describing: final))"])
    #else
    check("fake-build", false, ["error": "the core reports the fake engine but this app was built without -D FAKE"])
    #endif
    finish()
}
