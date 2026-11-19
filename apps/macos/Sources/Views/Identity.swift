// Origami's look, kept inside the content and never a setting: chassis colours,
// granite behind the screen and the bezel. Buttons and selection use the system accent.

import SwiftUI

extension Color {
    init(rgb: UInt32) {
        self.init(.sRGB, red: Double((rgb >> 16) & 255) / 255, green: Double((rgb >> 8) & 255) / 255,
                  blue: Double(rgb & 255) / 255, opacity: 1)
    }
}

extension MachineState.Tone {
    var colour: Color {
        switch self {
        case .grey: return Color(nsColor: .systemGray)
        case .amber: return Color(nsColor: .systemOrange)
        case .green: return Color(nsColor: .systemGreen)
        case .red: return Color(nsColor: .systemRed)
        }
    }
}

/// One or two words with a dot. The colour never stands alone.
struct StatusPill: View {
    let state: MachineState
    var body: some View {
        HStack(spacing: 5) {
            Circle().fill(state.tone.colour).frame(width: 7, height: 7)
            Text(state.pill).font(.caption.weight(.medium))
        }
        .padding(.horizontal, 8).padding(.vertical, 3)
        .background(Capsule().fill(state.tone.colour.opacity(0.14)))
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("Status: \(state.pill)")
    }
}

/// A generic chassis silhouette in the product's colour.
struct ChassisGlyph: View {
    let product: Product
    var body: some View {
        let colour = Color(rgb: product.colour)
        switch product.kind {
        case .workstation:
            RoundedRectangle(cornerRadius: 3).fill(colour.gradient)
                .overlay(alignment: .top) { Capsule().fill(.black.opacity(0.35)).frame(width: 9, height: 2).padding(.top, 4) }
                .overlay(alignment: .bottom) { Circle().fill(.white.opacity(0.8)).frame(width: 3, height: 3).padding(.bottom, 3) }
                .frame(width: 16, height: 22)
                .frame(width: 24, height: 24)
        case .server:
            RoundedRectangle(cornerRadius: 3).fill(colour.gradient)
                .overlay { VStack(spacing: 2) { ForEach(0..<3) { _ in Capsule().fill(.black.opacity(0.35)).frame(height: 1.5) } }.padding(.horizontal, 3) }
                .frame(width: 22, height: 18)
                .frame(width: 24, height: 24)
        }
    }
}

/// The IRIX granite tile (two colours, 256 px) recoloured to two tones and
/// pixel-doubled for a Retina display, never smoothed.
enum Granite {
    /// Tones as 0xRRGGBB: neutral dark behind the screen. The start card uses them as a gradient, `mid` under its cube.
    static let neutral = (Tokens.Colour.graniteNeutral[0], Tokens.Colour.graniteNeutral[1])
    static let mid: UInt32 = {
        func half(_ shift: UInt32) -> UInt32 { (((neutral.0 >> shift) & 255) + ((neutral.1 >> shift) & 255)) / 2 }
        return half(16) << 16 | half(8) << 8 | half(0)
    }()

    /// The tile's pixels: true where it is the lighter of its two colours.
    private static let mask: (side: Int, bits: [Bool])? = {
        guard let url = Bundle.main.url(forResource: "granite-tile", withExtension: "png"),
              let data = try? Data(contentsOf: url),
              let rep = NSBitmapImageRep(data: data) else { return nil }
        let w = rep.pixelsWide, h = rep.pixelsHigh
        var lum = [Int](repeating: 0, count: w * h)
        for y in 0..<h {
            for x in 0..<w {
                guard let c = rep.colorAt(x: x, y: y)?.usingColorSpace(.deviceRGB) else { continue }
                let sum: CGFloat = c.redComponent + c.greenComponent + c.blueComponent
                lum[y * w + x] = Int(sum * 85)
            }
        }
        let mid = ((lum.min() ?? 0) + (lum.max() ?? 0)) / 2
        return (w, lum.map { $0 > mid })
    }()

    private static var cache: [String: CGImage] = [:]

    /// Cached by tones and factor: a body evaluation must not rebuild 512 x 512 pixels.
    static func tile(_ tones: (UInt32, UInt32), factor: Int) -> CGImage? {
        let key = "\(tones.0)-\(tones.1)-\(factor)"
        if let hit = cache[key] { return hit }
        let made = build(tones, factor: factor)
        if let made { cache[key] = made }
        return made
    }

    private static func build(_ tones: (UInt32, UInt32), factor: Int) -> CGImage? {
        guard let (side, bits) = mask else { return nil }
        let n = side * factor
        var px = [UInt8](repeating: 255, count: n * n * 4)
        for y in 0..<n { for x in 0..<n {
            let t = bits[(y / factor) * side + x / factor] ? tones.0 : tones.1
            let o = (y * n + x) * 4
            px[o] = UInt8((t >> 16) & 255); px[o + 1] = UInt8((t >> 8) & 255); px[o + 2] = UInt8(t & 255)
        } }
        return CGImage(width: n, height: n, bitsPerComponent: 8, bitsPerPixel: 32, bytesPerRow: n * 4,
                       space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: CGBitmapInfo(rawValue: CGImageAlphaInfo.noneSkipLast.rawValue),
                       provider: CGDataProvider(data: Data(px) as CFData)!, decode: nil, shouldInterpolate: false, intent: .defaultIntent)
    }
}

struct GraniteBackground: View {
    var tones = Granite.neutral
    @Environment(\.colorSchemeContrast) private var contrast
    @Environment(\.displayScale) private var scale
    var body: some View {
        let flat = Color(rgb: tones.1)
        if contrast == .increased {
            flat   // Increase Contrast removes the speckle
        } else if let tile = Granite.tile(tones, factor: max(1, Int(scale.rounded()))) {
            Image(decorative: tile, scale: scale).interpolation(.none).resizable(resizingMode: .tile)
        } else {
            flat
        }
    }
}

/// A thin bevel: light at the top left, dark at the bottom right.
struct Bezel: ViewModifier {
    @Environment(\.colorSchemeContrast) private var contrast
    func body(content: Content) -> some View {
        content
            .padding(contrast == .increased ? 1 : 3)
            .background(
                RoundedRectangle(cornerRadius: 3).fill(Color(white: 0.1))
            )
            .overlay {
                if contrast != .increased {
                    RoundedRectangle(cornerRadius: 3).strokeBorder(
                        LinearGradient(colors: [.white.opacity(0.55), .black.opacity(0.7)], startPoint: .topLeading, endPoint: .bottomTrailing),
                        lineWidth: 3)
                }
            }
            .shadow(color: .black.opacity(0.5), radius: 6, y: 2)
    }
}
extension View { func bezel() -> some View { modifier(Bezel()) } }
