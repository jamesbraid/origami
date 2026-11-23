// Every menu command as data: ID, title and shortcut. Menus are built from
// this table and tests invoke commands by ID.

import SwiftUI

enum CommandID: String, CaseIterable {
    case newMachine, chooseLibrary, closeTab
    case start, powerOff, forceQuit, reset, pause, resume, screenshot
    case captureInput, followMouse, pasteAsTyping
    case console, diagnostics, library, settings, installIrix, showInFinder
    case showLibrary, fit, actualSize, pointScale, doubleScale
    case nextTab, previousTab

    enum Menu: String { case file = "File", machine = "Machine", view = "View", window = "Window" }

    struct Info {
        let title: String
        let menu: Menu
        let key: KeyEquivalent?
        let modifiers: EventModifiers
        /// Shown in the menu now but not yet wired to a core that can do it.
        var needsCore = false
    }

    var info: Info {
        switch self {
        case .newMachine: return Info(title: "New machine…", menu: .file, key: "n", modifiers: .command)
        case .chooseLibrary: return Info(title: "Choose library folder…", menu: .file, key: "o", modifiers: .command)
        case .closeTab: return Info(title: "Close tab", menu: .file, key: nil, modifiers: [])
        case .start: return Info(title: "Start", menu: .machine, key: "r", modifiers: .command)
        case .powerOff: return Info(title: "Power off", menu: .machine, key: "p", modifiers: [.command, .option])
        case .forceQuit: return Info(title: "Force quit emulator…", menu: .machine, key: nil, modifiers: [])
        case .reset: return Info(title: "Reset…", menu: .machine, key: nil, modifiers: [])
        case .pause: return Info(title: "Pause", menu: .machine, key: nil, modifiers: [])
        case .resume: return Info(title: "Resume", menu: .machine, key: nil, modifiers: [])
        case .screenshot: return Info(title: "Save screenshot", menu: .machine, key: "s", modifiers: [.command, .shift])
        case .captureInput: return Info(title: "Capture input", menu: .machine, key: nil, modifiers: [])
        case .followMouse: return Info(title: "Follow my mouse", menu: .machine, key: nil, modifiers: [])
        case .pasteAsTyping: return Info(title: "Paste as typing", menu: .machine, key: "v", modifiers: [.command, .option], needsCore: true)
        case .settings: return Info(title: "Settings…", menu: .machine, key: "i", modifiers: .command)
        case .installIrix: return Info(title: "Install IRIX…", menu: .machine, key: nil, modifiers: [])
        case .showInFinder: return Info(title: "Show in Finder", menu: .machine, key: nil, modifiers: [])
        case .showLibrary: return Info(title: "Show library", menu: .view, key: "s", modifiers: [.control, .command])
        case .fit: return Info(title: "Fit to window", menu: .view, key: "9", modifiers: .command)
        case .actualSize: return Info(title: "Actual size", menu: .view, key: "0", modifiers: .command)
        case .pointScale: return Info(title: "Scale 1×", menu: .view, key: "1", modifiers: [.command, .option])
        case .doubleScale: return Info(title: "Scale 2×", menu: .view, key: "2", modifiers: [.command, .option])
        case .console: return Info(title: "Console", menu: .window, key: "c", modifiers: [.command, .option])
        case .diagnostics: return Info(title: "Diagnostics", menu: .window, key: "l", modifiers: [.command, .option])
        case .library: return Info(title: "Library", menu: .window, key: "l", modifiers: [.command, .shift])
        case .nextTab: return Info(title: "Show next tab", menu: .window, key: "]", modifiers: [.command, .shift])
        case .previousTab: return Info(title: "Show previous tab", menu: .window, key: "[", modifiers: [.command, .shift])
        }
    }

    var title: String { info.title }
    static func commands(in menu: Menu) -> [CommandID] { allCases.filter { $0.info.menu == menu } }
}
