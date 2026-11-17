// The Console window's terminal: draws the core's terminal model one dirty row at a time, takes keys,
// and supports selection, copy, paste and scrollback.

import AppKit
import SwiftUI

final class ConsoleNSView: NSView, NSUserInterfaceValidations {
    let console: ConsoleModel
    private let font = NSFont.monospacedSystemFont(ofSize: 13, weight: .regular)
    private let boldFont = NSFont.monospacedSystemFont(ofSize: 13, weight: .bold)
    private(set) var cell = CGSize(width: 8, height: 16)
    private var anchor: (row: Int, col: Int)?
    private var head: (row: Int, col: Int)?
    private var timer: Timer?
    private var began = false
    private var shownCursor: (row: Int, col: Int, on: Bool)?

    static let background = NSColor(srgbRed: 0.09, green: 0.1, blue: 0.12, alpha: 1)
    static let foreground = NSColor(white: 0.88, alpha: 1)

    init(console: ConsoleModel) {
        self.console = console
        super.init(frame: NSRect(x: 0, y: 0, width: 660, height: 400))
        let adv = ("M" as NSString).size(withAttributes: [.font: font]).width
        cell = CGSize(width: adv, height: ceil(font.ascender - font.descender + font.leading))
    }
    required init?(coder: NSCoder) { fatalError() }

    override var isFlipped: Bool { true }
    override var acceptsFirstResponder: Bool { true }
    override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }

    override func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
        timer?.invalidate(); timer = nil
        guard window != nil else { if began { began = false; console.end() }; return }
        if !began { began = true; console.begin() }
        window?.makeFirstResponder(self)
        layoutTerminal()
        let t = Timer(timeInterval: 1.0 / 30, repeats: true) { [weak self] _ in MainActor.assumeIsolated { self?.tick() } }
        RunLoop.main.add(t, forMode: .common)
        timer = t
    }
    override func setFrameSize(_ s: NSSize) { super.setFrameSize(s); layoutTerminal() }

    func stop() { timer?.invalidate(); timer = nil; if began { began = false; console.end() } }

    private func layoutTerminal() {
        // The terminal replays the console's history at its first size and does not reflow, so wait for a real one.
        guard bounds.width >= cell.width * 20, bounds.height >= cell.height * 4 else { return }
        console.attach(rows: Int(bounds.height / cell.height), cols: Int(bounds.width / cell.width))
        console.redrawAll()
        needsDisplay = true
    }

    private func rowRect(_ r: Int) -> NSRect { NSRect(x: 0, y: CGFloat(r) * cell.height, width: bounds.width, height: cell.height) }

    /// Invalidate only the rows the model says changed, and the cursor's old and new rows.
    @MainActor private func tick() {
        guard let t = console.terminal else {
            // The window was opened before the machine ran: attach when it does.
            if console.machine.state == .running { layoutTerminal() }
            return
        }
        let c = t.cursor()
        let now = (row: Int(c.row), col: Int(c.col), on: c.visible && t.scrollOffset() == 0 && window?.isKeyWindow == true)
        if let rows = console.damage() {
            for r in rows { setNeedsDisplay(rowRect(Int(r))) }
        } else {
            clearSelection(); needsDisplay = true
        }
        if shownCursor?.row != now.row || shownCursor?.col != now.col || shownCursor?.on != now.on {
            if let old = shownCursor { setNeedsDisplay(rowRect(old.row)) }
            setNeedsDisplay(rowRect(now.row))
            shownCursor = now
        }
    }

    // MARK: drawing

    override func draw(_ dirty: NSRect) {
        Self.background.setFill(); dirty.fill()
        guard let t = console.terminal else {
            // No terminal: say why instead of a blank window.
            let why = console.error ?? "The console is not available."
            let attrs: [NSAttributedString.Key: Any] = [.font: font, .foregroundColor: Self.foreground.withAlphaComponent(0.7)]
            let size = (why as NSString).size(withAttributes: attrs)
            (why as NSString).draw(at: NSPoint(x: max(8, (bounds.width - size.width) / 2), y: bounds.height / 2 - size.height / 2), withAttributes: attrs)
            return
        }
        let size = t.size()
        let first = max(0, Int(dirty.minY / cell.height)), last = min(Int(size.rows), Int(dirty.maxY / cell.height) + 1)
        guard first < last else { return }
        let sel = selection()
        for r in first..<last {
            guard let cells = try? t.row(index: UInt16(r)) else { continue }
            drawRow(r, cells, selected: sel)
        }
        if let c = shownCursor, c.on, c.row >= first, c.row < last {
            NSColor(white: 0.9, alpha: 0.6).setFill()
            NSRect(x: CGFloat(c.col) * cell.width, y: CGFloat(c.row) * cell.height, width: cell.width, height: cell.height).fill()
        }
    }

    /// One row: its backgrounds as runs of one fill each, its text as one attributed string.
    private func drawRow(_ r: Int, _ cells: [OrigamiTerminalCell], selected sel: ((row: Int, col: Int), (row: Int, col: Int))?) {
        let y = CGFloat(r) * cell.height
        let text = NSMutableAttributedString()
        var runStart = 0, runColour: NSColor?
        func flush(_ end: Int) {
            if let c = runColour { c.setFill(); NSRect(x: CGFloat(runStart) * cell.width, y: y, width: CGFloat(end - runStart) * cell.width, height: cell.height).fill() }
        }
        for (x, c) in cells.enumerated() {
            var fg = Self.colour(c.fg) ?? Self.foreground
            var bg = Self.colour(c.bg)
            if c.inverse { let f = fg; fg = bg ?? Self.background; bg = f }
            let isSelected = sel.map { (r, x) >= ($0.0.row, $0.0.col) && (r, x) <= ($0.1.row, $0.1.col) } ?? false
            if isSelected { bg = .selectedTextBackgroundColor; fg = .selectedTextColor }
            if c.dim { fg = fg.withAlphaComponent(0.6) }
            if bg != runColour {
                flush(x); runStart = x; runColour = bg
            }
            if c.continuation { continue }
            var attrs: [NSAttributedString.Key: Any] = [.font: c.bold ? boldFont : font, .foregroundColor: fg]
            if c.underline { attrs[.underlineStyle] = NSUnderlineStyle.single.rawValue }
            if c.italic { attrs[.obliqueness] = 0.2 }
            text.append(NSAttributedString(string: c.text.isEmpty ? " " : c.text, attributes: attrs))
        }
        flush(cells.count)
        text.draw(at: NSPoint(x: 0, y: y))
    }

    /// The xterm palette: 16 ANSI colours, a 6x6x6 cube, and a grey ramp.
    static func colour(_ c: OrigamiTerminalColour) -> NSColor? {
        switch c {
        case .default: return nil
        case .rgb(let r, let g, let b): return NSColor(srgbRed: CGFloat(r) / 255, green: CGFloat(g) / 255, blue: CGFloat(b) / 255, alpha: 1)
        case .indexed(let i): return palette(Int(i))
        }
    }

    static func palette(_ i: Int) -> NSColor {
        let ansi: [UInt32] = [0x000000, 0xcd3131, 0x0dbc79, 0xe5e510, 0x2472c8, 0xbc3fbc, 0x11a8cd, 0xe5e5e5,
                              0x666666, 0xf14c4c, 0x23d18b, 0xf5f543, 0x3b8eea, 0xd670d6, 0x29b8db, 0xffffff]
        func rgb(_ v: UInt32) -> NSColor {
            NSColor(srgbRed: CGFloat((v >> 16) & 255) / 255, green: CGFloat((v >> 8) & 255) / 255, blue: CGFloat(v & 255) / 255, alpha: 1)
        }
        if i < 16 { return rgb(ansi[i]) }
        if i < 232 {
            let n = i - 16
            func level(_ k: Int) -> CGFloat { k == 0 ? 0 : CGFloat(55 + 40 * k) / 255 }
            return NSColor(srgbRed: level(n / 36), green: level((n / 6) % 6), blue: level(n % 6), alpha: 1)
        }
        let g = CGFloat(8 + 10 * (i - 232)) / 255
        return NSColor(srgbRed: g, green: g, blue: g, alpha: 1)
    }

    // MARK: keys, paste, copy

    override func keyDown(with e: NSEvent) {
        let meta = UserDefaults.standard.bool(forKey: ConsoleKeys.optionAsMetaKey)
        if let k = ConsoleKeys.key(keyCode: e.keyCode, chars: e.characters ?? "", ignoring: e.charactersIgnoringModifiers ?? "",
                                   flags: e.modifierFlags, optionAsMeta: meta) {
            console.terminal?.scroll(lines: -Int32(console.terminal?.scrollOffset() ?? 0))
            clearSelection()
            console.send(k.key, shift: k.shift, ctrl: k.ctrl, alt: k.alt)
        } else { super.keyDown(with: e) }
    }

    @objc func paste(_ sender: Any?) {
        if let s = NSPasteboard.general.string(forType: .string) { console.paste(s) }
    }

    @objc func copy(_ sender: Any?) {
        guard let t = console.terminal, let (a, b) = selection() else { return }
        var lines: [String] = []
        for r in a.row...b.row {
            guard let cells = try? t.row(index: UInt16(r)) else { continue }
            let from = r == a.row ? a.col : 0, to = r == b.row ? b.col : cells.count - 1
            // A wide character's second cell is a continuation with no text of its own: no space for it.
            let slice = cells.enumerated().filter { $0.offset >= from && $0.offset <= to && !$0.element.continuation }
                .map { $0.element.text.isEmpty ? " " : $0.element.text }
            lines.append(slice.joined().replacingOccurrences(of: "\\s+$", with: "", options: .regularExpression))
        }
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(lines.joined(separator: "\n"), forType: .string)
    }

    override func selectAll(_ sender: Any?) {
        guard let s = console.terminal?.size() else { return }
        anchor = (0, 0); head = (Int(s.rows) - 1, Int(s.cols) - 1); needsDisplay = true
    }

    @objc func validateUserInterfaceItem(_ item: NSValidatedUserInterfaceItem) -> Bool {
        item.action == #selector(copy(_:)) ? selection() != nil : true
    }

    // MARK: selection and scrolling

    private func clearSelection() { if anchor != nil { anchor = nil; head = nil; needsDisplay = true } }

    private func selection() -> ((row: Int, col: Int), (row: Int, col: Int))? {
        guard let a = anchor, let h = head, a != h else { return nil }
        return (a.row, a.col) <= (h.row, h.col) ? (a, h) : (h, a)
    }

    private func position(_ e: NSEvent) -> (row: Int, col: Int) {
        let p = convert(e.locationInWindow, from: nil)
        let s = console.terminal?.size() ?? OrigamiTerminalSize(rows: 1, cols: 1)
        return (max(0, min(Int(s.rows) - 1, Int(p.y / cell.height))), max(0, min(Int(s.cols) - 1, Int(p.x / cell.width))))
    }

    override func mouseDown(with e: NSEvent) { window?.makeFirstResponder(self); anchor = position(e); head = anchor; needsDisplay = true }
    override func mouseDragged(with e: NSEvent) { head = position(e); needsDisplay = true }

    /// Trackpad deltas are fractions of a line; the part not yet a whole line carries to the next event.
    private var scrollRemainder: CGFloat = 0

    override func scrollWheel(with e: NSEvent) {
        if e.phase == .began { scrollRemainder = 0 }
        scrollRemainder += e.scrollingDeltaY / (e.hasPreciseScrollingDeltas ? cell.height : 1)
        let lines = Int(scrollRemainder.rounded(.towardZero))
        guard lines != 0 else { return }
        scrollRemainder -= CGFloat(lines)
        clearSelection()
        console.terminal?.scroll(lines: Int32(lines))
    }
}

struct ConsoleHost: NSViewRepresentable {
    let console: ConsoleModel
    func makeNSView(context: Context) -> ConsoleNSView { ConsoleNSView(console: console) }
    func updateNSView(_ v: ConsoleNSView, context: Context) {}
    static func dismantleNSView(_ v: ConsoleNSView, coordinator: ()) { v.stop() }
}

/// One Console window per machine, opened by the machine's key.
struct ConsoleWindow: View {
    @EnvironmentObject var app: AppModel
    let key: String

    var body: some View {
        if let model = app.models.first(where: { $0.id == key }) {
            ConsoleHost(console: model.console())
                .frame(minWidth: 480, minHeight: 240)
                .navigationTitle("\(model.name) Console")
        } else {
            Text("This machine is no longer in the library.").foregroundStyle(.secondary).frame(width: 360, height: 120)
        }
    }
}
