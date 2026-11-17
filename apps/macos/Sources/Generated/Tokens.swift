// generated from resources/identity/tokens.toml by `cargo xtask tokens`; do not edit.

import CoreGraphics

enum Tokens {
    enum Chassis {
        static let tabSelectedFill: CGFloat = 0.22
        static let tabSelectedStroke: CGFloat = 0.5
    }
    enum Colour {
        static let graniteLight: [UInt32] = [0xe2e4e8, 0xd6d8dd]
        static let graniteNeutral: [UInt32] = [0x282a30, 0x1f2126]
    }
    enum Pane {
        static let productLineAlpha: CGFloat = 0.7
        static let tintBottomAlpha: CGFloat = 0
        static let tintTopAlpha: CGFloat = 0.35
    }
    enum Size {
        static let artGallery = CGSize(width: 120, height: 90)
        static let artHero = CGSize(width: 240, height: 180)
        static let artSidebar: CGFloat = 34
        static let artSummary: CGFloat = 72
        static let artTab: CGFloat = 20
        static let card = CGSize(width: 140, height: 104)
        static let tabBarHeight: CGFloat = 34
        static let tabHeight: CGFloat = 26
    }
}
