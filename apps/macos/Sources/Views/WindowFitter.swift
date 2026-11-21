// Sizes the window to the picture: once per run when its picture first appears, growing only, and when the
// View menu asks for a scale. Nothing else resizes the window; the picture fills whatever size it has.

import AppKit
import SwiftUI

struct WindowFitter: NSViewRepresentable {
    let key: String
    let model: MachineModel
    let guest: CGSize
    let available: CGSize
    let backing: CGFloat

    func makeNSView(context: Context) -> FitView { FitView() }

    func updateNSView(_ view: FitView, context: Context) {
        model.fitter = view
        view.key = key
        view.guest = guest
        view.available = available
        view.backing = backing
        view.schedule()
    }
}

final class FitView: NSView {
    var key = ""
    var guest = CGSize.zero
    var available = CGSize.zero
    var backing: CGFloat = 1
    private static var done = Set<String>()
    /// How many pictures have had their one fit; the display self-test reads it.
    static var fittedCount: Int { done.count }
    /// The room the window may grow into; the self-test replaces it on a runner with no screen.
    static var visibleFrame: (NSWindow) -> CGRect? = { ($0.screen ?? NSScreen.main)?.visibleFrame }

    // Fit as soon as there is both a window and a real picture size, not on whatever the user does next.
    override func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
        schedule()
    }

    func schedule() {
        guard window != nil, guest.width > 1, !Self.done.contains(key) else { return }
        DispatchQueue.main.async { [weak self] in self?.fitOnce() }
    }

    private func fitOnce() {
        guard guest.width > 1, let window, !Self.done.contains(key), let room = Self.visibleFrame(window) else { return }
        Self.done.insert(key)
        let chrome = CGSize(width: window.frame.width - picture.width, height: window.frame.height - picture.height)
        let fits = CGSize(width: room.width - chrome.width, height: room.height - chrome.height)
        show(pointsPerPixel: Scaling.firstScale(guest: guest, room: fits), growOnly: true)
    }

    /// The room the picture has now: the screen view less its margin.
    private var picture: CGSize { CGSize(width: available.width - 2 * Scaling.inset, height: available.height - 2 * Scaling.inset) }

    /// Resize the window so the picture is `p` points per guest pixel, as far as the screen allows.
    /// `growOnly` keeps a window the user made bigger: no axis shrinks, and one big enough on both is left alone.
    func show(pointsPerPixel p: CGFloat, growOnly: Bool = false) {
        guard guest.width > 1, let window, !window.styleMask.contains(.fullScreen), let room = Self.visibleFrame(window) else { return }
        var f = window.frame
        var w = f.width + guest.width * p - picture.width, h = f.height + guest.height * p - picture.height
        if growOnly {
            if w <= f.width && h <= f.height { return }
            (w, h) = (max(w, f.width), max(h, f.height))
        }
        f.size.width = min(room.width, w)
        f.size.height = min(room.height, h)
        f.origin.y = window.frame.maxY - f.height   // keep the top edge where it was
        f.origin.x = min(max(room.minX, f.origin.x), room.maxX - f.width)
        if f.origin.y < room.minY { f.origin.y = room.minY }
        window.setFrame(f, display: true, animate: false)
    }
}
