// Origami's look, kept inside the content and never a setting: Indigo accent,
// chassis colours, granite behind the screen, the bezel and the chrome cube.

import SwiftUI

extension Color {
    /// #4a52c4 in light mode, #7c83f2 in dark.
    static let indigoAccent = Color(nsColor: NSColor(name: nil) { appearance in
        let dark = appearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua
        return dark ? NSColor(srgbRed: 0x7c / 255, green: 0x83 / 255, blue: 0xf2 / 255, alpha: 1)
                    : NSColor(srgbRed: 0x4a / 255, green: 0x52 / 255, blue: 0xc4 / 255, alpha: 1)
    })

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

/// A faint stone speckle, generated once, tiled behind the guest's screen.
enum Granite {
    static let tile: NSImage = {
        let side = 220
        var pixels = [UInt8](repeating: 0, count: side * side * 4)
        var seed: UInt32 = 0x9e3779b9
        func next() -> UInt32 { seed = seed &* 1664525 &+ 1013904223; return seed >> 8 }
        for i in 0..<(side * side) {
            // Mostly the base grey, with scattered lighter and darker grains.
            let r = next() % 100
            var v = 40 + Int(next() % 6)
            if r < 6 { v += 18 + Int(next() % 22) } else if r < 14 { v -= 10 }
            let g = UInt8(max(0, min(255, v)))
            pixels[i * 4] = g; pixels[i * 4 + 1] = g; pixels[i * 4 + 2] = UInt8(min(255, Int(g) + 3)); pixels[i * 4 + 3] = 255
        }
        let provider = CGDataProvider(data: Data(pixels) as CFData)!
        let image = CGImage(width: side, height: side, bitsPerComponent: 8, bitsPerPixel: 32, bytesPerRow: side * 4,
                            space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: CGBitmapInfo(rawValue: CGImageAlphaInfo.noneSkipLast.rawValue),
                            provider: provider, decode: nil, shouldInterpolate: false, intent: .defaultIntent)!
        return NSImage(cgImage: image, size: NSSize(width: side, height: side))
    }()
}

struct GraniteBackground: View {
    @Environment(\.colorSchemeContrast) private var contrast
    var body: some View {
        if contrast == .increased {
            Color(white: 0.16)   // Increase Contrast removes the speckle
        } else {
            Image(nsImage: Granite.tile).resizable(resizingMode: .tile)
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

/// Origami's emblem: a solid bevelled chrome cube, not the SGI logo (which is
/// an open cube of tubes). It turns slowly while a machine starts; Reduce
/// Motion holds it still.
struct ChromeCube: View {
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        TimelineView(.animation(minimumInterval: 1.0 / 30.0, paused: reduceMotion)) { context in
            Canvas { gc, size in
                let angle = reduceMotion ? 0.7 : context.date.timeIntervalSinceReferenceDate * 0.55
                draw(gc, size, angle)
            }
        }
        .accessibilityHidden(true)
    }

    private func draw(_ gc: GraphicsContext, _ size: CGSize, _ yaw: Double) {
        let pitch = 0.5
        let s = min(size.width, size.height) * 0.30
        let c = CGPoint(x: size.width / 2, y: size.height / 2)
        func project(_ x: Double, _ y: Double, _ z: Double) -> (CGPoint, Double) {
            let x1 = x * cos(yaw) + z * sin(yaw), z1 = -x * sin(yaw) + z * cos(yaw)
            let y2 = y * cos(pitch) - z1 * sin(pitch), z2 = y * sin(pitch) + z1 * cos(pitch)
            return (CGPoint(x: c.x + x1 * s, y: c.y - y2 * s), z2)
        }
        // Faces: corner indices and outward normal.
        let corners: [(Double, Double, Double)] = (0..<8).map { i in
            (i & 1 == 0 ? -1 : 1, i & 2 == 0 ? -1 : 1, i & 4 == 0 ? -1 : 1)
        }
        let faces: [([Int], (Double, Double, Double))] = [
            ([0, 1, 3, 2], (0, 0, -1)), ([4, 5, 7, 6], (0, 0, 1)),
            ([0, 1, 5, 4], (0, -1, 0)), ([2, 3, 7, 6], (0, 1, 0)),
            ([0, 2, 6, 4], (-1, 0, 0)), ([1, 3, 7, 5], (1, 0, 0)),
        ]
        var drawn: [(Double, Path, Double)] = []
        for (indices, n) in faces {
            let pts = indices.map { project(corners[$0].0, corners[$0].1, corners[$0].2) }
            // Rotate the normal the same way to cull the faces turned away.
            let (_, nz) = project(n.0, n.1, n.2)
            let (_, oz) = project(0, 0, 0)
            let facing = nz - oz
            guard facing > 0 else { continue }
            var path = Path()
            path.move(to: pts[0].0)
            for p in pts.dropFirst() { path.addLine(to: p.0) }
            path.closeSubpath()
            drawn.append((pts.map { $0.1 }.reduce(0, +), path, facing))
        }
        for (_, path, facing) in drawn.sorted(by: { $0.0 > $1.0 }) {
            let lit = 0.30 + 0.70 * min(1, max(0, facing))
            let hi = Color(white: min(1, lit + 0.25)), lo = Color(white: max(0, lit - 0.30))
            gc.fill(path, with: .linearGradient(Gradient(colors: [hi, lo]),
                                                startPoint: path.boundingRect.origin,
                                                endPoint: CGPoint(x: path.boundingRect.maxX, y: path.boundingRect.maxY)))
            gc.stroke(path, with: .color(.white.opacity(0.55)), lineWidth: 1.2)
        }
    }
}
