// A machine's serial console: the core's terminal model on the machine's console, plus the
// damage bookkeeping the view needs to redraw only what changed.

import AppKit

@MainActor
final class ConsoleModel {
    let machine: MachineModel
    private(set) var terminal: OrigamiTerminal?
    private var seen: UInt64 = 0
    /// Why there is no terminal, or why the last input was not taken.
    private(set) var error: String?
    /// The grid the terminal was first made at. The model replays the console's history at that size and
    /// does not reflow it, so it must be the view's real size.
    private(set) var firstAttach: (rows: Int, cols: Int)?

    init(machine: MachineModel) { self.machine = machine }

    /// Make the terminal on first use, at the given grid size, and resize it afterwards.
    func attach(rows: Int, cols: Int) {
        let r = UInt16(max(1, min(256, rows))), c = UInt16(max(1, min(512, cols)))
        if let terminal { terminal.resize(rows: r, cols: c); return }
        guard let session = machine.session else { error = "The machine is not running."; return }
        do {
            terminal = try OrigamiTerminal(machine: session.machine, rows: r, cols: c)
            firstAttach = (Int(r), Int(c))
            error = nil
        } catch let e { error = "\(e)" }
    }

    /// A view showing this console begins and ends; the terminal goes when the last one does.
    private var users = 0
    func begin() { users += 1 }
    func end() { users = max(0, users - 1); if users == 0 { detach() } }

    func detach() { terminal = nil; seen = 0 }

    /// The rows to redraw since the last call, or nil for all of them.
    func damage() -> [UInt16]? {
        guard let terminal else { return [] }
        let d = terminal.changedSince(since: seen)
        seen = d.sequence
        return d.full ? nil : d.rows
    }

    /// Show the next draw in full, as after a resize that the model did not report.
    func redrawAll() { seen = 0 }

    /// Input goes to the core from a serial queue, so a slow core call never holds the main thread and
    /// keys still arrive in order.
    private let sender = DispatchQueue(label: "origami.console.input")

    func send(_ key: OrigamiTerminalKey, shift: Bool = false, ctrl: Bool = false, alt: Bool = false) {
        guard let terminal else { return }
        sender.async { [weak self] in
            do { try terminal.sendKeyWith(key: key, shift: shift, ctrl: ctrl, alt: alt) } catch { self?.inputFailed(error) }
        }
    }

    func paste(_ text: String) {
        guard let terminal else { return }
        sender.async { [weak self] in
            do { try terminal.paste(text: text) } catch { self?.inputFailed(error) }
        }
    }

    private nonisolated func inputFailed(_ e: Error) {
        DispatchQueue.main.async { [weak self] in MainActor.assumeIsolated { self?.error = "\(e)" } }
    }

    /// Wait until the input sent so far has reached the core; for the self-test.
    func drainInput() { sender.sync {} }
}

/// What a Mac key press sends a serial terminal, or nil for none.
enum ConsoleKeys {
    /// The Option-as-Meta setting: off by default, so Option still types the Mac's own characters.
    static let optionAsMetaKey = "consoleOptionAsMeta"

    struct Mapped: Equatable {
        let key: OrigamiTerminalKey
        var shift = false, ctrl = false, alt = false
    }

    static func key(keyCode: UInt16, chars: String, ignoring: String, flags: NSEvent.ModifierFlags,
                    optionAsMeta: Bool) -> Mapped? {
        // Command chords (Cmd-arrow, Cmd-Backspace, Cmd-C) belong to the app.
        if flags.contains(.command) { return nil }
        let special: OrigamiTerminalKey?
        switch keyCode {
        case 126: special = .up
        case 125: special = .down
        case 124: special = .right
        case 123: special = .left
        case 115: special = .home
        case 119: special = .end
        case 116: special = .pageUp
        case 121: special = .pageDown
        case 114: special = .insert
        case 117: special = .delete
        case 122: special = .function(number: 1)
        case 120: special = .function(number: 2)
        case 99: special = .function(number: 3)
        case 118: special = .function(number: 4)
        case 96: special = .function(number: 5)
        case 97: special = .function(number: 6)
        case 98: special = .function(number: 7)
        case 100: special = .function(number: 8)
        case 101: special = .function(number: 9)
        case 109: special = .function(number: 10)
        case 103: special = .function(number: 11)
        case 111: special = .function(number: 12)
        case 36, 76: special = .enter
        case 51: special = .backspace
        case 48: special = flags.contains(.shift) ? .backTab : .tab
        case 53: special = .escape
        default: special = nil
        }
        let alt = flags.contains(.option) && optionAsMeta
        // The core turns Shift, Control and Meta on these keys into xterm's modified forms.
        let shift = flags.contains(.shift)
        if let special { return Mapped(key: special, shift: shift && special != .backTab, ctrl: flags.contains(.control), alt: alt) }
        let ctrl = flags.contains(.control)
        if ctrl || alt {
            // Control and Meta take one bare key.
            guard let scalar = ignoring.unicodeScalars.first, scalar.value >= 0x20, scalar.value != 0x7f else { return nil }
            return Mapped(key: .text(text: String(scalar)), ctrl: ctrl, alt: alt)
        }
        // Otherwise everything typed, with Option's composed characters, minus control and function-key codes.
        let text = String(String.UnicodeScalarView(chars.unicodeScalars.filter {
            $0.value >= 0x20 && $0.value != 0x7f && !(0xF700...0xF8FF).contains($0.value)
        }))
        return text.isEmpty ? nil : Mapped(key: .text(text: text))
    }
}
