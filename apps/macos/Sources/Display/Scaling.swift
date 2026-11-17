// The view is sized to exactly the picture, so pointer mapping needs no letterbox. Fit is the default:
// whole device-pixel steps from 1 point per guest pixel up, a smooth fractional fill below it.

import CoreGraphics

enum ScaleMode: Equatable {
    /// 1 guest pixel = 1 point, else the largest that fits.
    case points
    /// The biggest whole-number scale that fits.
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
        let want: CGFloat
        switch mode {
        case .fit: want = fill >= 1 ? (fill * b).rounded(.down) / b : fill   // whole device-pixel steps from 1 pt up, smooth below
        case .points: want = fill >= 1 ? 1 : fill
        case .actual: want = fill >= 1 / b ? 1 / b : fill
        case .double: want = fill >= 2 ? 2 : (fill >= 1 ? (fill * b).rounded(.down) / b : fill)
        }
        return CGSize(width: guest.width * want, height: guest.height * want)
    }
}
