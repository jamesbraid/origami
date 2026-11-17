// Sizes the window once per run so the guest shows at 1 point per pixel when the screen allows.

import AppKit
import SwiftUI

struct WindowFitter: NSViewRepresentable {
    let key: String
    let guest: CGSize
    let available: CGSize
    private static var done = Set<ObjectIdentifier>()

    func makeNSView(context: Context) -> NSView { NSView() }

    func updateNSView(_ view: NSView, context: Context) {
        // At most once per window: a reset or a new picture size never grows it again.
        guard guest.width > 1, let window = view.window, !Self.done.contains(ObjectIdentifier(window)) else { return }
        Self.done.insert(ObjectIdentifier(window))
        let need = CGSize(width: guest.width + 60, height: guest.height + 60)
        let grow = CGSize(width: max(0, need.width - available.width), height: max(0, need.height - available.height))
        guard grow.width > 0 || grow.height > 0, let screen = window.screen ?? NSScreen.main else { return }
        var f = window.frame
        f.size.width = min(screen.visibleFrame.width, f.width + grow.width)
        f.size.height = min(screen.visibleFrame.height, f.height + grow.height)
        f.origin.y = window.frame.maxY - f.height   // keep the top edge where it was
        f.origin.x = min(max(screen.visibleFrame.minX, f.origin.x), screen.visibleFrame.maxX - f.width)
        if f.origin.y < screen.visibleFrame.minY { f.origin.y = screen.visibleFrame.minY }
        window.setFrame(f, display: true, animate: false)
    }
}
