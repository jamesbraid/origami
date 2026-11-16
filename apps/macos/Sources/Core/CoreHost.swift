// The Rust core as the app sees it: an OrigamiCore (UniFFI) and, per opened
// machine, a MachineSession over the C frame plane. Only this file and
// MachineView.swift call the frame functions.

import AppKit
import OrigamiFrame

/// The core calls this on its own threads. It only forwards.
final class EventForwarder: OrigamiObserver, @unchecked Sendable {
    var handler: (@Sendable (OrigamiEvent) -> Void)?
    func onEvent(event: OrigamiEvent) { handler?(event) }
}

final class CoreHost: @unchecked Sendable {
    let core: OrigamiCore
    private let forwarder = EventForwarder()
    private let lock = NSLock()
    private var pending = Set<String>()
    private var libraryChanged = false
    private var queued = false
    /// Called on the main thread with the machine keys that changed, and
    /// whether the library did. Events coalesce: one call per burst.
    var onChange: (@MainActor (_ machines: Set<String>, _ library: Bool) -> Void)?

    init(libraryRoot: String) throws {
        core = try OrigamiCore(libraryRoot: libraryRoot, observer: forwarder)
        forwarder.handler = { [unowned self] event in self.receive(event) }
    }

    /// Core thread.
    private func receive(_ event: OrigamiEvent) {
        lock.lock()
        if let key = event.machine { pending.insert(key) } else { libraryChanged = true }
        if event.kinds.contains(.library) { libraryChanged = true }
        let post = !queued
        queued = true
        lock.unlock()
        if post { DispatchQueue.main.async { self.flush() } }
    }

    private func flush() {
        lock.lock()
        let keys = pending, library = libraryChanged
        pending = []; libraryChanged = false; queued = false
        lock.unlock()
        MainActor.assumeIsolated { onChange?(keys, library) }
    }

    func shutdown() { core.shutdown() }
}

/// What a head looks like to the app.
struct HeadInfo: Equatable {
    var width: Int, height: Int
    var active: Bool
    var seq: UInt64
}

/// One opened machine: the control-plane object and its frame-plane handle.
final class MachineSession: @unchecked Sendable {
    let machine: OrigamiMachine
    /// An owned `origami_machine*`; released with the session.
    let handle: OpaquePointer
    private let lock = NSLock()
    private var wakePosted = false
    private(set) var wakes = 0
    private var frameOwner: ObjectIdentifier?
    private var frameHandler: (() -> Void)?

    /// `handler` runs on the main thread after the core published a new picture.
    func setFrameHandler(owner: AnyObject, _ handler: @escaping () -> Void) {
        frameOwner = ObjectIdentifier(owner); frameHandler = handler
    }
    /// Only the view that set the handler can clear it, so a dismantled view
    /// cannot unhook its replacement.
    func clearFrameHandler(owner: AnyObject) {
        if frameOwner == ObjectIdentifier(owner) { frameOwner = nil; frameHandler = nil }
    }

    init(machine: OrigamiMachine) {
        self.machine = machine
        handle = OpaquePointer(bitPattern: UInt(machine.framePlaneHandle()))!
        // The user data is this session, unretained: it clears the callback in deinit.
        origami_frame_set_wake(handle, { user in
            guard let user else { return }
            Unmanaged<MachineSession>.fromOpaque(user).takeUnretainedValue().wake()
        }, Unmanaged.passUnretained(self).toOpaque())
    }

    deinit {
        origami_frame_set_wake(handle, nil, nil)
        origami_machine_release(handle)
    }

    /// Engine thread: coalesce into one main-thread notification.
    private func wake() {
        lock.lock()
        wakes += 1
        let post = !wakePosted
        wakePosted = true
        lock.unlock()
        if post { DispatchQueue.main.async { [weak self] in self?.deliver() } }
    }

    private func deliver() {
        lock.lock(); wakePosted = false; lock.unlock()
        frameHandler?()
    }

    /// The head as of the last state event. Input mapping reads this; asking
    /// the core (a UniFFI call) on every mouse move would cost more than it tells.
    private(set) var cachedHead: HeadInfo?
    func updateHead() { cachedHead = head }

    var head: HeadInfo? {
        guard let h = machine.view().heads.first else { return nil }
        return HeadInfo(width: Int(h.width), height: Int(h.height), active: h.active, seq: h.seq)
    }

    // MARK: input (the frame plane's batch call: no serialisation, never blocks)

    func send(_ event: origami_input_event) {
        var e = event
        origami_input_send(handle, &e, 1)
    }
    func key(_ code: UInt32, down: Bool) {
        send(origami_input_event(kind: UInt32(ORIGAMI_INPUT_KEY), code: code, down: down ? 1 : 0, head: 0, x: 0, y: 0))
    }
    func pointerAt(x: Int32, y: Int32) {
        send(origami_input_event(kind: UInt32(ORIGAMI_INPUT_POINTER_AT), code: 0, down: 0, head: 0, x: x, y: y))
    }
    func pointerMove(dx: Int32, dy: Int32) {
        send(origami_input_event(kind: UInt32(ORIGAMI_INPUT_POINTER_MOVE), code: 0, down: 0, head: 0, x: dx, y: dy))
    }
    func button(_ code: UInt32, down: Bool) {
        send(origami_input_event(kind: UInt32(ORIGAMI_INPUT_BUTTON), code: code, down: down ? 1 : 0, head: 0, x: 0, y: 0))
    }
    func wheel(_ notches: Int32) {
        send(origami_input_event(kind: UInt32(ORIGAMI_INPUT_WHEEL), code: 0, down: 0, head: 0, x: notches, y: 0))
    }
    /// Lift every key and button the guest believes is down.
    @discardableResult
    func releaseAll() -> UInt32 {
        var n: UInt32 = 0
        origami_input_release_all(handle, &n)
        return n
    }

    // MARK: copy path (tests and the Diagnostics counters; drawing goes through the view)

    /// The changed rows of head 0 copied out, or nil when nothing changed.
    func takeCopy(into pixels: inout [UInt8]) -> (frame: origami_frame, runs: [origami_run])? {
        guard let h = head else { return nil }
        let need = h.width * h.height * 4
        if pixels.count < need { pixels = [UInt8](repeating: 0, count: need) }
        var runs = [origami_run](repeating: origami_run(y0: 0, y1: 0), count: max(1, h.height))
        var out = origami_frame()
        out.size = UInt32(MemoryLayout<origami_frame>.size)
        let pixelCap = pixels.count, runCap = runs.count
        let status = origami_frame_take(handle, 0, &pixels, pixelCap, &runs, runCap, &out)
        guard status == ORIGAMI_OK, out.kind == UInt32(ORIGAMI_FRAME_ROWS) else { return nil }
        return (out, Array(runs.prefix(Int(out.run_count))))
    }
}

/// The frame plane's key positions in the header's order (`origami_key`).
enum Key: UInt32 {
    case a, b, c, d, e, f, g, h, i, j, k, l, m, n, o, p, q, r, s, t, u, v, w, x, y, z
    case digit0, digit1, digit2, digit3, digit4, digit5, digit6, digit7, digit8, digit9
    case f1, f2, f3, f4, f5, f6, f7, f8, f9, f10, f11, f12
    case escape, tab, capsLock, space, enter, backspace, minus, equal, bracketLeft, bracketRight
    case backslash, semicolon, apostrophe, grave, comma, period, slash
    case shiftLeft, shiftRight, controlLeft, controlRight, altLeft, altRight, metaLeft, metaRight, menu
    case insert, delete, home, end, pageUp, pageDown, up, down, left, right, printScreen, scrollLock, pause
    case numLock, numpad0, numpad1, numpad2, numpad3, numpad4, numpad5, numpad6, numpad7, numpad8, numpad9
    case numpadAdd, numpadSubtract, numpadMultiply, numpadDivide, numpadDecimal, numpadEnter
}

/// macOS virtual key codes (`kVK_*`) as key positions. An ANSI keyboard:
/// positions, not characters, so the guest's own layout applies.
enum MacKeys {
    static let table: [UInt16: Key] = [
        0: .a, 1: .s, 2: .d, 3: .f, 4: .h, 5: .g, 6: .z, 7: .x, 8: .c, 9: .v, 11: .b, 12: .q, 13: .w, 14: .e,
        15: .r, 16: .y, 17: .t, 18: .digit1, 19: .digit2, 20: .digit3, 21: .digit4, 22: .digit6, 23: .digit5,
        24: .equal, 25: .digit9, 26: .digit7, 27: .minus, 28: .digit8, 29: .digit0, 30: .bracketRight, 31: .o,
        32: .u, 33: .bracketLeft, 34: .i, 35: .p, 36: .enter, 37: .l, 38: .j, 39: .apostrophe, 40: .k,
        41: .semicolon, 42: .backslash, 43: .comma, 44: .slash, 45: .n, 46: .m, 47: .period, 48: .tab,
        49: .space, 50: .grave, 51: .backspace, 53: .escape, 54: .metaRight, 55: .metaLeft, 56: .shiftLeft,
        57: .capsLock, 58: .altLeft, 59: .controlLeft, 60: .shiftRight, 61: .altRight, 62: .controlRight,
        96: .f5, 97: .f6, 98: .f7, 99: .f3, 100: .f8, 101: .f9, 103: .f11, 109: .f10, 111: .f12,
        114: .insert, 115: .home, 116: .pageUp, 117: .delete, 118: .f4, 119: .end, 120: .f2, 121: .pageDown,
        122: .f1, 123: .left, 124: .right, 125: .down, 126: .up,
        // Keypad. Clear (71) is the Mac's Num Lock; the keypad's = has no key in the guest.
        71: .numLock, 82: .numpad0, 83: .numpad1, 84: .numpad2, 85: .numpad3, 86: .numpad4, 87: .numpad5,
        88: .numpad6, 89: .numpad7, 91: .numpad8, 92: .numpad9, 69: .numpadAdd, 78: .numpadSubtract,
        67: .numpadMultiply, 75: .numpadDivide, 65: .numpadDecimal, 76: .numpadEnter,
    ]
}
