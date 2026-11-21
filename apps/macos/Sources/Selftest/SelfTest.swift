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
    // Picture sizing: the picture fills its area at any scale; the first window is a whole number of device pixels.
    do {
        let g = CGSize(width: 1280, height: 1024)
        func size(_ w: CGFloat, _ h: CGFloat) -> CGSize { Scaling.viewSize(guest: g, available: CGSize(width: w, height: h)) }
        // Pointer mapping follows the picture's rectangle at any scale.
        func mapped(_ w: CGFloat, _ h: CGFloat) -> Bool {
            let v = size(w, h)
            let a = MachineView.guestPoint(CGPoint(x: v.width / 2, y: v.height / 2), view: v, head: g)
            let b = MachineView.guestPoint(CGPoint(x: 0, y: 0), view: v, head: g)
            let c = MachineView.guestPoint(CGPoint(x: v.width - 0.01, y: v.height - 0.01), view: v, head: g)
            return a == (640, 512) && b == (0, 0) && c == (1279, 1023)
        }
        // The picture's origin lands on a whole device pixel at 1x and 2x, with fractional or odd sizes.
        check("origin-on-a-device-pixel", [(CGFloat(1), CGSize(width: 1001, height: 700)), (2, CGSize(width: 1000.5, height: 733))].allSatisfy { b, c in
            let v = size(c.width - 60, c.height - 60)
            let d = Scaling.snap(container: c, size: v, backing: b)
            let x = ((c.width - v.width) / 2 + d.width) * b, y = ((c.height - v.height) / 2 + d.height) * b
            return abs(x - x.rounded()) < 1e-6 && abs(y - y.rounded()) < 1e-6
        }, [:])
        check("pointer-mapping-follows-the-scale", mapped(1500, 1100) && mapped(2600, 2100) && mapped(1000, 700) && mapped(500, 400), [:])
        let first = { (w: CGFloat, h: CGFloat) in Scaling.firstScale(guest: g, room: CGSize(width: w, height: h)) }
        check("picture-fills-and-first-scale-is-one-point-or-the-largest-fit", size(1000, 700) == CGSize(width: 875, height: 700)
              && size(2600, 1100) == CGSize(width: 1375, height: 1100)
              && first(1452, 852) == 852.0 / 1024   // a 14-inch laptop: the largest fit
              && first(2500, 1348) == 1             // a 5K display: one point
              && first(2940, 2300) == 1,            // never more than one point
              ["laptop": first(1452, 852), "5k": first(2500, 1348)])
    }

    // A second machine of the same product, to check selection keys rows by path.
    if fake {
        do {
            let op = try host.core.createMachine(request: OrigamiCreateRequest(
                name: "beta", offering: "fake.1", memoryPerNodeMib: nil, graphics: nil, bootProm: library + "/prom.bin", ioProm: nil, engine: nil, systemDisk: nil, identity: nil))
            check("create-second-machine", op.wait(timeoutMs: 10_000) == .succeeded, [:])
            app.rescanNow()
        } catch { check("create-second-machine", false, ["error": "\(error)"]) }
    }
    #if FAKE
    // Choosing another library folder takes effect at once, with no restart.
    if fake {
        let other = out + "/other-library"
        try? FileManager.default.createDirectory(atPath: other, withIntermediateDirectories: true)
        FileManager.default.createFile(atPath: other + "/prom.bin", contents: Data(count: 16))
        let before = app.models.map(\.name)
        app.useLibrary(URL(fileURLWithPath: other))
        check("library-switch-empties-the-list", waitFor(10) { app.models.isEmpty }, ["was": before])
        check("library-switch-makes-in-the-new-folder", makeFakeMachine(host, library: other, name: "gamma") == nil, [:])
        check("library-switch-lists-the-new-folder", waitFor(10) { app.models.map(\.name) == ["gamma"] }, [:])
        app.useLibrary(libraryURL)
        check("library-switch-back", waitFor(10) { app.models.map(\.name) == before }, ["now": app.models.map(\.name)])
    }
    #endif
    // Settings: read through the core off the main thread, edit while stopped, refused while running. A machine of its
    // own, because editing the file must not change the one the rest of the run starts.
    if fake {
        do {
            let op = try host.core.createMachine(request: OrigamiCreateRequest(
                name: "settingsvic", offering: "fake.1", memoryPerNodeMib: nil, graphics: nil, bootProm: library + "/prom.bin", ioProm: nil, engine: nil, systemDisk: nil, identity: nil))
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
            // The Startup pane's picker calls setConsole: a second line is stored, the default line clears the choice.
            if let s = sm.settings, let other = s.consolePorts.first(where: { !$0.isDefault }), let line = s.consolePorts.first(where: { $0.isDefault }) {
                check("console-line-listed-with-the-default-marked", s.consolePort == line.name && line.label.hasSuffix("(default)"),
                      ["in_use": s.consolePort, "lines": s.consolePorts.map { $0.name }])
                sm.setConsole(other.name)
                let chose = waitFor(30) { sm.settings?.consolePort == other.name && !sm.working }
                check("console-line-chosen", chose, ["wanted": other.name, "error": sm.error ?? "none"])
                sm.setConsole(line.name)
                let reset = waitFor(30) { sm.settings?.consolePort == line.name && !sm.working }
                check("console-line-default-puts-it-back", reset, ["now": sm.settings?.consolePort ?? "", "error": sm.error ?? "none"])
            } else { check("console-line-chosen", false, ["error": "the machine lists no second serial line"]) }
            // Network: Bridged opens its fields before anything is saved, applies with its device, and a moved
            // subnet returns the lease advice with a command to copy.
            if let bridged = sm.networkChoices.first(where: { $0.label == "Bridged" }) {
                sm.chooseNetwork(bridged.mode)
                check("network-bridged-opens-its-fields-first", sm.bridging && sm.settings?.network.mode == "user", ["mode": sm.settings?.network.mode ?? ""])
                sm.bridgeDevice = "tap0"
                sm.applyBridged()
                let bridgedOn = waitFor(30) { ["tap", "vmnet"].contains(sm.settings?.network.mode ?? "") && !sm.working }
                let device = sm.settings?.network.device
                check("network-bridged-is-applied-with-its-device", bridgedOn && device == "tap0", ["device": device ?? "", "error": sm.error ?? "none"])
                check("network-ethernet-address-is-shown-with-where-it-came-from",
                      sm.settings?.network.mac?.hasPrefix("08:00:69:") == true && sm.settings?.network.macNote.contains("ID") == true, ["mac": sm.settings?.network.mac ?? ""])
                sm.chooseNetwork(.user)
                _ = waitFor(30) { sm.settings?.network.mode == "user" && !sm.working }
                sm.subnetText = "10.42.0.0/24"; sm.applySubnet()
                let moved = waitFor(30) { sm.settings?.network.subnet == "10.42.0.0/24" && !sm.working }
                check("network-subnet-moves-and-returns-advice-to-copy", moved && sm.notes.contains { $0.copy?.hasPrefix("rm /var/adm") == true }, ["error": sm.error ?? "none"])
                sm.subnetText = "8.8.8.0/24"; sm.applySubnet()
                _ = waitFor(30) { !sm.working && sm.error != nil }
                check("network-public-subnet-is-refused", sm.error != nil && sm.settings?.network.subnet == "10.42.0.0/24", ["error": sm.error ?? "none"])
            } else { check("network-bridged-opens-its-fields-first", false, ["error": "no Bridged choice"]) }
            // Shared folders: the sheet's draft is named from the folder, a bad name is refused, and the row carries
            // the core's mount commands for the machine's network.
            let folder = URL(fileURLWithPath: out)
            sm.chooseShareFolder(folder)
            check("share-name-is-suggested-from-the-folder", sm.draft?.name == origamiSuggestShareName(dir: folder.path) && sm.draftProblem == nil, ["name": sm.draft?.name ?? ""])
            sm.draft?.name = "a/b"
            check("share-bad-name-is-refused-with-the-core-sentence", sm.draftProblem != nil, ["problem": sm.draftProblem ?? "none"])
            sm.draft?.name = "src"; sm.addDraft()
            let shared = waitFor(30) { sm.settings?.shares.first?.name == "src" && !sm.working }
            let ip = (sm.settings?.network.subnet ?? "").replacingOccurrences(of: ".0/24", with: ".4")
            check("share-row-has-the-mount-commands", shared && sm.settings?.shares.first?.mountLines == ["mkdir -p /mnt/src", "mount \(ip):/src /mnt/src"],
                  ["lines": sm.settings?.shares.first?.mountLines ?? [], "error": sm.error ?? "none"])
            sm.apply(.removeShare(name: "src"))
            check("share-removed-keeps-the-host-folder", waitFor(30) { sm.settings?.shares.isEmpty == true && !sm.working } && FileManager.default.fileExists(atPath: folder.path), [:])
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

    do {
        let old = "origami.selftest.old", new = "origami.selftest.new"
        UserDefaults.standard.removePersistentDomain(forName: old)
        UserDefaults.standard.removePersistentDomain(forName: new)
        defer { UserDefaults.standard.removePersistentDomain(forName: old); UserDefaults.standard.removePersistentDomain(forName: new) }
        let oldSuite = UserDefaults(suiteName: old)!, newSuite = UserDefaults(suiteName: new)!
        oldSuite.set("/old/library", forKey: "libraryRoot"); oldSuite.set(true, forKey: "optionAsMeta")
        newSuite.set("/new/library", forKey: "libraryRoot")
        let first = PrefsMigration.run(from: old, into: newSuite)
        oldSuite.set("later", forKey: "addedAfter")
        let again = PrefsMigration.run(from: old, into: newSuite)
        check("prefs-migrate-once-and-keep-existing", first == ["optionAsMeta"] && again.isEmpty
              && newSuite.string(forKey: "libraryRoot") == "/new/library" && newSuite.bool(forKey: "optionAsMeta")
              && newSuite.object(forKey: "addedAfter") == nil && oldSuite.string(forKey: "libraryRoot") == "/old/library",
              ["copied": first, "second_run": again])
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
        let o2 = MachineModel(summary: OrigamiMachineSummary(key: "/tmp/o2-tint", name: "o2-tint", product: "o2", productLabel: "O2", topology: "o2", topologyName: "O2", hasDisplay: true, runsIrix: true, nodes: 1,
                                                             memoryPerNodeMib: 512, state: .notRunning, problem: nil, needsUpgrade: false, systemDisk: nil),
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
        if let studio { writePNG(studio, to: "\(out)/starting-readback.png") }
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
        // About's backdrop is a smooth gradient from the scene, not a tile: a strip down the left edge steps through many
        // levels where a two-tone tile has two, and the top is not the bottom.
        cube.configure(look: .about, dark: true, machine: nil)
        _ = cube.present()
        if let f = cube.readback() {
            var seen = Set<UInt32>()
            for y in 0..<f.height { let o = y * f.width * 4; seen.insert(UInt32(f.bgra[o]) | UInt32(f.bgra[o + 1]) << 8 | UInt32(f.bgra[o + 2]) << 16) }
            let top = Array(f.bgra[0..<4]), bottom = Array(f.bgra[((f.height - 1) * f.width * 4)..<((f.height - 1) * f.width * 4 + 4)])
            check("about-background-is-a-gradient-not-granite", seen.count >= 8 && top != bottom,
                  ["levels_down_the_edge": seen.count, "top_bgra": top, "bottom_bgra": bottom])
            writePNG(f, to: "\(out)/about-readback-dark.png")
        } else { check("about-background-is-a-gradient-not-granite", false, ["error": "no readback"]) }
        cube.configure(look: .about, dark: false, machine: nil)
        _ = cube.present()
        if let f = cube.readback() { writePNG(f, to: "\(out)/about-readback-light.png") }
        cube.configure(look: .about, dark: true, machine: nil)
        _ = cube.present()
        // A drag turns the cube; once released it comes to rest at the logo pose, the same pixels as before.
        // The last scene step's crossfade has to be over first, or the two frames differ by it.
        _ = waitFor(5) { !cube.present().moving }
        let rest = cube.readback()
        let (vw, vh) = (cube.bounds.width, cube.bounds.height)
        cube.pointerForTest(UInt32(ORIGAMI_CUBE_POINTER_PRESS), vw * 0.3, vh * 0.5, ms: 0)
        cube.pointerForTest(UInt32(ORIGAMI_CUBE_POINTER_MOVE), vw * 0.6, vh * 0.55, ms: 16)
        let turned = cube.readback()
        cube.pointerForTest(UInt32(ORIGAMI_CUBE_POINTER_RELEASE), vw * 0.6, vh * 0.55, ms: 500)
        let returned = waitFor(10) { !cube.present().moving }
        let back = cube.readback()
        check("about-cube-turns-when-dragged-and-returns-to-rest",
              rest != nil && turned != nil && turned?.bgra != rest?.bgra && returned && back?.bgra == rest?.bgra,
              ["turned": turned?.bgra != rest?.bgra, "returned_to_rest": returned, "same_pixels_as_before": back?.bgra == rest?.bgra,
               "reduce_motion": NSWorkspace.shared.accessibilityDisplayShouldReduceMotion])
        // Edges: the silhouette has partial-coverage pixels from 4x multisampling or better, and a render at the view's size
        // is close to the same render at 4x filtered down.
        let edges = edgeQuality(side: 160, scene: origami_cube_scene_for_time(12, 0))
        check("cube-edges-are-antialiased", (edges?.levels ?? 0) >= 3 && (edges?.error ?? 99) < 4.5,
              ["partial_coverage_levels": edges?.levels ?? -1, "mean_error_vs_4x_supersampling_of_255": edges?.error ?? -1])
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

    // About as the app builds it, read back from Metal: the scene reaches the chrome and the cube is chrome.
    do {
        typealias Frame = (width: Int, height: Int, bgra: [UInt8])
        /// The cube's pixels: those in its centred square (60% of the band's height) that differ from the band's colour on the
        /// same row, read from a column left of the square. Returns their share of the square, mean colour and 5-to-95% luminance spread.
        func chrome(_ f: Frame) -> (covered: Double, mean: [Double], spread: Double) {
            let side = f.height * 6 / 10, x0 = (f.width - side) / 2, y0 = (f.height - side) / 2
            var n = 0.0, sum = [0.0, 0.0, 0.0], lum: [Double] = []
            for y in y0..<(y0 + side) {
                let ref = (y * f.width + max(x0 - 4, 0)) * 4
                for x in x0..<(x0 + side) {
                    let i = (y * f.width + x) * 4
                    guard (0..<3).contains(where: { abs(Int(f.bgra[i + $0]) - Int(f.bgra[ref + $0])) > 12 }) else { continue }
                    n += 1
                    let b = Double(f.bgra[i]), g = Double(f.bgra[i + 1]), r = Double(f.bgra[i + 2])
                    sum[0] += r; sum[1] += g; sum[2] += b
                    lum.append(0.2126 * r + 0.7152 * g + 0.0722 * b)
                }
            }
            lum.sort()
            let spread = lum.isEmpty ? 0 : lum[lum.count * 95 / 100] - lum[lum.count * 5 / 100]
            return (n / Double(max(1, side * side)), sum.map { $0 / max(n, 1) }, spread)
        }
        let w = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 360, height: 460), styleMask: [.titled], backing: .buffered, defer: false)
        w.isReleasedWhenClosed = false
        let host = NSHostingView(rootView: AboutView().environmentObject(app).frame(width: 360))
        w.contentView = host
        func find(_ v: NSView) -> CubeNSView? { (v as? CubeNSView) ?? v.subviews.lazy.compactMap(find).first }
        _ = waitFor(10) { find(host) != nil }
        var stats: [(id: String, c: (covered: Double, mean: [Double], spread: Double))] = []
        var rest = false, stepped = false
        if let v = find(host) {
            v.countsOffscreen = true
            host.layoutSubtreeIfNeeded()
            _ = waitFor(5) { v.cube != nil }
            for step in 0..<Int(origami_cube_scene_count()) {
                _ = v.present(); spin(0.3)
                let id = v.currentScene >= 0 ? sceneText(UInt32(v.currentScene), Int32(ORIGAMI_CUBE_SCENE_ID)) : "none"
                guard let f = v.readback() else { continue }
                writePNG(f, to: "\(out)/about-\(step)-\(id).png")
                stats.append((id, chrome(f)))
                // About rests (the design has no spin): a second frame is the same picture.
                if step == 0 { _ = v.present(); spin(0.2); rest = v.readback()?.bgra == f.bgra }
                v.stepScene(); spin(0.4)
                if step == 0 { _ = v.present(); stepped = v.readback()?.bgra != f.bgra }
                spin(1.0)
            }
        }
        w.contentView = NSView()
        // The photographs are embedded in the core, so a bundle needs no file for them; each reaches the chrome as its own colours.
        let means = Set(stats.map { $0.c.mean.map { Int($0 / 4) } })
        check("about-scenes-reach-the-chrome", stats.count == Int(origami_cube_scene_count()) && means.count >= stats.count - 2,
              ["scenes": stats.map { $0.id }, "distinct_means": means.count])
        // Chrome: the cube covers a good part of its view, spans dark to bright and is neither black nor white.
        let bad = stats.filter { $0.c.covered < 0.3 || $0.c.covered > 0.55 || $0.c.spread < 60 || $0.c.mean.contains { $0 < 30 || $0 > 220 } }
        check("about-cube-pixels-are-chrome", !stats.isEmpty && bad.isEmpty, ["outside_range": bad.map { $0.id }, "measured": stats.map { "\($0.id) covered=\(String(format: "%.3f", $0.c.covered)) spread=\(String(format: "%.1f", $0.c.spread)) mean=\($0.c.mean.map { Int($0) })" }])
        check("about-rests-and-the-fade-moves", rest && stepped, ["rest": rest, "stepped": stepped])
    }

    // The start screen's cube turns unless Reduce Motion is on, and then it stays at its still pose.
    do {
        let w = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 108, height: 108), styleMask: [.titled], backing: .buffered, defer: false)
        w.isReleasedWhenClosed = false
        let cube = CubeNSView(frame: NSRect(x: 0, y: 0, width: 108, height: 108))
        w.contentView = cube
        cube.countsOffscreen = true
        cube.configure(look: .starting(chassis: 0x2e354e), dark: true, machine: nil)
        _ = cube.present(); spin(1.5)
        let a = cube.readback()
        _ = cube.present(); spin(0.5); _ = cube.present()
        let b = cube.readback()
        let reduce = NSWorkspace.shared.accessibilityDisplayShouldReduceMotion
        check("start-cube-moves-between-frames-unless-reduce-motion", a != nil && b != nil && (a?.bgra != b?.bgra) == !reduce,
              ["reduce_motion": reduce, "moved": a?.bgra != b?.bgra])
        cube.detach()
    }

    // New Machine: default model, name rules, an error that keeps the sheet, and open on success.
    do {
        let nm = NewMachineModel(app: app)
        nm.load()
        check("new-machine-catalogue-loads", waitFor(30) { !nm.loading && !nm.offerings.isEmpty },
              ["offerings": nm.offerings.map(\.id), "selected": nm.offerings.indices.contains(nm.selected) ? nm.offerings[nm.selected].id : "none"])
        let expectedDefault = nm.offerings.firstIndex { $0.id == "octane2-vpro" } ?? 0
        check("new-machine-defaults-to-octane2-vpro-when-offered", nm.selected == expectedDefault, ["selected": nm.selected])
        // Advanced: the ID is prefilled with the next free one, and a bad one stops Create with the core's sentence.
        let prefilled = waitFor(30) { !nm.idText.isEmpty && nm.identity?.mac.isEmpty == false }
        check("new-machine-advanced-prefills-the-id-and-address", prefilled, ["id": nm.idText, "mac": nm.identity?.mac ?? ""])
        let next = nm.idText
        nm.idText = "0"
        let refused = waitFor(30) { nm.identityProblem != nil }
        nm.name = "x"
        check("new-machine-advanced-refuses-an-id-out-of-range", refused && !nm.canCreate, ["problem": nm.identityProblem ?? "none"])
        nm.idText = next
        _ = waitFor(30) { nm.identityProblem == nil }
        nm.name = ""; let emptyOff = !nm.canCreate
        nm.name = "  \t "; let blankOff = !nm.canCreate
        nm.name = " gamma "; let namedOn = nm.canCreate
        check("new-machine-create-needs-a-trimmed-name", emptyOff && blankOff && namedOn, [:])
        // A ready-made disk is a choice only while one is listed, and Create waits for one.
        nm.disk = .ready
        let readyBlocked = !nm.readyDisks.isEmpty || !nm.canCreate
        nm.disk = .noDisk
        check("new-machine-ready-disk-needs-a-listed-disk", readyBlocked && nm.choices.contains(.ready) == !nm.readyDisks.isEmpty
              && (try? host.core.cachedDisks()) != nil, [:])
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
            let want = Scaling.viewSize(guest: CGSize(width: picture.width, height: picture.height), available: avail)
            return abs(v.frame.width - want.width) < 1.5 && abs(v.frame.height - want.height) < 1.5
        }
        let got = displayViews(tabHost).first?.frame.size ?? .zero
        check(name, ok, ["view": "\(Int(got.width))x\(Int(got.height))", "host": "\(Int(tabHost.bounds.width))x\(Int(tabHost.bounds.height))",
                        "picture": "\(picture.width)x\(picture.height)"])
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
        let got = waitFor(20) { (load.scene?.bars.count ?? 0) == 2 }
        let bars = load.scene?.bars ?? []
        func pct(_ i: Int, _ j: Int) -> Float { j < bars.count ? bars[j].segments[i].fraction * 100 : -100 }
        func near(_ a: Float, _ b: Float) -> Bool { abs(a - b) < 6 }
        check("cpu-panel-shows-the-fake-engines-two-cpus", got && near(pct(0, 0), 30) && near(pct(1, 0), 10) && near(pct(0, 1), 60) && near(pct(1, 1), 5),
              ["bars": bars.map { "\($0.title) " + $0.segments.map { "\($0.fraction)" }.joined(separator: "/") }, "note": load.scene?.note ?? "none"])
        check("cpu-panel-legend-is-user-sys-spare", bars.first?.legend.map { $0.label } == ["user", "sys", "spare"]
              && bars.allSatisfy { abs($0.segments.map { $0.fraction }.reduce(0, +) - 1) < 0.001 }, [:])
        // Paint the panel into a picture and read it back: the user segment is blue, the spare rest green.
        if let scene = load.scene {
            let path = "\(out)/macos-osview-selftest.png"
            let shot = snapshot(CpuPanel(model: load), size: CGSize(width: 420, height: CpuPanel.height(bars: scene.bars.count)), to: path)
            let rep = (try? Data(contentsOf: URL(fileURLWithPath: path))).flatMap { NSBitmapImageRep(data: $0) }
            let y = Int(CpuPanel.pad + CpuPanel.header + 4)
            func rgb(_ x: Int) -> [Int] {
                let c = rep?.colorAt(x: x, y: y)?.usingColorSpace(.sRGB)
                return [c?.redComponent, c?.greenComponent, c?.blueComponent].map { Int((($0 ?? 0) * 255).rounded()) }
            }
            let left = rgb(20), right = rgb(400)
            check("cpu-panel-paints-blue-user-and-green-spare", shot.ok && left[2] > 200 && left[0] < 60 && right[1] > 200 && right[0] < 60, ["left": left, "right": right])
        }
        load.stop()
        let after = load.samples
        spin(1.5)
        check("cpu-panel-hidden-polls-nothing", load.samples == after && load.scene == nil, ["samples_after_stop": load.samples - after])
        check("cpu-placeholder-says-measuring-or-why", cpuLoadPlaceholder(nil) == "measuring…", [:])
        let emptyPath = "\(out)/macos-osview-empty.png"
        let empty = snapshot(CpuPanel(model: CpuLoadModel()), size: CGSize(width: 420, height: CpuPanel.height(bars: 0)), to: emptyPath)
        check("cpu-panel-before-the-first-sample-is-painted", empty.ok && empty.colours >= 2, ["colours": empty.colours])
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

    // The drawable is the view's own pixels, so the core does all the scaling.
    do {
        func own() -> Bool {
            let k = view.metalLayer.contentsScale
            return view.metalLayer.frame.size == view.bounds.size
                && view.metalLayer.drawableSize == CGSize(width: (view.bounds.width * k).rounded(), height: (view.bounds.height * k).rounded())
        }
        let before = own()
        view.setFrameSize(NSSize(width: 500, height: 400))
        check("drawable-is-the-views-own-pixels", before && own(),
              ["drawable": "\(view.metalLayer.drawableSize)", "layer": "\(view.metalLayer.frame.size)", "view": "\(view.bounds.size)"])
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
    let tips = ToolbarTip.all
    check("toolbar-buttons-have-tooltips", tips.count == 7 && tips.allSatisfy { !$0.isEmpty && !$0.contains("·") } && Set(tips).count == tips.count,
          ["tips": tips])

    // 7a2. The real toolbar of the main window: its items and tooltips, and a picture of it.
    do {
        let main = NSWindow(contentViewController: NSHostingController(rootView: MainView().environmentObject(app)))
        main.styleMask = [.titled, .closable, .resizable]
        main.setContentSize(NSSize(width: 1100, height: 720))
        main.orderFront(nil)
        spin(2)
        let items = main.toolbar?.items ?? []
        let named = items.filter { ToolbarTip.byLabel[$0.label] != nil }
        check("toolbar-items-have-tooltips", named.count == 6 && named.allSatisfy { $0.toolTip == ToolbarTip.byLabel[$0.label] },
              ["items": items.map { "\($0.label)|\($0.toolTip ?? "")|\($0.view?.toolTip ?? "")" }])
        if let frame = main.contentView?.superview, let rep = frame.bitmapImageRepForCachingDisplay(in: frame.bounds) {
            frame.cacheDisplay(in: frame.bounds, to: rep)
            try? rep.representation(using: .png, properties: [:])?.write(to: URL(fileURLWithPath: "\(out)/ui-main-window.png"))
        }
        main.contentViewController = nil   // a closed window's views would go on showing the current machine
        main.close()
        app.openWindow = nil; app.openConsole = nil   // MainView set them to this window's environment
    }

    // 7a. A toolbar action never moves or resizes the window. The fit to the picture happens when the view
    // arrives, so no later click carries it out.
    do {
        // The view is in a laid-out window before the picture size is known, as a machine's screen is on start.
        final class Arrival: ObservableObject { @Published var guest = CGSize.zero }
        struct Probe: View {
            @EnvironmentObject var app: AppModel
            @ObservedObject var arrival: Arrival
            let model: MachineModel
            var key = "probe"
            var body: some View {
                VStack(spacing: 0) {
                    GeometryReader { geo in
                        WindowFitter(key: key, model: model, guest: arrival.guest, available: geo.size, backing: 1).frame(width: 0, height: 0)
                    }
                    if app.showsCpu { Color.gray.frame(height: 80) }
                }
            }
        }
        let arrival = Arrival()
        FitView.visibleFrame = { _ in CGRect(x: 0, y: 0, width: 1600, height: 1200) }   // a runner has no screen
        let w = NSWindow(contentRect: NSRect(x: 40, y: 40, width: 900, height: 700), styleMask: [.titled, .resizable],
                         backing: .buffered, defer: false)
        w.isReleasedWhenClosed = false
        w.contentView = NSHostingView(rootView: Probe(arrival: arrival, model: model).environmentObject(app))
        w.orderFront(nil)
        spin(1)
        let waiting = w.frame
        check("window-waits-for-the-picture", waiting.size == CGSize(width: 900, height: waiting.height), ["frame": "\(waiting)"])
        arrival.guest = CGSize(width: 1280, height: 1024)
        spin(1)
        let settled = w.frame
        check("window-fits-when-the-picture-arrives", settled.width > 900 || settled.height > 700, ["frame": "\(settled)"])
        let actions: [(String, () -> Void)] = [
            ("cpu-on", { app.showsCpu = true }), ("cpu-off", { app.showsCpu = false }),
            ("console", { app.invoke(.console) }), ("diagnostics", { app.invoke(.diagnostics) }),
            ("settings", { app.invoke(.settings); app.wantSettings = false }),
            ("pointer", { app.invoke(.followMouse); app.invoke(.followMouse) }),
        ]
        for (name, act) in actions {
            act(); spin(0.5)
            check("toolbar-\(name)-keeps-the-window-frame", w.frame == settled, ["before": "\(settled)", "after": "\(w.frame)"])
        }
        w.close()
        // The first fit only grows: a window already bigger than a 640x480 picture needs keeps its size.
        let small = Arrival()
        let bw = NSWindow(contentRect: NSRect(x: 40, y: 40, width: 1400, height: 1000), styleMask: [.titled, .resizable],
                          backing: .buffered, defer: false)
        bw.isReleasedWhenClosed = false
        bw.contentView = NSHostingView(rootView: Probe(arrival: small, model: model, key: "probe-big").environmentObject(app))
        bw.orderFront(nil)
        spin(1)
        let roomy = bw.frame
        small.guest = CGSize(width: 640, height: 480)
        spin(1)
        check("first-fit-never-shrinks-the-window", bw.frame == roomy, ["before": "\(roomy)", "after": "\(bw.frame)"])
        bw.close()
    }

    // 7a1. A 1280x1024 picture on a 2x display, in the real tab: the first window is one point per guest pixel
    // or the largest fit, the picture fills every size the window takes, the View menu's scales size the
    // window once, and the core draws fractional scales sharp bilinear.
    do {
        do {
            let op = try host.core.createMachine(request: OrigamiCreateRequest(
                name: "wide", offering: "fakebig.1", memoryPerNodeMib: nil, graphics: nil, bootProm: library + "/prom.bin", ioProm: nil, engine: nil, systemDisk: nil, identity: nil))
            check("create-wide-machine", op.wait(timeoutMs: 10_000) == .succeeded, [:])
            app.rescanNow()
        } catch { check("create-wide-machine", false, ["error": "\(error)"]) }
        if let wide = app.model(named: "wide") {
            app.select(wide.id)
            func run(_ name: String) {
                wide.start()
                check(name, waitFor(60) {
                    try? host.core.fakeAdvanceFrame(key: wide.record.key)
                    return wide.state == .running && wide.picture?.width == 1280 && wide.picture?.height == 1024
                }, ["state": wide.state.pill, "picture": "\(String(describing: wide.picture))"])
            }
            func stop() {
                var off: Error?? = .none
                wide.stop { e in off = .some(e) }
                _ = waitFor(60) { off != nil && wide.state == .stopped }
            }
            // A 14-inch laptop has no room for one point per guest pixel: the first window is the largest fit.
            run("wide-machine-shows-1280x1024")
            FitView.visibleFrame = { _ in CGRect(x: 0, y: 0, width: 1512, height: 944) }
            do {
                let lw = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 1000, height: 700), styleMask: [.titled, .resizable],
                                  backing: .buffered, defer: false)
                lw.isReleasedWhenClosed = false
                let lt = NSHostingView(rootView: MachineTab(model: wide).environmentObject(app).environment(\.displayScale, 2))
                lw.contentView = lt
                var got = CGSize.zero
                let ok = waitFor(5) {
                    lt.layoutSubtreeIfNeeded()
                    got = displayViews(lt).first?.frame.size ?? .zero
                    let a = CGSize(width: lt.bounds.width - 60, height: lt.bounds.height - 60)
                    return abs(lw.frame.height - 944) < 0.5 && abs(got.height - a.height) < 0.5 && abs(got.width / max(1, got.height) - 1.25) < 0.01 && got.height < 1024
                }
                check("first-window-on-a-laptop-is-the-largest-fit", ok, ["view": "\(got)", "window": "\(lw.frame.size)"])
                lw.contentView = nil
                lw.close()
            }
            stop()
            run("wide-machine-runs-again")
            FitView.visibleFrame = { _ in CGRect(x: 0, y: 0, width: 2560, height: 1440) }   // a 5K display's room
            let w = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 1000, height: 700), styleMask: [.titled, .resizable],
                             backing: .buffered, defer: false)
            w.isReleasedWhenClosed = false
            let tab = NSHostingView(rootView: MachineTab(model: wide).environmentObject(app).environment(\.displayScale, 2))
            w.contentView = tab
            typealias Shown = (view: CGSize, layer: CGSize, drawable: CGSize, avail: CGSize)
            func shown() -> Shown {
                tab.layoutSubtreeIfNeeded()
                let v = displayViews(tab).first
                let a = CGSize(width: tab.bounds.width - 60, height: tab.bounds.height - 60)
                return (v?.frame.size ?? .zero, v?.metalLayer.frame.size ?? .zero, v?.metalLayer.drawableSize ?? .zero, a)
            }
            func fills(_ s: Shown) -> Bool {
                abs(min(s.avail.width - s.view.width, s.avail.height - s.view.height)) < 1 && abs(s.view.width / max(1, s.view.height) - 1.25) < 0.01
            }
            // The core draws into the view's own pixels.
            func drawn(_ s: Shown) -> Bool {
                let k = displayViews(tab).first?.metalLayer.contentsScale ?? 0
                return s.layer == s.view && s.drawable == CGSize(width: (s.view.width * k).rounded(), height: (s.view.height * k).rounded())
            }
            func info(_ s: Shown) -> [String: Any] {
                ["view": "\(s.view)", "layer": "\(s.layer)", "drawable": "\(s.drawable)", "avail": "\(s.avail)", "window": "\(w.frame.size)"]
            }
            var s = shown()
            check("first-window-on-a-5k-display-is-one-point", waitFor(5) { s = shown(); return s.view == CGSize(width: 1280, height: 1024) && fills(s) && drawn(s) }, info(s))
            // Each scale sizes the window once, to exactly that picture; Fit leaves the window alone.
            FitView.visibleFrame = { _ in CGRect(x: 0, y: 0, width: 4000, height: 3000) }
            for (c, want) in [(CommandID.actualSize, CGSize(width: 640, height: 512)), (.doubleScale, CGSize(width: 2560, height: 2048)),
                              (.pointScale, CGSize(width: 1280, height: 1024))] {
                app.invoke(c)
                check("view-menu-\(c.rawValue)-sets-the-exact-size", waitFor(5) { s = shown(); return s.view == want && fills(s) && drawn(s) }, info(s))
            }
            let before = w.frame
            app.invoke(.fit); spin(0.5)
            check("view-menu-fit-keeps-the-window", w.frame == before && fills(shown()), info(shown()))
            // A resize, as dragging an edge or the zoom button does: the frame stays what it was set to and the picture follows.
            var last = shown().view
            for size in [NSSize(width: 1100, height: 760), NSSize(width: 1512, height: 944), NSSize(width: 2100, height: 1700)] {
                w.setContentSize(size)
                let kept = w.frame
                let ok = waitFor(5) { s = shown(); return fills(s) && drawn(s) && s.view != last }
                spin(0.5)
                check("picture-follows-a-resize-to-\(Int(size.width))x\(Int(size.height))", ok && w.frame == kept,
                      info(s).merging(["before": "\(last)", "frame_kept": w.frame == kept]) { a, _ in a })
                last = s.view
            }
            // At 1.59 output pixels per guest pixel, the fake picture's red channel drops from 255 to 0 every 256
            // guest pixels. Sharp bilinear blends each drop over at most one output pixel; plain bilinear over two.
            if let v = displayViews(tab).first {
                try? host.core.fakeAdvanceFrame(key: wide.record.key)
                _ = waitFor(5) { v.presented > 0 }
                if let rb = v.readback() {
                    let k = Double(rb.width) / 1280
                    func red(_ x: Int, _ y: Int) -> Int { Int(rb.bgra[(y * rb.width + x) * 4 + 2]) }
                    func green(_ x: Int, _ y: Int) -> Int { Int(rb.bgra[(y * rb.width + x) * 4 + 1]) }
                    var blends: [Int] = []
                    for g in [256, 512, 768, 1024] {
                        let at = Int(Double(g) * k), y = Int(100.5 * k)
                        blends.append((at - 3...at + 3).filter { (24...231).contains(red($0, y)) }.count)
                    }
                    for g in [256, 512, 768] {
                        let at = Int(Double(g) * k), x = Int(100.5 * k)
                        blends.append((at - 3...at + 3).filter { (24...231).contains(green(x, $0)) }.count)
                    }
                    check("fractional-scale-is-sharp-bilinear", k > 1.5 && k < 1.7 && blends.allSatisfy { $0 <= 1 },
                          ["readback": "\(rb.width)x\(rb.height)", "scale": k, "blended_pixels_per_edge": blends])
                } else { check("fractional-scale-is-sharp-bilinear", false, ["error": v.lastError]) }
            }
            w.contentView = nil
            w.close()
            stop()
            app.closeTab(wide.id)
        } else { check("wide-machine-shows-1280x1024", false, ["error": "no model for the wide machine"]) }
        app.select(model.id)
    }

    // 7b. Pause and Resume.
    do {
        var done: Error?? = .none
        model.pause { e in done = .some(e) }
        // `done` fires before the background refresh applies the new state, so wait on the state too.
        check("pause", waitFor(30) { done != nil && model.state == .paused } && app.isEnabled(.resume) && !app.isEnabled(.pause),
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
        check("resume", waitFor(30) { done != nil && model.state == .running }, ["state": model.state.pill])
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
                name: "delta", offering: "fake.1", memoryPerNodeMib: nil, graphics: nil, bootProm: library + "/prom.bin", ioProm: nil, engine: nil, systemDisk: nil, identity: nil))
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

    // An older machine (the format 4 fixture) is listed with its own pill, cannot start, and is upgraded through the
    // model the card's Upgrade button calls; its notes show on the machine, which then starts.
    do {
        let fixtures = ProcessInfo.processInfo.environment["ORIGAMI_FIXTURES"] ?? ""
        try? FileManager.default.createDirectory(atPath: library + "/old", withIntermediateDirectories: true)
        let copied = (try? FileManager.default.copyItem(atPath: fixtures + "/format4-origin200.toml", toPath: library + "/old/machine.toml")) != nil
        // A real older machine has its state files; the fake engine wants these two to start it.
        try? FileManager.default.createDirectory(atPath: library + "/old/state", withIntermediateDirectories: true)
        for f in ["nvram0", "flash0"] { FileManager.default.createFile(atPath: library + "/old/state/\(f).raw", contents: Data(count: 16)) }
        check("upgrade-fixture-copied", copied, ["fixtures": fixtures])
        app.rescanNow()
        if let old = app.model(named: "old") {
            check("upgrade-pill-and-no-start", old.state == .needsUpgrade && old.state.pill == "Needs upgrade" && !old.canStart,
                  ["state": old.state.pill])
            let card = snapshot(MachineTab(model: old).environmentObject(app), size: CGSize(width: 760, height: 560), to: "\(out)/ui-upgrade-card.png")
            check("ui-upgrade-card-renders", card.ok && card.colours > 5, ["distinct_colours_sampled": card.colours])
            old.upgrade()
            check("upgrade-replaces-the-card-with-the-machine", waitFor(60) { old.state == .stopped && !old.upgrading },
                  ["state": old.state.pill, "error": old.upgradeError ?? "none"])
            let notes = old.upgradeNotes.compactMap(\.shown)
            check("upgrade-notes-are-kept", notes.contains { $0.contains("format 5") }, ["notes": notes])
            old.start()
            check("upgraded-machine-starts", waitFor(60) { old.state == .running }, ["state": old.state.pill, "failure": old.failure ?? "none"])
            old.stop()
            _ = waitFor(60) { old.state == .stopped }
        } else { check("upgrade-pill-and-no-start", false, ["error": "the older machine is not listed"]) }
    }
    // Disks, through the models the sheets and panes call: an empty disk, an image copied in and one used in place,
    // the OS badge on the library row, and a CD swapped and ejected while the machine runs.
    do {
        let fixtures = ProcessInfo.processInfo.environment["ORIGAMI_FIXTURES"] ?? ""
        let image = URL(fileURLWithPath: fixtures + "/disk-irix-6.5.30.img")
        func made(_ name: String, _ setup: (NewMachineModel) -> Void) -> String? {
            let nm = NewMachineModel(app: app)
            nm.load(); _ = waitFor(30) { !nm.loading }
            nm.bootProm = library + "/prom.bin"; nm.name = name
            setup(nm)
            var result: Bool?
            nm.create { result = $0 }
            _ = waitFor(60) { result != nil }
            app.rescanNow()
            return app.model(named: name)?.id
        }
        func chosen(_ nm: NewMachineModel) { nm.disk = .image; nm.image.choose(); _ = waitFor(30) { nm.image.check != nil } }
        let drives = { (key: String?) in key.flatMap { try? host.core.openMachine(key: $0).settings().drives } ?? [] }
        let empty = drives(made("emptybox") { $0.disk = .empty; $0.sizeGB = 1 })
        check("disk-empty-made-with-the-machine", empty.count == 1 && empty.first?.target == 1, ["drives": empty.map(\.image)])
        var copyNote = "", copyBadge = "", placeNote = ""
        PathPicker.answers = [image]
        let copyKey = made("copybox") { nm in chosen(nm); copyNote = nm.image.note?.text ?? ""; copyBadge = nm.image.badge }
        let copied = drives(copyKey)
        check("disk-copy-keeps-the-image-in-the-machine-folder",
              copied.first?.image == "drives/disk-irix-6.5.30.img" && FileManager.default.fileExists(atPath: library + "/copybox/drives/disk-irix-6.5.30.img"),
              ["drives": copied.map(\.image)])
        check("disk-copy-says-the-original-is-safe", copyNote.contains("keeps your original safe"), ["note": copyNote])
        check("disk-badge-names-the-os-in-the-sheet", copyBadge == "IRIX 6.5.30", ["badge": copyBadge])
        check("disk-badge-names-the-os-on-the-library-row",
              waitFor(60) { app.rescanNow(); return app.model(named: "copybox")?.record.systemDisk == "IRIX 6.5.30" },
              ["badge": app.model(named: "copybox")?.record.systemDisk ?? "none"])
        PathPicker.answers = [image]
        let placeKey = made("placebox") { nm in chosen(nm); nm.image.copy = false; placeNote = nm.image.note?.text ?? "" }
        let placed = drives(placeKey)
        check("disk-in-place-stays-where-it-is",
              placed.first?.image.hasSuffix("/disk-irix-6.5.30.img") == true && !FileManager.default.fileExists(atPath: library + "/placebox/drives/disk-irix-6.5.30.img"),
              ["drives": placed.map(\.image)])
        check("disk-in-place-warns-that-irix-writes-into-it", placeNote.contains("IRIX will write into this file"), ["note": placeNote])
        // The CD: attached while stopped, then swapped and ejected on the running machine, whose file keeps the first.
        if makeFakeMachine(host, library: library, name: "cdvic") == nil {
            app.rescanNow()
            if let vic = app.model(named: "cdvic") {
                let sm = SettingsModel(machine: vic)
                sm.load(); _ = waitFor(30) { sm.settings != nil }
                let (one, two) = (library + "/one.iso", library + "/two.iso")
                for c in [one, two] { FileManager.default.createFile(atPath: c, contents: Data(count: 2048)) }
                sm.attachCD(URL(fileURLWithPath: one), target: sm.settings?.cdTarget ?? 4)
                let cd = { sm.settings?.drives.first { $0.kind == "cdrom" } }
                check("disk-cd-attached-while-stopped", waitFor(30) { cd() != nil && !sm.working }, ["error": sm.error ?? "none"])
                let drive = cd()?.name ?? ""
                _ = try? host.core.startMachine(key: vic.record.key)
                sm.changeMedia(drive, to: URL(fileURLWithPath: two))
                _ = waitFor(30) { !sm.working }
                check("disk-cd-swapped-while-running", ((try? host.core.fakeMedia(key: vic.record.key)) ?? []) == ["\(drive) \(two)"], ["error": sm.error ?? "none"])
                sm.changeMedia(drive, to: nil)
                _ = waitFor(30) { !sm.working }
                check("disk-cd-ejected-while-running", ((try? host.core.fakeMedia(key: vic.record.key)) ?? []).last == "\(drive) eject", ["error": sm.error ?? "none"])
                check("disk-cd-swap-leaves-the-file-alone", cd()?.image.hasSuffix("one.iso") == true, ["image": cd()?.image ?? "none"])
                _ = try? host.core.openMachine(key: vic.record.key).stop(how: .force).wait(timeoutMs: 30_000)
            } else { check("disk-cd-attached-while-stopped", false, ["error": "the machine is not listed"]) }
        } else { check("disk-cd-attached-while-stopped", false, ["error": "the machine was not made"]) }
    }
    // The Install IRIX assistant, through its model on the fake engine's stand-in media server: release, media, software, disk,
    // check, serving with the machine running, the console, the guide, and Finish putting the network back.
    do {
        let nm = NewMachineModel(app: app)
        nm.load(); _ = waitFor(30) { !nm.loading }
        nm.disk = .noDisk
        let offeredWithoutDisk = nm.offersInstall
        nm.disk = .empty
        check("install-after-creating-is-offered-only-with-an-empty-disk", !offeredWithoutDisk && nm.offersInstall, [:])
        check("install-command-in-the-core-table", host.core.commands().contains { $0.id == "machine.install-irix" }, [:])
        if makeFakeMachine(host, library: library, name: "irixbox") == nil {
            app.rescanNow()
            if let box = app.model(named: "irixbox") {
                app.select(box.id)
                check("install-command-enabled-while-stopped", app.isEnabled(.installIrix), [:])
                app.invoke(.installIrix)
                if let im = app.installModel(for: box.id) {
                    _ = waitFor(30) { im.loaded }
                    check("install-opens-at-the-newest-release-with-standard",
                          im.step == .release && im.flow.release() == origamiInstallReleases().first?.release && im.flow.profile() == "standard",
                          ["release": im.flow.release(), "profile": im.flow.profile()])
                    im.next()
                    // A folder with no discs is red and blocks Continue; downloading clears it.
                    let bare = library + "/install-media"
                    try? FileManager.default.createDirectory(atPath: bare, withIntermediateDirectories: true)
                    im.choosePath = { _, _ in URL(fileURLWithPath: bare) }
                    im.edit { $0.setUseFolder(on: true) }; im.chooseFolder()
                    _ = waitFor(30) { im.folderVerdict != nil }
                    check("install-empty-folder-blocks-continue",
                          im.folderVerdict == "That folder has no 6.5.30 or 6.5-base folder." && im.folderTone == .red && !im.canContinue,
                          ["verdict": im.folderVerdict ?? "none"])
                    im.edit { $0.setUseFolder(on: false) }
                    check("install-download-allows-continue", im.canContinue, [:])
                    im.next()
                    _ = waitFor(30) { !im.flow.addons().isEmpty }
                    im.edit { $0.tick(name: "mipspro", on: true) }
                    im.next()
                    _ = waitFor(30) { im.diskKnown }
                    check("install-disk-step-offers-a-disk-of-the-default-size",
                          im.step == .disk && im.disk == nil && im.flow.newDiskGb() == origamiDiskLabels().defaultGb && im.canContinue,
                          ["size": im.flow.newDiskGb()])
                    im.next()
                    check("install-disk-is-created", waitFor(60) { im.step == .check && im.disk != nil }, ["error": im.error ?? "none"])
                    im.runCheck()
                    check("install-check-passes", waitFor(60) { im.flow.check() == .passed && im.canContinue }, ["error": im.error ?? "none"])
                    im.next()
                    check("install-serves-and-starts-the-machine",
                          waitFor(60) { im.footer == "10.98.0.2 serving 6.5.30, Standard + MIPSpro" && box.state == .running },
                          ["footer": im.footer, "state": box.state.pill, "failed": im.serveFailed ?? "none"])
                    _ = waitFor(30) { im.guide != nil }
                    // Type It exists only on a serial machine; pressed, its text reaches the fake guest's console.
                    // Cancel here asks the core's question first; Back keeps the assistant and its server footer.
                    var asked: (title: String, buttons: [String])?
                    im.ask = { title, _, buttons, _ in asked = (title, buttons); return buttons.firstIndex(of: "Back") ?? 0 }
                    im.requestClose()
                    check("install-cancel-asks-before-stopping-the-machine",
                          asked?.title == "Stop the machine?" && asked?.buttons.contains("Back") == true, ["asked": asked?.title ?? "none"])
                    check("install-cancel-back-keeps-the-window-and-server",
                          !im.closing && im.footer == "10.98.0.2 serving 6.5.30, Standard + MIPSpro" && im.serving, ["footer": im.footer])
                    let serialBox = (try? host.core.openMachine(key: box.id).bootConsole()) == .serial
                    check("install-type-it-follows-the-boot-console", im.canTypeIt == serialBox, ["serial": serialBox])
                    if im.canTypeIt {
                        let send = im.guide?.prepare.compactMap(\.send).first
                        let from = box.session?.machine.consoleRead(from: 0, max: 1).next ?? 0
                        if let send { im.typeIt(send) }
                        let reached = waitFor(30) {
                            guard let send, let c = box.session?.machine.consoleRead(from: from, max: 4096) else { return false }
                            return String(decoding: c.bytes, as: UTF8.self).contains(send)
                        }
                        check("install-type-it-reaches-the-console", send != nil && reached, ["send": send ?? "none"])
                    }
                    im.next(); im.next(); im.next()
                    check("install-reaches-finish-and-waits-for-the-machine", im.step == .finish && !im.canContinue, ["state": box.state.pill])
                    box.stop()
                    check("install-finish-waits-for-the-machine-to-stop", waitFor(60) { box.state == .stopped && im.canContinue }, ["state": box.state.pill])
                    var closed = false
                    im.onClose = { closed = true }
                    im.next()
                    check("install-finish-closes-the-window", waitFor(30) { closed && !im.serving }, ["error": im.error ?? "none"])
                    let sm = SettingsModel(machine: box)
                    sm.load(); _ = waitFor(30) { sm.settings != nil }
                    check("install-finish-puts-the-network-back-as-it-was", sm.settings?.network.mode == "user", ["mode": sm.settings?.network.mode ?? "none"])
                } else { check("install-opens-an-assistant", false, [:]) }
            } else { check("install-opens-an-assistant", false, ["error": "the machine is not listed"]) }
        } else { check("install-opens-an-assistant", false, ["error": "the machine was not made"]) }
    }
    #else
    check("fake-build", false, ["error": "the core reports the fake engine but this app was built without -D FAKE"])
    #endif
    finish()
}
