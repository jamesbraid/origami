// `OrigamiApp --docs-shots LIBRARY OUTDIR`: the pictures docs/app.md shows, taken of real windows with screencapture.
// On the QEMU build it creates an Octane2 from the catalogue and starts it; on the fake build it makes a fake machine.

import AppKit
import Foundation
import OrigamiFrame
import SwiftUI

@MainActor
func runDocsShots(library: String, out: String) {
    func fail(_ why: String) -> Never {
        FileHandle.standardError.write(Data("docs-shots: \(why)\n".utf8))
        exit(1)
    }
    do { try FileManager.default.createDirectory(atPath: out, withIntermediateDirectories: true) } catch { fail("cannot create \(out): \(error)") }
    let app = AppModel(root: URL(fileURLWithPath: library))
    guard let host = app.host else { fail(app.launchError ?? "the core did not start") }
    NSApp.activate(ignoringOtherApps: true)

    func window<V: View>(_ title: String, _ view: V, _ w: CGFloat, _ h: CGFloat, fit: Bool = false, dark: Bool = false, using model: AppModel? = nil) -> NSWindow {
        let controller = NSHostingController(rootView: view.environmentObject(model ?? app))
        let win = NSWindow(contentViewController: controller)
        win.styleMask = [.titled, .closable, .resizable]
        win.title = title
        win.isReleasedWhenClosed = false
        if dark { win.appearance = NSAppearance(named: .darkAqua) }
        win.setContentSize(fit ? controller.view.fittingSize : NSSize(width: w, height: h))
        win.center()
        return win
    }
    /// The window as the screen shows it, from the window server, with no shadow.
    func capture(_ win: NSWindow, _ name: String, settle: Double = 3, hide: Bool = true, after: (() -> Void)? = nil) {
        func activate() {
            NSApp.activate(ignoringOtherApps: true)
            win.makeKeyAndOrderFront(nil)
            win.makeKey()
        }
        activate()
        spin(2)
        after?()
        spin(settle)
        // An inactive window draws grey traffic lights and the grey selection: wait for key before capturing.
        activate()
        if !waitFor(10, { win.isKeyWindow && NSApp.isActive }) {
            FileHandle.standardError.write(Data("docs-shots: \(name): window is not key (app active \(NSApp.isActive))\n".utf8))
        }
        spin(0.5)
        let file = "\(out)/\(name).png"
        // The app is started through LaunchServices to be frontmost, and a window server capture needs the launcher's
        // permission, not ours: the script that started us takes it when asked by file (want-NAME.txt, then got-NAME.txt).
        try? "\(win.windowNumber)".write(toFile: "\(out)/want-\(name).txt", atomically: true, encoding: .utf8)
        guard waitFor(60, { FileManager.default.fileExists(atPath: "\(out)/got-\(name).txt") }),
              FileManager.default.fileExists(atPath: file) else { fail("the capture of \(name) was not made") }
        if hide { win.orderOut(nil) }
    }

    /// The machine's Metal picture is not in a window capture on a runner without a GPU: draw the guest's own screenshot over it.
    func laidOverPicture(_ win: NSWindow, _ model: MachineModel, _ name: String) {
        func views(_ v: NSView) -> [MachineView] { (v as? MachineView).map { [$0] } ?? v.subviews.flatMap(views) }
        guard let content = win.contentView, let view = views(content).first else { fail("no machine view in the window") }
        let guest = URL(fileURLWithPath: "\(out)/guest.png")
        var saved = false
        model.screenshot(to: guest) { e in if let e { fail("guest screenshot: \(e)") }; saved = true }
        _ = waitFor(30) { saved }
        let file = "\(out)/\(name).png"
        guard let base = NSBitmapImageRep(data: (try? Data(contentsOf: URL(fileURLWithPath: file))) ?? Data()),
              let pic = NSBitmapImageRep(data: (try? Data(contentsOf: guest)) ?? Data()) else { fail("cannot read the pictures to combine") }
        let scale = CGFloat(base.pixelsWide) / win.frame.width
        let titlebar = win.frame.height - win.contentLayoutRect.height
        let r = view.convert(view.bounds, to: nil)
        let rect = CGRect(x: r.minX * scale, y: (titlebar + content.frame.height - r.maxY) * scale, width: r.width * scale, height: r.height * scale)
        let result = NSImage(size: NSSize(width: base.pixelsWide, height: base.pixelsHigh))
        result.lockFocus()
        base.draw(in: NSRect(x: 0, y: 0, width: base.pixelsWide, height: base.pixelsHigh))
        // lockFocus has its origin at the bottom left.
        pic.draw(in: NSRect(x: rect.minX, y: CGFloat(base.pixelsHigh) - rect.maxY, width: rect.width, height: rect.height))
        result.unlockFocus()
        guard let tiff = result.tiffRepresentation, let png = NSBitmapImageRep(data: tiff)?.representation(using: .png, properties: [:]) else { fail("cannot encode \(name)") }
        try? png.write(to: URL(fileURLWithPath: file))
        try? FileManager.default.removeItem(at: guest)
    }

    /// The library list draws its selection in the accent colour only while it is the first responder.
    func focusLibrary(_ win: NSWindow) {
        func table(_ v: NSView) -> NSTableView? { (v as? NSTableView) ?? v.subviews.lazy.compactMap(table).first }
        if let content = win.contentView, let t = table(content) { win.makeFirstResponder(t) }
    }

    /// The pictures the visual sign-off asks for, with the machines stopped: real windows, light and dark.
    func uxShots() {
        let fixtures = ProcessInfo.processInfo.environment["ORIGAMI_FIXTURES"] ?? ""
        // A machine with a recognised system disk: its page has the System card.
        func fresh() -> NewMachineModel {
            let m = NewMachineModel(app: app)
            m.load()
            _ = waitFor(60) { !m.loading }
            #if !FAKE
            m.engine = "qemu"
            if let pick = m.offerings.firstIndex(where: { $0.id.hasPrefix("octane2") && $0.engine == "qemu" }) { m.selected = pick }
            #endif
            return m
        }
        let sys = fresh()
        sys.name = "octane2-irix"
        sys.disk = .image
        sys.image.set(URL(fileURLWithPath: fixtures + "/disk-irix-6.5.30.img"))
        guard waitFor(60, { sys.image.check != nil }) else { fail("the fixture disk image was not read (\(fixtures))") }
        var madeSys: Bool?
        sys.create { madeSys = $0 }
        guard waitFor(300, { madeSys != nil }), madeSys == true else { fail("creating the machine with a disk failed: \(sys.error ?? "?")") }
        guard waitFor(120, { app.rescanNow(); return app.model(named: "octane2-irix")?.record.systemDisk != nil }),
              let withDisk = app.model(named: "octane2-irix") else { fail("the system disk was not recognised") }
        app.select(withDisk.id)
        let main = window("Origami", MainView(), 1100, 720)
        capture(main, "macos-system-card", hide: false, after: { focusLibrary(main) })
        main.appearance = NSAppearance(named: .darkAqua)
        capture(main, "macos-system-card-dark", hide: false, after: { focusLibrary(main) })
        main.orderOut(nil)

        // The cards for each state, on machines that are in no library; light, and dark for the first and last.
        func card(_ name: String, _ state: MachineState, dark: Bool = false, problem: String? = nil, prepare: (MachineModel) -> Void = { _ in }) {
            let m = MachineModel(summary: syntheticSummary("octane2", problem: problem), host: host, defaultPointer: .follow)
            prepare(m)
            m.injectStaleState(state)
            capture(window("octane2", MachineTab(model: m), 700, 460, dark: dark), "macos-card-\(name)\(dark ? "-dark" : "")")
        }
        let slot = OrigamiErrorInfo(kind: .invalid, message: "Its graphics board is set to a slot this model doesn't have.", detail: "graphics slot 7", fix: .openSettings)
        for dark in [false, true] {
            card("couldnt-start", .stoppedUnexpectedly, dark: dark) { m in m.failStartForTest(slot); _ = waitFor(10) { m.state == .stoppedUnexpectedly } }
            card("no-picture", .noPicture, dark: dark)
            card("saving", .stopping, dark: dark)
            card("left-running", .leftRunning, dark: dark)
            card("needs-upgrade", .needsUpgrade, dark: dark)
            card("cant-open", .stopped, dark: dark, problem: "machine.toml could not be read")
        }
        let rows = VStack(alignment: .leading, spacing: 4) {
            LibraryRow(model: MachineModel(summary: syntheticSummary("octane2", problem: "machine.toml could not be read"), host: host, defaultPointer: .follow))
            LibraryRow(model: MachineModel(summary: syntheticSummary("fuel"), host: host, defaultPointer: .follow))
        }.padding(10).frame(width: 260)
        capture(window("Library", rows, 260, 100), "macos-row-cant-open")
        let emptyRoot = URL(fileURLWithPath: out).appendingPathComponent("empty-library")
        let empty = AppModel(root: emptyRoot)
        capture(window("Origami", MainView(), 1100, 720, using: empty), "macos-empty-library", hide: true)

        // Each alert, with its default button.
        let alerts: [(String, OrigamiAlertKind)] = [
            ("power-off", .powerOff(name: "octane2")), ("reset", .reset(name: "octane2")), ("force-quit", .forceQuit(name: "octane2")),
            ("quit-running", .quitRunning(names: ["octane2"])), ("remove", .remove(name: "octane2", diskLeft: [])),
        ]
        for (name, kind) in alerts {
            let alert = Alerts.make(origamiAlert(kind: kind, platform: thisPlatform))
            alert.layout()
            capture(alert.window, "macos-alert-\(name)")
        }

        // The Keyboard shortcuts window, and the menu bar's commands as the app lists them.
        capture(window(ui(.keyboardShortcuts), ShortcutsView(), 440, 520), "macos-shortcuts")
        capture(window("Menus", MenuSummary(), 760, 440), "macos-menus")

        // New machine with Advanced open and an error under the ID.
        // The sheet reloads the catalogue when it appears, so the choices are made once it is up.
        let adv = NewMachineModel(app: app)
        let advWin = window("New machine", NewMachineSheet(model: adv), 460, 560)
        capture(advWin, "macos-new-machine-advanced", after: {
            _ = waitFor(60) { !adv.loading }
            #if !FAKE
            adv.engine = "qemu"
            if let pick = adv.offerings.firstIndex(where: { $0.id.hasPrefix("octane2") && $0.engine == "qemu" }) { adv.selected = pick }
            #endif
            adv.name = "octane2"
            adv.advancedOpen = true
            adv.idText = "0"
            _ = waitFor(30) { adv.identityProblem(for: .id) != nil }
            advWin.setContentSize(NSSize(width: 460, height: min(adv.sheetHeight, NewMachineSheet.screenRoom)))
        })
    }

    #if FAKE
    if let e = makeFakeMachine(host, library: library, name: "octane2") { fail("creating the machine failed: \(e)") }
    app.rescanNow()
    let nm = NewMachineModel(app: app)
    nm.load()
    guard waitFor(60, { !nm.loading }) else { fail("the New Machine catalogue did not load") }
    #else
    let nm = NewMachineModel(app: app)
    #endif
    // The sheet loads the catalogue when it appears and resets its choice, so the choice is made once it is up.
    func choose() {
        _ = waitFor(60) { !nm.loading }
        #if !FAKE
        nm.engine = "qemu"
        guard let pick = nm.offerings.firstIndex(where: { $0.id.hasPrefix("octane2") && $0.engine == "qemu" }) else {
            fail("the catalogue has no Octane2 on qemu: \(nm.offerings.map(\.id))")
        }
        nm.selected = pick
        #endif
        nm.name = "octane2"
    }

    // The pictures show the whole sheet, even on a runner whose screen is shorter than it.
    NewMachineSheet.roomLimit = 1000

    // New Machine, with the model picked and a name typed, before the machine exists.
    let newWin = window("New machine", NewMachineSheet(model: nm), 460, 560)
    capture(newWin, "macos-new-machine", after: { choose(); newWin.setContentSize(NSSize(width: 460, height: min(nm.sheetHeight, NewMachineSheet.screenRoom))) })

    // The same sheet with a ready-made disk picked: the list comes from origami-dist.
    func chooseReady() {
        choose()
        guard waitFor(60, { !nm.readyDisks.isEmpty }) else { fail("origami-dist listed no ready-made disk for \(nm.name)") }
        nm.disk = .ready
    }
    let readyWin = window("New machine", NewMachineSheet(model: nm), 460, 560)
    capture(readyWin, "macos-ready-disk", after: { chooseReady(); readyWin.setContentSize(NSSize(width: 460, height: min(nm.sheetHeight, NewMachineSheet.screenRoom))) })
    nm.disk = .noDisk

    #if !FAKE
    choose()
    var created: Bool?
    nm.create { created = $0 }
    guard waitFor(300, { created != nil }), created == true else { fail("creating the machine failed: \(nm.error ?? "?")") }
    #endif
    guard let model = app.models.first else { fail("no machine in \(library)") }
    app.select(model.id)
    let main = window("Origami", MainView(), 1100, 720)
    capture(main, "macos-library", hide: false, after: { focusLibrary(main) })

    // The Install IRIX assistant on the stopped machine, at its Software step.
    var installSteps: [String] = []
    if let install = app.installModel(for: model.id) {
        _ = waitFor(30) { install.loaded }
        install.next()
        install.next()
        _ = waitFor(30) { !install.flow.addons().isEmpty }
        installSteps = install.flow.stepTitles().map { $0.title }
        capture(window("Install IRIX: \(model.name)", InstallSetupSheet(model: install), 740, 480), "macos-install-assistant")
        app.forgetInstall(model.id)
    } else { fail("no install assistant for \(model.name)") }
    // Settings at Network, with Bridged chosen and its fields open. The sheet reloads when it appears, which closes the fields.
    let settings = SettingsModel(machine: model)
    func chooseBridged() {
        _ = waitFor(30) { settings.settings != nil && !settings.working }
        guard let bridged = settings.networkChoices.first(where: { $0.label == "Bridged" }) else { fail("the machine's settings offer no Bridged mode") }
        settings.chooseNetwork(bridged.mode)
    }
    capture(window("\(model.name) Settings", MachineSettings(model: model, pane: .network, edit: settings), 700, 460), "macos-network", after: chooseBridged)

    var started = false
    model.start { _ in started = true }
    guard waitFor(300, { started }) else { fail("the machine did not start") }
    #if FAKE
    try? host.core.fakeAdvanceFrame(key: model.record.key)
    #endif
    guard waitFor(120, { model.state == .running }) else { fail("the machine did not reach Running (\(model.state.word))") }
    // The PROM takes a while to draw its menu.
    spin(90)
    capture(main, "macos-running", settle: 3, hide: false, after: { focusLibrary(main) })
    laidOverPicture(main, model, "macos-running")
    app.showsCpu = true
    capture(main, "macos-cpu-panel", settle: 4, hide: false, after: { focusLibrary(main) })
    laidOverPicture(main, model, "macos-cpu-panel")
    app.showsCpu = false
    // The status bar while the mouse is captured, and the same window in dark mode.
    model.pointerMode = .capture
    model.captureChanged(true)
    _ = waitFor(20) { model.statusBar?.pointer?.hasPrefix("Captured") == true }
    capture(main, "macos-status-captured", settle: 3, hide: false, after: { focusLibrary(main) })
    laidOverPicture(main, model, "macos-status-captured")
    model.captureChanged(false)
    model.pointerMode = .follow
    _ = waitFor(20) { model.statusBar?.pointer?.hasPrefix("Follows") == true }
    main.appearance = NSAppearance(named: .darkAqua)
    capture(main, "macos-running-dark", settle: 3, hide: false, after: { focusLibrary(main) })
    laidOverPicture(main, model, "macos-running-dark")
    main.appearance = nil
    capture(window("Diagnostics", DiagnosticsView(), 640, 400), "macos-diagnostics")
    let about = window("About Origami", AboutView(), 360, 0, fit: true)
    capture(about, "macos-about", settle: 4, hide: false)
    // The window capture cannot see the Metal layer on a runner, so the cube's own readback is kept beside it.
    func cubes(_ v: NSView) -> [CubeNSView] { (v as? CubeNSView).map { [$0] } ?? v.subviews.flatMap(cubes) }
    if let c = about.contentView.flatMap(cubes)?.first, let f = c.readback() { writePNG(f, to: "\(out)/macos-about-readback.png") }
    about.orderOut(nil)
    capture(window("Acknowledgements", AcknowledgementsView(), 480, 560), "macos-acknowledgements")
    capture(window("Licences", LicencesView(), 520, 600), "macos-licences")

    main.orderOut(nil)
    var stopped = false
    app.stopAll(force: true) { _ in stopped = true }
    _ = waitFor(120) { stopped }
    uxShots()

    // The names the other apps must match: tools/screenshots parity compares these files.
    let names: [String: Any] = ["platform": "macos", "sidebar_heading": Labels.sidebarHeading,
                                "settings_tabs": MachineSettings.Pane.allCases.map(\.rawValue),
                                "new_machine_fields": Labels.NewMachine.fields, "install_steps": installSteps,
                                "about": [ui(.aboutOrigami), origamiVersionLine(version: Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "")],
                                "dialog_titles": [ui(.newMachine), ui(.settings), ui(.diagnostics), ui(.installIrix)],
                                "toolbar_tooltips": [ui(.showOrHideTheEmulated), ui(.openTheMainMenu)]]
    if let json = try? JSONSerialization.data(withJSONObject: names, options: [.prettyPrinted, .sortedKeys]) {
        try? json.write(to: URL(fileURLWithPath: "\(out)/macos-parity.json"))
    }
    emit(["step": "docs-shots", "ok": true, "out": out])
    exit(0)
}

/// The commands in each menu, as the app's command table lists them: each one once.
struct MenuSummary: View {
    var body: some View {
        HStack(alignment: .top, spacing: 24) {
            ForEach(CommandID.Menu.allCases, id: \.rawValue) { menu in
                VStack(alignment: .leading, spacing: 4) {
                    Text(menu.rawValue).font(.headline)
                    ForEach(CommandID.commands(in: menu), id: \.rawValue) { c in
                        HStack(spacing: 6) {
                            Text(c.title)
                            Text(c.tooltip.flatMap { $0.hasSuffix(")") ? $0.components(separatedBy: " (").last : nil } ?? "").foregroundStyle(.secondary).font(.caption)
                        }
                    }
                }
            }
        }
        .padding(16)
    }
}
