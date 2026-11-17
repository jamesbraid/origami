// The picture is shown at a whole-number scale in a view sized to exactly the picture, so pointer
// mapping needs no letterbox. Fit is the default and fills fractionally once the core can.

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
    /// The view size in points for a `guest` picture in `available` points on a display with
    /// `backing` device pixels per point.
    static func viewSize(guest: CGSize, available: CGSize, backing: CGFloat, mode: ScaleMode) -> CGSize {
        let b = max(1, backing.rounded())
        let largest = Int(min(available.width * b / guest.width, available.height * b / guest.height))
        var n = largest
        switch mode {
        case .fit: break
        case .points: n = min(Int(b), largest)
        case .actual: n = min(1, largest)
        case .double: n = min(Int(2 * b), largest)
        }
        if n >= 1 { return CGSize(width: guest.width * CGFloat(n) / b, height: guest.height * CGFloat(n) / b) }
        // Not even 1 device pixel per guest pixel fits: the core fits the picture, so give it the aspect.
        let s = min(available.width / guest.width, available.height / guest.height)
        return CGSize(width: guest.width * s, height: guest.height * s)
    }
}
