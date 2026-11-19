// The view is sized to exactly the picture, so pointer mapping needs no letterbox. Fit is the default:
// whole device-pixel steps from 1 device pixel per guest pixel up, a smooth fill only below that.

import CoreGraphics

enum ScaleMode: Equatable {
    /// 1 guest pixel = 1 point, else the largest that fits.
    case points
    /// The biggest whole number of device pixels per guest pixel that fits.
    case fit
    /// 1 guest pixel = 1 device pixel.
    case actual
    /// 2 points per guest pixel, else the largest that fits.
    case double
}

enum Scaling {
    /// The view size in points for a `guest` picture in `available` points on a display with `backing`
    /// device pixels per point. The compositor scales the guest-sized drawable to this size.
    static func viewSize(guest: CGSize, available: CGSize, backing: CGFloat, mode: ScaleMode) -> CGSize {
        let b = max(1, backing.rounded())
        let fill = min(available.width / guest.width, available.height / guest.height)   // points per guest pixel
        let steps = (fill * b + 1e-9).rounded(.down)
        let fitted = steps >= 1 ? steps / b : fill   // whole device pixels per guest pixel, smooth only below one
        let want: CGFloat
        switch mode {
        case .fit: want = fitted
        case .points: want = fill >= 1 ? 1 : fitted
        case .actual: want = fill >= 1 / b ? 1 / b : fill
        case .double: want = fill >= 2 ? 2 : fitted
        }
        return CGSize(width: guest.width * want, height: guest.height * want)
    }

    /// How far to move a view of `size` from the centre of `container` so its origin is a whole device pixel.
    static func snap(container: CGSize, size: CGSize, backing: CGFloat) -> CGSize {
        let b = max(1, backing.rounded())
        func one(_ c: CGFloat, _ s: CGFloat) -> CGFloat { let o = (c - s) / 2; return ((o * b).rounded() / b) - o }
        return CGSize(width: one(container.width, size.width), height: one(container.height, size.height))
    }

    /// The status bar's text: device pixels per guest pixel as a percentage, led by Fit when that is the mode.
    static func label(mode: ScaleMode, size: CGSize, guest: CGSize, backing: CGFloat) -> String {
        let percent = Int((size.width * max(1, backing.rounded()) / max(1, guest.width) * 100).rounded())
        return mode == .fit ? "Fit \(percent)%" : "\(percent)%"
    }
}
