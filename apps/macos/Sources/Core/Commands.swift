// Every menu command as data: ID, title and shortcut. Menus are built from
// this table and tests invoke commands by ID.

import SwiftUI

enum CommandID: String, CaseIterable {
    case newMachine, chooseLibrary, closeTab
    case start, powerOff, forceQuit, reset, pause, resume, screenshot
    case captureInput, followMouse, pasteAsTyping
    case console, diagnostics, library, settings, showInFinder
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
        case .newMachine: return Info(title: "New Machine…", menu: .file, key: "n", modifiers: .command, needsCore: true)
        case .chooseLibrary: return Info(title: "Choose Library Folder…", menu: .file, key: "o", modifiers: .command)
        case .closeTab: return Info(title: "Close Tab", menu: .file, key: nil, modifiers: [])
        case .start: return Info(title: "Start", menu: .machine, key: "r", modifiers: .command)
        case .powerOff: return Info(title: "Power Off", menu: .machine, key: "p", modifiers: [.command, .option])
        case .forceQuit: return Info(title: "Force Quit Emulator…", menu: .machine, key: nil, modifiers: [])
        case .reset: return Info(title: "Reset…", menu: .machine, key: nil, modifiers: [])
        case .pause: return Info(title: "Pause", menu: .machine, key: nil, modifiers: [])
        case .resume: return Info(title: "Resume", menu: .machine, key: nil, modifiers: [])
        case .screenshot: return Info(title: "Save Screenshot", menu: .machine, key: "s", modifiers: [.command, .shift])
        case .captureInput: return Info(title: "Capture Input", menu: .machine, key: nil, modifiers: [])
        case .followMouse: return Info(title: "Follow My Mouse", menu: .machine, key: nil, modifiers: [])
        case .pasteAsTyping: return Info(title: "Paste as Typing", menu: .machine, key: "v", modifiers: [.command, .option], needsCore: true)
        case .settings: return Info(title: "Settings…", menu: .machine, key: "i", modifiers: .command)
        case .showInFinder: return Info(title: "Show in Finder", menu: .machine, key: nil, modifiers: [])
        case .showLibrary: return Info(title: "Show Library", menu: .view, key: "s", modifiers: [.control, .command])
        case .fit: return Info(title: "Fit to Window", menu: .view, key: "9", modifiers: .command)
        case .actualSize: return Info(title: "Actual Size", menu: .view, key: "0", modifiers: .command)
        case .pointScale: return Info(title: "Scale 1×", menu: .view, key: "1", modifiers: [.command, .option])
        case .doubleScale: return Info(title: "Scale 2×", menu: .view, key: "2", modifiers: [.command, .option])
        case .console: return Info(title: "Console", menu: .window, key: "c", modifiers: [.command, .option], needsCore: true)
        case .diagnostics: return Info(title: "Diagnostics", menu: .window, key: "l", modifiers: [.command, .option])
        case .library: return Info(title: "Library", menu: .window, key: "l", modifiers: [.command, .shift])
        case .nextTab: return Info(title: "Show Next Tab", menu: .window, key: "]", modifiers: [.command, .shift])
        case .previousTab: return Info(title: "Show Previous Tab", menu: .window, key: "[", modifiers: [.command, .shift])
        }
    }

    var title: String { info.title }
    static func commands(in menu: Menu) -> [CommandID] { allCases.filter { $0.info.menu == menu } }
}
