// The SwiftUI app: scenes, menus from the command table, and what quitting
// does to running machines.

import OrigamiFrame
import SwiftUI

/// Where the library comes from. Set once, before the app starts.
enum Launch {
    /// The library folder: `--machines DIR`, `ORIGAMI_MACHINES`, the saved choice, then the default.
    static func root(arguments: [String]) -> URL {
        var it = arguments.makeIterator()
        while let a = it.next() {
            if a == "--machines", let d = it.next() { return URL(fileURLWithPath: (d as NSString).expandingTildeInPath) }
        }
        if let env = ProcessInfo.processInfo.environment["ORIGAMI_MACHINES"] {
            return URL(fileURLWithPath: (env as NSString).expandingTildeInPath)
        }
        if let saved = UserDefaults.standard.string(forKey: "libraryRoot") { return URL(fileURLWithPath: saved) }
        // A build can name the folder its machines are in (OrigamiDefaultLibrary in Info.plist).
        if let named = Bundle.main.object(forInfoDictionaryKey: "OrigamiDefaultLibrary") as? String {
            return URL(fileURLWithPath: (named as NSString).expandingTildeInPath)
        }
        return Library.defaultRoot()
    }
}

@MainActor
enum Shared {
    static var model: AppModel!
}

/// A small window with a spinner and a line of text, for a quit that takes a while.
@MainActor
final class QuitProgress {
    private let panel = NSPanel(contentRect: NSRect(x: 0, y: 0, width: 360, height: 90),
                                styleMask: [.titled], backing: .buffered, defer: false)
    private let label = NSTextField(labelWithString: "")
    private let spinner = NSProgressIndicator()

    init() {
        panel.title = "Origami"
        spinner.style = .spinning; spinner.controlSize = .small; spinner.startAnimation(nil)
        spinner.frame = NSRect(x: 20, y: 36, width: 20, height: 20)
        label.frame = NSRect(x: 52, y: 36, width: 290, height: 20)
        panel.contentView?.addSubview(spinner)
        panel.contentView?.addSubview(label)
    }
    func show(_ text: String) { label.stringValue = text; if !panel.isVisible { panel.center(); panel.orderFront(nil) } }
    func hide() { panel.orderOut(nil) }
}

final class AppDelegate: NSObject, NSApplicationDelegate {
    private var progress: QuitProgress?

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool { true }

    func applicationWillTerminate(_ notification: Notification) {
        MainActor.assumeIsolated { Shared.model.host?.shutdown() }
    }

    @MainActor
    func applicationShouldTerminate(_ sender: NSApplication) -> NSApplication.TerminateReply {
        let app = Shared.model!
        // One prompt at most, naming only machines that are up now; none up quits silently.
        guard case .ask(let running) = app.quitPlan() else { return .terminateNow }
        let names = running.joined(separator: ", ")
        let alert = NSAlert()
        alert.messageText = running.count == 1 ? "\(names) is still running" : "\(running.count) machines are still running"
        alert.informativeText = running.count == 1
            ? "Keep it running to find it again the next time Origami opens, or power it off now."
            : "Keep them running to find them again the next time Origami opens, or power them off now. (\(names))"
        alert.addButton(withTitle: "Keep Running")
        alert.addButton(withTitle: "Power Off")
        alert.addButton(withTitle: "Cancel")
        switch alert.runModal() {
        case .alertFirstButtonReturn: return .terminateNow
        case .alertSecondButtonReturn:
            progress = QuitProgress()
            powerOffThenQuit(app, force: false)
            return .terminateLater
        default: return .terminateCancel
        }
    }

    /// Deferred so it cannot run before `.terminateLater` has been returned.
    private static func reply(_ quit: Bool) {
        DispatchQueue.main.async { NSApp.reply(toApplicationShouldTerminate: quit) }
    }

    /// Never quits while a machine is still up unless the person says so.
    @MainActor
    private func powerOffThenQuit(_ app: AppModel, force: Bool) {
        app.stopAll(force: force, progress: { [weak self] in self?.progress?.show($0) }) { [weak self] failed in
            self?.progress?.hide()
            if failed.isEmpty { AppDelegate.reply(true); return }
            let names = failed.map(\.name).joined(separator: ", ")
            let alert = NSAlert()
            alert.messageText = force ? "\(names) would not stop" : "\(names) did not power off"
            alert.informativeText = force ? "The emulator is still running."
                : "Force Quit stops the emulator at once; changes the guest has not written to disk are lost."
            if !force { alert.addButton(withTitle: "Force Quit Instead") }
            alert.addButton(withTitle: "Quit and Leave Running")
            alert.addButton(withTitle: "Cancel Quit")
            let r = alert.runModal().rawValue - NSApplication.ModalResponse.alertFirstButtonReturn.rawValue
            let choice = force ? r + 1 : r
            switch choice {
            case 0: self?.powerOffThenQuit(app, force: true)
            case 1: AppDelegate.reply(true)
            default: AppDelegate.reply(false)
            }
        }
    }
}

@MainActor
struct OrigamiCommands: Commands {
    @ObservedObject var app: AppModel
    @Environment(\.openWindow) private var openWindow
    @ViewBuilder private func item(_ c: CommandID) -> some View {
        let enabled = app.isEnabled(c)
        switch c {
        case .followMouse:
            Toggle(c.title, isOn: Binding(get: { app.current?.pointerMode == .follow }, set: { _ in app.invoke(c) })).disabled(!enabled)
        case .showLibrary:
            Button(app.sidebarVisible ? "Hide Library" : "Show Library") { app.invoke(c) }
                .keyboardShortcut("s", modifiers: [.control, .command])
        default:
            if let key = c.info.key {
                Button(c.title) { app.invoke(c) }.keyboardShortcut(key, modifiers: c.info.modifiers).disabled(!enabled)
            } else {
                Button(c.title) { app.invoke(c) }.disabled(!enabled)
            }
        }
    }

    var body: some Commands {
        CommandGroup(replacing: .appInfo) {
            Button("About Origami") { openWindow(id: "about") }
        }
        CommandGroup(replacing: .newItem) {
            item(.newMachine); item(.chooseLibrary)
        }
        CommandMenu("Machine") {
            item(.start); item(.powerOff); item(.forceQuit); item(.reset)
            Divider()
            item(.captureInput); item(.followMouse); item(.pasteAsTyping)
            Divider()
            item(.settings); item(.showInFinder)
        }
        CommandGroup(replacing: .sidebar) {
            item(.showLibrary)
            Divider()
            item(.pointScale); item(.doubleScale); item(.fit); item(.actualSize)
        }
        CommandGroup(after: .windowArrangement) {
            item(.console); item(.diagnostics); item(.library)
            Divider()
            item(.nextTab); item(.previousTab); item(.closeTab)
        }
    }
}

struct OrigamiApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var delegate
    @StateObject private var app = Shared.model

    var body: some Scene {
        Window("Origami", id: "main") {
            MainView()
                .environmentObject(app)
        }
        .defaultSize(width: 1100, height: 760)
        .commands { OrigamiCommands(app: app) }

        Window("Diagnostics", id: "diagnostics") {
            DiagnosticsView().environmentObject(app)
        }
        .defaultSize(width: 640, height: 400)

        Window("About Origami", id: "about") {
            AboutView().environmentObject(app)
        }
        .windowResizability(.contentSize)

        Settings { PreferencesView().environmentObject(app) }
    }
}
