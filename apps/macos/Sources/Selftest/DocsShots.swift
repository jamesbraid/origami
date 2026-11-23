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

    func window<V: View>(_ title: String, _ view: V, _ w: CGFloat, _ h: CGFloat, fit: Bool = false) -> NSWindow {
        let controller = NSHostingController(rootView: view.environmentObject(app))
        let win = NSWindow(contentViewController: controller)
        win.styleMask = [.titled, .closable, .resizable]
        win.title = title
        win.isReleasedWhenClosed = false
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

    // New Machine, with the model picked and a name typed, before the machine exists.
    capture(window("New machine", NewMachineSheet(model: nm), 460, 340), "macos-new-machine", after: choose)

    // The same sheet with a ready-made disk picked: the list comes from origami-dist.
    func chooseReady() {
        choose()
        guard waitFor(60, { !nm.readyDisks.isEmpty }) else { fail("origami-dist listed no ready-made disk for \(nm.name)") }
        nm.disk = .ready
    }
    capture(window("New machine", NewMachineSheet(model: nm), 460, 400), "macos-ready-disk", after: chooseReady)
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
        installSteps = install.flow.steps().map { origamiInstallStepTitle(step: $0) }
        capture(window("Install IRIX: \(model.name)", InstallAssistant(model: install), 780, 540), "macos-install-assistant")
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
    guard waitFor(120, { model.state == .running }) else { fail("the machine did not reach Running (\(model.state.pill))") }
    // The PROM takes a while to draw its menu.
    spin(90)
    capture(main, "macos-running", settle: 3, hide: false, after: { focusLibrary(main) })
    laidOverPicture(main, model, "macos-running")
    app.showsCpu = true
    capture(main, "macos-cpu-panel", settle: 4, hide: false, after: { focusLibrary(main) })
    laidOverPicture(main, model, "macos-cpu-panel")
    app.showsCpu = false
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
    // The names the other apps must match: tools/screenshots parity compares these files.
    let names: [String: Any] = ["platform": "macos", "sidebar_heading": Labels.sidebarHeading,
                                "settings_tabs": MachineSettings.Pane.allCases.map(\.rawValue),
                                "new_machine_fields": Labels.NewMachine.fields, "install_steps": installSteps]
    if let json = try? JSONSerialization.data(withJSONObject: names, options: [.prettyPrinted, .sortedKeys]) {
        try? json.write(to: URL(fileURLWithPath: "\(out)/macos-parity.json"))
    }
    emit(["step": "docs-shots", "ok": true, "out": out])
    exit(0)
}
