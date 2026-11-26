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

    /// The app draws its own machine tabs, so the system's window tabs and their menu items are off.
    func applicationWillFinishLaunching(_ notification: Notification) { NSWindow.allowsAutomaticWindowTabbing = false }

    func applicationWillTerminate(_ notification: Notification) {
        MainActor.assumeIsolated { Shared.model.host?.shutdown() }
    }

    @MainActor
    func applicationShouldTerminate(_ sender: NSApplication) -> NSApplication.TerminateReply {
        let app = Shared.model!
        if app.installServing {
            guard app.confirmStopInstall(true) else { return .terminateCancel }
            app.stopInstalls { [weak self] in self?.planQuit(app) }
            return .terminateLater
        }
        planQuit(app)
        return .terminateLater
    }

    @MainActor
    private func planQuit(_ app: AppModel) {
        // One prompt at most, naming only machines that are up now; none up quits silently. The core
        // is asked off the main thread, so the answer comes later.
        app.quitPlan { [weak self] plan in
            guard case .ask(let running) = plan else { AppDelegate.reply(true); return }
            self?.askAboutRunning(running, app)
        }
    }

    @MainActor
    private func askAboutRunning(_ running: [String], _ app: AppModel) {
        let alert = origamiAlert(kind: .quitRunning(names: running), platform: thisPlatform)
        let i = app.present(alert)
        guard alert.buttons.indices.contains(i) else { AppDelegate.reply(false); return }
        switch alert.buttons[i].role {
        case .safe: AppDelegate.reply(true)
        case .destructive:
            progress = QuitProgress()
            powerOffThenQuit(app, force: false)
        case .cancel: AppDelegate.reply(false)
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
            // Force quit is the only way on; Cancel leaves the machines running and the app open.
            if app.confirmed(.powerOffFailed(names: failed.map(\.name))) { self?.powerOffThenQuit(app, force: true) }
            else { AppDelegate.reply(false) }
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
            Button(app.sidebarVisible ? "Hide library" : "Show library") { app.invoke(c) }
                .keyboardShortcut("s", modifiers: [.control, .command])
        case .pause:
            // One item that changes its name, as the toolbar's Power menu does.
            let paused = app.current?.state == .paused
            Button(paused ? CommandID.resume.title : c.title) { app.invoke(paused ? .resume : .pause) }
                .keyboardShortcut("p", modifiers: .command).disabled(!app.isEnabled(paused ? .resume : .pause))
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
            Button(ui(.aboutOrigami)) { openWindow(id: "about") }
        }
        CommandGroup(replacing: .newItem) {
            item(.newMachine); item(.chooseLibrary)
        }
        // Close is the machine's: the standard window Close would end the app with its last window.
        CommandGroup(replacing: .saveItem) { }
        CommandMenu(ui(.machine)) {
            item(.start); item(.cancelStart); item(.powerOff); item(.forceQuit); item(.reset)
            Divider()
            item(.pause); item(.screenshot)
            Divider()
            item(.captureInput); item(.followMouse); item(.pasteAsTyping)
            Divider()
            item(.settings); item(.installIrix); item(.showInFinder)
            Divider()
            item(.remove)
        }
        CommandGroup(replacing: .sidebar) {
            item(.showLibrary)
            Divider()
            item(.pointScale); item(.doubleScale); item(.fit); item(.actualSize)
            Divider()
            item(.cpuLoad)
        }
        CommandGroup(after: .windowArrangement) {
            item(.console); item(.diagnostics); item(.library)
            Divider()
            item(.nextTab); item(.previousTab); item(.closeTab)
        }
        CommandGroup(replacing: .help) { item(.keyboardShortcuts) }
    }
}

struct OrigamiApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var delegate
    @StateObject private var app = Shared.model

    var body: some Scene {
        Window(ui(.origami), id: "main") {
            MainView()
                .environmentObject(app)
        }
        .defaultSize(width: 1100, height: 760)
        .commands { OrigamiCommands(app: app) }

        Window(ui(.diagnostics), id: "diagnostics") {
            DiagnosticsView().environmentObject(app)
        }
        .defaultSize(width: 640, height: 400)
        .commandsRemoved()

        WindowGroup(ui(.console), id: "console", for: String.self) { $key in
            if let key { ConsoleWindow(key: key).environmentObject(app) }
        }
        .defaultSize(width: 700, height: 440)
        .commandsRemoved()

        Window(ui(.keyboardShortcuts), id: "shortcuts") { ShortcutsView() }
            .windowResizability(.contentSize)
            .commandsRemoved()

        Window(ui(.aboutOrigami), id: "about") {
            AboutView().environmentObject(app)
        }
        .windowResizability(.contentSize)
        .commandsRemoved()

        Settings { PreferencesView().environmentObject(app) }
    }
}
