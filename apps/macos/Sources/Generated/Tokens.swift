// generated from resources/identity/tokens.toml by `cargo xtask tokens`; do not edit.

import CoreGraphics

enum Tokens {
    enum Chassis {
        static let tabSelectedFill: CGFloat = 0.22
        static let tabSelectedStroke: CGFloat = 0.5
    }
    enum Colour {
        static let graniteNeutral: [UInt32] = [0x282a30, 0x1f2126]
    }
    enum Icon {
        static let emptyState: CGFloat = 64
        static let rowDot: CGFloat = 7
        static let toolbar: CGFloat = 16
    }
    enum Motion {
        static let captureHintMs: CGFloat = 3000
        static let fadeMs: CGFloat = 200
        static let toastMs: CGFloat = 4000
    }
    enum Pane {
        static let productLineAlpha: CGFloat = 0.7
        static let tintBottomAlpha: CGFloat = 0
        static let tintTopAlpha: CGFloat = 0.35
    }
    enum Radius {
        static let card: CGFloat = 10
        static let control: CGFloat = 6
        static let dot: CGFloat = 3.5
        static let sheet: CGFloat = 12
    }
    enum Size {
        static let artGallery = CGSize(width: 120, height: 90)
        static let artHero = CGSize(width: 240, height: 180)
        static let artSidebar: CGFloat = 34
        static let artSummary: CGFloat = 72
        static let artTab: CGFloat = 20
        static let card = CGSize(width: 140, height: 104)
        static let statusBarHeight: CGFloat = 24
        static let tabBarHeight: CGFloat = 34
        static let tabHeight: CGFloat = 26
    }
    enum Space {
        static let l: CGFloat = 20
        static let m: CGFloat = 12
        static let s: CGFloat = 8
        static let xl: CGFloat = 24
        static let xs: CGFloat = 4
    }
}
