// The view is sized to exactly the picture, so pointer mapping needs no letterbox. The picture fills its
// area at any scale; the core draws it sharp bilinear, so text stays near crisp between whole scales.

import CoreGraphics

enum Scaling {
    /// The granite margin and bezel around the picture, per side.
    static let inset: CGFloat = 30

    /// The view size in points for a `guest` picture filling `available` points, aspect kept.
    static func viewSize(guest: CGSize, available: CGSize) -> CGSize {
        let fill = min(available.width / guest.width, available.height / guest.height)
        return CGSize(width: guest.width * fill, height: guest.height * fill)
    }

    /// Points per guest pixel when the picture first appears: exactly 1 where `room` holds it, else
    /// the largest picture that fits.
    static func firstScale(guest: CGSize, room: CGSize) -> CGFloat {
        min(1, room.width / guest.width, room.height / guest.height)
    }

    /// How far to move a view of `size` from the centre of `container` so its origin is a whole device pixel.
    static func snap(container: CGSize, size: CGSize, backing: CGFloat) -> CGSize {
        let b = max(1, backing.rounded())
        func one(_ c: CGFloat, _ s: CGFloat) -> CGFloat { let o = (c - s) / 2; return ((o * b).rounded() / b) - o }
        return CGSize(width: one(container.width, size.width), height: one(container.height, size.height))
    }
}
