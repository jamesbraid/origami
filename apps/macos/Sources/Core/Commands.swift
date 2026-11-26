// Every menu command as data: ID, title and shortcut. Menus are built from
// this table and tests invoke commands by ID. Titles and tooltips come from the
// core's command table by `coreID`; the rest are the app's own.

import SwiftUI

enum CommandID: String, CaseIterable {
    case newMachine, chooseLibrary, closeTab
    case start, powerOff, forceQuit, reset, pause, resume, screenshot
    case captureInput, followMouse, pasteAsTyping
    case console, diagnostics, library, settings, installIrix, showInFinder, remove, cancelStart, cpuLoad
    case showLibrary, fit, actualSize, pointScale, doubleScale
    case nextTab, previousTab, keyboardShortcuts

    enum Menu: String, CaseIterable { case file = "File", machine = "Machine", view = "View", window = "Window", help = "Help" }

    struct Info {
        let title: String
        let menu: Menu
        let key: KeyEquivalent?
        let modifiers: EventModifiers
        /// Shown in the menu now but not yet wired to a core that can do it.
        var needsCore = false
    }

    /// The command's id in the core's table, where the core has one.
    var coreID: String? {
        switch self {
        case .newMachine: return "machine.new"
        case .start: return "machine.start"
        case .powerOff: return "machine.stop"
        case .forceQuit: return "machine.force-quit"
        case .reset: return "machine.reset"
        case .pause: return "machine.pause"
        case .resume: return "machine.resume"
        case .screenshot: return "machine.screenshot"
        case .captureInput: return "machine.capture-mouse"
        case .console: return "window.console"
        case .diagnostics: return "window.diagnostics"
        case .settings: return "machine.settings"
        case .installIrix: return "machine.install-irix"
        case .remove: return "machine.remove"
        case .cancelStart: return "machine.cancel-start"
        case .cpuLoad: return "view.cpu-load"
        case .nextTab: return "window.next-machine"
        case .closeTab: return "window.close-machine"
        case .keyboardShortcuts: return "view.shortcuts"
        default: return nil
        }
    }

    var info: Info {
        switch self {
        case .newMachine: return Info(title: "New machine…", menu: .file, key: "n", modifiers: .command)
        case .chooseLibrary: return Info(title: "Choose library folder…", menu: .file, key: "o", modifiers: .command)
        case .closeTab: return Info(title: "Close machine", menu: .window, key: "w", modifiers: .command)
        case .start: return Info(title: "Start", menu: .machine, key: "r", modifiers: .command)
        case .powerOff: return Info(title: "Power off", menu: .machine, key: "p", modifiers: [.command, .option])
        case .forceQuit: return Info(title: "Force quit", menu: .machine, key: nil, modifiers: [])
        case .reset: return Info(title: "Reset", menu: .machine, key: nil, modifiers: [])
        case .pause: return Info(title: "Pause", menu: .machine, key: "p", modifiers: .command)
        case .resume: return Info(title: "Resume", menu: .machine, key: nil, modifiers: [])
        case .screenshot: return Info(title: "Screenshot", menu: .machine, key: "s", modifiers: [.command, .shift])
        case .captureInput: return Info(title: "Capture mouse", menu: .machine, key: nil, modifiers: [])
        case .followMouse: return Info(title: "Follow my mouse", menu: .machine, key: nil, modifiers: [])
        case .pasteAsTyping: return Info(title: "Paste as typing", menu: .machine, key: "v", modifiers: [.command, .option], needsCore: true)
        case .settings: return Info(title: "Settings", menu: .machine, key: "i", modifiers: .command)
        case .installIrix: return Info(title: "Install IRIX…", menu: .machine, key: nil, modifiers: [])
        case .showInFinder: return Info(title: origamiShowInLabel(platform: thisPlatform), menu: .machine, key: nil, modifiers: [])
        case .remove: return Info(title: "Move to Trash…", menu: .machine, key: .delete, modifiers: .command)
        case .cancelStart: return Info(title: "Cancel start", menu: .machine, key: nil, modifiers: [])
        case .cpuLoad: return Info(title: "CPU load", menu: .view, key: nil, modifiers: [])
        case .showLibrary: return Info(title: "Show library", menu: .view, key: "s", modifiers: [.control, .command])
        case .fit: return Info(title: "Fit to window", menu: .view, key: "9", modifiers: .command)
        case .actualSize: return Info(title: "Actual size", menu: .view, key: "0", modifiers: .command)
        case .pointScale: return Info(title: "Scale 1×", menu: .view, key: "1", modifiers: [.command, .option])
        case .doubleScale: return Info(title: "Scale 2×", menu: .view, key: "2", modifiers: [.command, .option])
        case .console: return Info(title: "Console", menu: .window, key: "c", modifiers: [.command, .option])
        case .diagnostics: return Info(title: "Diagnostics", menu: .window, key: "l", modifiers: [.command, .option])
        case .library: return Info(title: "Library", menu: .window, key: "l", modifiers: [.command, .shift])
        case .nextTab: return Info(title: "Show next machine", menu: .window, key: .tab, modifiers: .control)
        case .previousTab: return Info(title: "Show previous machine", menu: .window, key: .tab, modifiers: [.control, .shift])
        case .keyboardShortcuts: return Info(title: "Keyboard shortcuts", menu: .help, key: nil, modifiers: [])
        }
    }

    /// The core's title where it has one.
    var title: String { CoreCommands.byID[coreID ?? ""]?.title ?? info.title }
    /// The core's tooltip, ending with the shortcut.
    var tooltip: String? { coreID.flatMap { origamiCommandTooltip(id: $0, platform: thisPlatform) } }
    static func commands(in menu: Menu) -> [CommandID] { allCases.filter { $0.info.menu == menu } }
}

/// The core's command table, read once the core is up.
enum CoreCommands {
    static var byID: [String: OrigamiCommand] = [:]
    static func load(_ core: OrigamiCore) { byID = Dictionary(core.commands().map { ($0.id, $0) }, uniquingKeysWith: { a, _ in a }) }
}
