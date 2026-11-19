// Sizes the window once per run so the guest shows at 1 point per pixel when the screen allows.

import AppKit
import SwiftUI

struct WindowFitter: NSViewRepresentable {
    let key: String
    let guest: CGSize
    let available: CGSize

    func makeNSView(context: Context) -> FitView { FitView() }

    func updateNSView(_ view: FitView, context: Context) {
        view.guest = guest
        view.available = available
        view.schedule()
    }
}

final class FitView: NSView {
    var guest = CGSize.zero
    var available = CGSize.zero
    private static var done = Set<ObjectIdentifier>()
    /// The room the window may grow into; the self-test replaces it on a runner with no screen.
    static var visibleFrame: (NSWindow) -> CGRect? = { ($0.screen ?? NSScreen.main)?.visibleFrame }

    // Fit as soon as there is both a window and a real picture size. Waiting for some later SwiftUI update fitted
    // the window on whatever the user did next, such as a toolbar click that resized the view.
    override func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
        schedule()
    }

    func schedule() {
        guard window != nil, guest.width > 1 else { return }
        DispatchQueue.main.async { [weak self] in self?.fit() }
    }

    private func fit() {
        // At most once per window: a reset or a new picture size never grows it again.
        guard guest.width > 1, let window, !Self.done.contains(ObjectIdentifier(window)) else { return }
        Self.done.insert(ObjectIdentifier(window))
        let need = CGSize(width: guest.width + 60, height: guest.height + 60)
        let grow = CGSize(width: max(0, need.width - available.width), height: max(0, need.height - available.height))
        guard grow.width > 0 || grow.height > 0, let room = Self.visibleFrame(window) else { return }
        var f = window.frame
        f.size.width = min(room.width, f.width + grow.width)
        f.size.height = min(room.height, f.height + grow.height)
        f.origin.y = window.frame.maxY - f.height   // keep the top edge where it was
        f.origin.x = min(max(room.minX, f.origin.x), room.maxX - f.width)
        if f.origin.y < room.minY { f.origin.y = room.minY }
        window.setFrame(f, display: true, animate: false)
    }
}
