// Machine art: the shared product stills from resources/identity/stills (copied
// into the bundle by build.sh), shown as they are with no status badge.

import SwiftUI

enum Art {
    private struct Manifest: Decodable {
        struct Still: Decodable { let colour: String }
        let topologies: [String: String]
        let products: [String: String]
        let stills: [String: Still]
    }

    private static let root = Bundle.main.resourceURL?.appendingPathComponent("stills")
    private static let manifest: Manifest? = {
        guard let url = root?.appendingPathComponent("manifest.json"), let data = try? Data(contentsOf: url) else { return nil }
        return try? JSONDecoder().decode(Manifest.self, from: data)
    }()

    /// The art id for a machine: its topology's still, else its product's.
    static func id(topology: String?, product: String) -> String? {
        guard let m = manifest else { return nil }
        if let t = topology, let id = m.topologies[t] { return id }
        return m.products[product.lowercased()]
    }

    static func colour(_ id: String) -> UInt32? {
        guard let hex = manifest?.stills[id]?.colour else { return nil }
        return UInt32(hex.dropFirst(), radix: 16)
    }

    private static var cache: [String: NSImage] = [:]

    /// `name` is "34-dark", "72-light" or "hero": the @2x file at its point size.
    static func image(_ id: String, _ name: String, points: NSSize) -> NSImage? {
        let key = "\(id)/\(name)/\(Int(points.width))x\(Int(points.height))"
        if let c = cache[key] { return c }
        guard let url = root?.appendingPathComponent(id).appendingPathComponent("\(name)@2x.png"),
              let img = NSImage(contentsOf: url) else { return nil }
        img.size = points
        cache[key] = img
        return img
    }
}

/// A machine's still at `pt` points square (20, 34 or 72), or its glyph when it has none.
struct MachineArt: View {
    let product: Product
    let pt: Int
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        if let id = product.art,
           let img = Art.image(id, "\(pt)-\(scheme == .dark ? "dark" : "light")", points: NSSize(width: pt, height: pt)) {
            Image(nsImage: img).resizable().interpolation(.high).frame(width: CGFloat(pt), height: CGFloat(pt))
                .accessibilityHidden(true)
        } else {
            ChassisGlyph(product: product).scaleEffect(CGFloat(pt) / 24).frame(width: CGFloat(pt), height: CGFloat(pt))
        }
    }
}

/// The never-started hero: 240 x 180 points, shadow kept.
struct HeroArt: View {
    let product: Product
    var body: some View {
        if let id = product.art, let img = Art.image(id, "hero", points: NSSize(width: Tokens.Size.artHero.width, height: Tokens.Size.artHero.height)) {
            Image(nsImage: img).resizable().interpolation(.high).frame(width: Tokens.Size.artHero.width, height: Tokens.Size.artHero.height).accessibilityHidden(true)
        } else {
            ChassisGlyph(product: product).scaleEffect(3.2).frame(width: 240, height: 84)
        }
    }
}
