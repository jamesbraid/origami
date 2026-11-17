// The display view: the core draws into its CAMetalLayer on a wake-up, resize or
// scale change only; follow mode sends absolute pointer positions, capture relative.

import AppKit
import OrigamiFrame
import QuartzCore

final class MachineView: NSView {
    let session: MachineSession
    var metalLayer: CAMetalLayer { layer as! CAMetalLayer }
    private(set) var presented = 0
    private(set) var drawNanos: UInt64 = 0
    /// The core's view of this layer: nil until the first draw, and after detach.
    private var coreView: OpaquePointer?
    private var coreVisible = true
    /// Set by detach: a dismantled view never attaches again.
    private var detached = false
    /// Why the last attach or present failed, for Diagnostics and the self-test.
    private(set) var lastError = ""
    /// What the last present reported: 0 skipped, 1 presented, 2 draw again, 3 occluded.
    private(set) var lastDrawn: UInt32 = 99

    private(set) var captured = false
    /// Follow the host pointer; off, a click captures it.
    var absolutePointer = true { didSet { refreshCursorRects() } }
    /// Called after the capture state changes, on the main thread.
    var onCaptureChange: ((Bool) -> Void)?
    /// Non-modifier keys the guest sees as down, lifted when Command goes up.
    private var heldKeys = Set<UInt16>()
    private var scrollRemainder = 0.0
    private var cursorHidden = false
    private var hadFrame = false
    private var observers: [NSObjectProtocol] = []
    /// Over the picture in follow mode: nothing, so only the guest's arrow shows.
    private let blankCursor = NSCursor(image: NSImage(size: NSSize(width: 1, height: 1)), hotSpot: .zero)
    private var remX = 0.0, remY = 0.0
    /// Cursor hiding and mouse freezing need a window someone can see.
    private var interactive: Bool { window?.isVisible == true }

    init(session: MachineSession, size: NSSize = NSSize(width: 640, height: 512)) {
        self.session = session
        super.init(frame: NSRect(origin: .zero, size: size))
        wantsLayer = true
        metalLayer.contentsScale = 1
        session.setFrameHandler(owner: self) { [weak self] in self?.redraw() }
    }
    required init?(coder: NSCoder) { fatalError() }

    deinit {
        for o in observers { NotificationCenter.default.removeObserver(o) }
        if let v = coreView { origami_view_detach(v) }
    }

    /// Let go of the layer before it goes: the core must not draw into a dead window.
    func detach() {
        detached = true
        session.clearFrameHandler(owner: self)
        if let v = coreView { origami_view_detach(v); coreView = nil }
    }

    /// The core is drawing this machine to a layer now: a cube may reflect its screen.
    var isPresenting: Bool { coreView != nil && coreVisible && !detached && hadFrame }

    func setVisible(_ visible: Bool) {
        coreVisible = visible
        if let v = coreView { origami_view_set_visible(v, visible ? 1 : 0) }
        if visible { redraw() }
    }

    override func makeBackingLayer() -> CALayer { CAMetalLayer() }
    override var isFlipped: Bool { true }
    override var acceptsFirstResponder: Bool { true }
    override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }

    private var pixelSize: (w: UInt32, h: UInt32) {
        let scale = metalLayer.contentsScale
        return (UInt32(max(1, bounds.width * scale)), UInt32(max(1, bounds.height * scale)))
    }

    private func resizeDrawable() {
        let scale = window?.backingScaleFactor ?? metalLayer.contentsScale
        if metalLayer.contentsScale != scale {
            metalLayer.contentsScale = scale
            if let v = coreView { origami_view_set_scale(v, Double(scale)) }
        }
        let size = pixelSize
        metalLayer.drawableSize = CGSize(width: Double(size.w), height: Double(size.h))
        if let v = coreView { origami_view_resize(v, size.w, size.h) }
        redraw()
    }
    override func setFrameSize(_ s: NSSize) { super.setFrameSize(s); resizeDrawable() }
    override func viewDidChangeBackingProperties() { super.viewDidChangeBackingProperties(); resizeDrawable() }

    override func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
        for o in observers { NotificationCenter.default.removeObserver(o) }
        observers = []
        resizeDrawable()
        guard let window else { return }
        let center = NotificationCenter.default
        observers.append(center.addObserver(forName: NSWindow.didResignKeyNotification, object: window, queue: .main) { [weak self] _ in
            self?.endCapture(); self?.releaseAllKeys()
        })
        observers.append(center.addObserver(forName: NSApplication.didResignActiveNotification, object: nil, queue: .main) { [weak self] _ in
            self?.endCapture(); self?.releaseAllKeys()
        })
        // A hidden window or a minimised one draws nothing.
        observers.append(center.addObserver(forName: NSWindow.didChangeOcclusionStateNotification, object: window, queue: .main) { [weak self] _ in
            guard let self, let w = self.window else { return }
            self.setVisible(w.occlusionState.contains(.visible))
        })
    }

    override func resetCursorRects() {
        super.resetCursorRects()
        if absolutePointer && !captured && hadFrame { addCursorRect(bounds, cursor: blankCursor) }
    }
    func refreshCursorRects() { window?.invalidateCursorRects(for: self) }

    /// Ask the core to draw the newest picture, if there is one.
    func redraw() {
        if let w = window, w.isVisible, !w.occlusionState.contains(.visible) { return }
        guard coreVisible, !detached else { return }
        let t0 = clock_gettime_nsec_np(CLOCK_THREAD_CPUTIME_ID)
        if coreView == nil && !attach() { return }
        guard let v = coreView else { return }
        // A rebuilt swapchain asks for another call; a few are plenty.
        for _ in 0..<3 {
            var drawn: UInt32 = 0
            let st = origami_view_present(v, &drawn)
            guard st == ORIGAMI_OK else { lastError = "present: \(Self.coreError(st))"; break }
            lastDrawn = drawn
            // 3 is "occluded": drawn offscreen because the window is not on screen.
            if drawn == UInt32(ORIGAMI_DRAWN_PRESENTED) || drawn == 3 {
                presented += 1
                if !hadFrame { hadFrame = true; refreshCursorRects() }   // the host cursor hides once there is a picture
            }
            if drawn != UInt32(ORIGAMI_DRAWN_AGAIN) { break }
        }
        drawNanos += clock_gettime_nsec_np(CLOCK_THREAD_CPUTIME_ID) - t0
    }

    private static func coreError(_ status: Int32) -> String {
        var buf = [CChar](repeating: 0, count: 512)
        _ = origami_last_error(&buf, buf.count)
        return "\(status) \(String(cString: buf))"
    }

    /// What the core last drew, as tight BGRA with the top row first. For self-tests only:
    /// it draws again and waits for the GPU.
    func readback() -> (width: Int, height: Int, bgra: [UInt8])? {
        guard let v = coreView else { return nil }
        var w: UInt32 = 0, h: UInt32 = 0, stride: UInt32 = 0
        _ = origami_view_readback(v, nil, 0, &w, &h, &stride)
        guard w > 0, h > 0 else { return nil }
        var buf = [UInt8](repeating: 0, count: Int(stride) * Int(h))
        let st = origami_view_readback(v, &buf, buf.count, &w, &h, &stride)
        guard st == ORIGAMI_OK else { lastError = "readback: \(Self.coreError(st))"; return nil }
        if Int(stride) != Int(w) * 4 {   // repack to tight rows
            var tight = [UInt8](); tight.reserveCapacity(Int(w) * Int(h) * 4)
            for y in 0..<Int(h) { tight += buf[y * Int(stride) ..< y * Int(stride) + Int(w) * 4] }
            buf = tight
        }
        return (Int(w), Int(h), buf)
    }

    private func attach() -> Bool {
        let size = pixelSize
        var surface = origami_surface()
        surface.size = UInt32(MemoryLayout<origami_surface>.size)
        surface.kind = UInt32(ORIGAMI_SURFACE_CA_METAL_LAYER)
        surface.window = Unmanaged.passUnretained(metalLayer).toOpaque()
        var out: OpaquePointer?
        let status = origami_view_attach(session.handle, 0, &surface, size.w, size.h, Double(metalLayer.contentsScale), &out)
        guard status == ORIGAMI_OK, let out else { lastError = "attach: \(Self.coreError(status))"; return false }
        coreView = out
        origami_view_set_visible(out, coreVisible ? 1 : 0)
        return true
    }

    // MARK: capture

    func beginCapture() {
        guard !captured else { return }
        captured = true
        refreshCursorRects()
        if interactive {
            NSCursor.hide(); cursorHidden = true
            CGAssociateMouseAndMouseCursorPosition(0)
        }
        onCaptureChange?(true)
    }

    func endCapture() {
        guard captured else { return }
        releaseAllKeys()
        captured = false
        refreshCursorRects()
        if cursorHidden { NSCursor.unhide(); cursorHidden = false }
        CGAssociateMouseAndMouseCursorPosition(1)
        onCaptureChange?(false)
    }

    /// Every key and button the guest sees as up, so nothing sticks when focus leaves.
    func releaseAllKeys() {
        session.releaseAll()
        heldKeys.removeAll()
    }

    override func resignFirstResponder() -> Bool { endCapture(); releaseAllKeys(); return true }

    // MARK: keyboard

    private func send(_ code: UInt16, _ down: Bool) {
        guard let key = MacKeys.table[code] else { return }
        if down { heldKeys.insert(code) } else { heldKeys.remove(code) }
        session.key(key.rawValue, down: down)
    }
    private func isModifier(_ code: UInt16) -> Bool { code == 57 || Self.modifierBit[code] != nil }
    // The host's key repeat is the guest's: QEMU's PS/2 keyboard has no typematic of its own.
    override func keyDown(with e: NSEvent) { send(e.keyCode, true) }
    override func keyUp(with e: NSEvent) { send(e.keyCode, false) }
    override func flagsChanged(with e: NSEvent) {
        // Control+Option together give the mouse back.
        if captured && e.modifierFlags.contains(.control) && e.modifierFlags.contains(.option) {
            endCapture(); return
        }
        // Press or release comes from the event's per-side flag bit, never from our
        // held set (focus loss clears it while a modifier is still down).
        if e.keyCode == 57 {   // Caps Lock toggles: the guest sees a press and a release
            send(57, true); send(57, false)
        } else if let mask = Self.modifierBit[e.keyCode] {
            let down = e.modifierFlags.rawValue & mask != 0
            send(e.keyCode, down)
            // macOS sends no key-up for a key pressed during a Command chord.
            if !down && (e.keyCode == 54 || e.keyCode == 55) {
                for k in Array(heldKeys) where !isModifier(k) { send(k, false) }
            }
        }
    }

    /// Device-dependent flag bits (IOKit NX_DEVICE*KEYMASK), as QEMU's Cocoa UI reads them.
    private static let modifierBit: [UInt16: UInt] = [
        59: 0x1, 62: 0x2000,   // Control left, right
        56: 0x2, 60: 0x4,      // Shift
        55: 0x8, 54: 0x10,     // Command
        58: 0x20, 61: 0x40,    // Option
    ]

    // MARK: pointer

    /// Host motion while captured, in points; fractions carry to the next call.
    func applyMouseDelta(_ dx: Double, _ dy: Double) {
        remX += dx; remY += dy
        let ix = Int32(remX.rounded(.towardZero)), iy = Int32(remY.rounded(.towardZero))
        remX -= Double(ix); remY -= Double(iy)
        if ix != 0 || iy != 0 { session.pointerMove(dx: ix, dy: iy) }
    }

    /// Guest pixels under the event; the picture fills the view.
    private func headPoint(_ e: NSEvent) -> (Int32, Int32) {
        let p = convert(e.locationInWindow, from: nil)
        guard let h = session.cachedHead else { return (0, 0) }
        let x = p.x * Double(h.width) / Double(max(bounds.width, 1))
        let y = p.y * Double(h.height) / Double(max(bounds.height, 1))
        return (Int32(max(0, min(Double(h.width - 1), x))), Int32(max(0, min(Double(h.height - 1), y))))
    }
    private func moved(_ e: NSEvent) {
        if captured { applyMouseDelta(Double(e.deltaX), Double(e.deltaY)) }
        else if absolutePointer {
            let (x, y) = headPoint(e)
            session.pointerAt(x: x, y: y)
        }
    }
    override func mouseMoved(with e: NSEvent) { moved(e) }
    override func mouseDragged(with e: NSEvent) { moved(e) }
    override func rightMouseDragged(with e: NSEvent) { moved(e) }
    override func otherMouseDragged(with e: NSEvent) { moved(e) }

    private func button(_ b: UInt32, _ down: Bool, _ e: NSEvent) {
        if !captured && !absolutePointer {
            if down { beginCapture() }   // the click that captures is not the guest's
            return
        }
        if absolutePointer && down { moved(e) }
        session.button(b, down: down)
    }
    override func mouseDown(with e: NSEvent) { window?.makeFirstResponder(self); button(UInt32(ORIGAMI_BUTTON_LEFT), true, e) }
    override func mouseUp(with e: NSEvent) { if captured || absolutePointer { button(UInt32(ORIGAMI_BUTTON_LEFT), false, e) } }
    override func rightMouseDown(with e: NSEvent) { button(UInt32(ORIGAMI_BUTTON_RIGHT), true, e) }
    override func rightMouseUp(with e: NSEvent) { if captured || absolutePointer { button(UInt32(ORIGAMI_BUTTON_RIGHT), false, e) } }
    override func otherMouseDown(with e: NSEvent) { button(UInt32(ORIGAMI_BUTTON_MIDDLE), true, e) }
    override func otherMouseUp(with e: NSEvent) { if captured || absolutePointer { button(UInt32(ORIGAMI_BUTTON_MIDDLE), false, e) } }
    override func scrollWheel(with e: NSEvent) {
        guard captured || absolutePointer else { return }
        // A trackpad sends many small deltas: about 10 points a notch, fractions carried.
        scrollRemainder += e.scrollingDeltaY / (e.hasPreciseScrollingDeltas ? 10 : 1)
        let n = Int32(max(-5, min(5, scrollRemainder.rounded(.towardZero))))
        scrollRemainder -= Double(n)
        if n != 0 { session.wheel(n) }
    }
    override func updateTrackingAreas() {
        super.updateTrackingAreas()
        trackingAreas.forEach(removeTrackingArea)
        addTrackingArea(NSTrackingArea(rect: bounds, options: [.mouseMoved, .activeInKeyWindow, .inVisibleRect], owner: self))
    }
}
