// The CPU panel: the core's activity view painted as gr_osview-style strip bars. Each CPU has a title and a
// coloured legend above a black-framed bar cut into segments, with a heavy ruler marked in tenths.

import SwiftUI

struct CpuPanel: View {
    @ObservedObject var model: CpuLoadModel
    static let pad: CGFloat = 8, header: CGFloat = 18, bar: CGFloat = 20, frame: CGFloat = 2, ruler: CGFloat = 5, gap: CGFloat = 6, caption: CGFloat = 16

    static func height(bars: Int) -> CGFloat { pad * 2 + CGFloat(max(bars, 1)) * (header + bar + ruler + gap) + caption }
    static func color(_ rgb: UInt32) -> Color {
        Color(red: Double((rgb >> 16) & 0xFF) / 255, green: Double((rgb >> 8) & 0xFF) / 255, blue: Double(rgb & 0xFF) / 255)
    }

    @Environment(\.displayScale) private var displayScale

    var body: some View {
        let scene = model.scene
        let scale = displayScale
        Canvas { ctx, size in
            Self.paint(scene, ctx: &ctx, width: size.width, scale: scale)
        }
        .frame(height: Self.height(bars: scene?.bars.count ?? 0))
        .frame(maxWidth: .infinity)
        .background(Self.color(scene?.background ?? 0xBFBFBF))
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(Self.summary(scene))
    }

    /// What a screen reader says: each bar's segments as percentages.
    static func summary(_ v: OrigamiOsview?) -> String {
        guard let v = v, !v.bars.isEmpty else { return cpuLoadPlaceholder(v) }
        return v.bars.map { b in
            b.title + ": " + zip(b.legend, b.segments).map { "\(Int(($1.fraction * 100).rounded())) percent \($0.label)" }.joined(separator: ", ")
        }.joined(separator: ". ")
    }

    /// With no scene yet the panel is grey with "measuring…"; the caption comes with the first scene.
    static func paint(_ v: OrigamiOsview?, ctx: inout GraphicsContext, width w: CGFloat, scale: CGFloat) {
        func text(_ s: String, _ color: Color, bold: Bool, at p: CGPoint) -> CGFloat {
            var t = Text(s).font(.system(size: bold ? 11 : 10, weight: bold ? .bold : .regular)).foregroundColor(color)
            if bold { t = t.italic() }
            let r = ctx.resolve(t)
            ctx.draw(r, at: p, anchor: .topLeading)
            return r.measure(in: CGSize(width: 10000, height: 100)).width
        }
        var y = pad
        if v?.bars.isEmpty ?? true {
            _ = text(v?.note ?? cpuLoadPlaceholder(nil), .black, bold: false, at: CGPoint(x: pad, y: pad))
            y = pad + header
        }
        guard let v = v else { return }
        for b in v.bars {
            var x = pad + text(b.title + ":", .black, bold: true, at: CGPoint(x: pad, y: y)) + 6
            for k in b.legend { x += text(k.label, color(k.rgb), bold: true, at: CGPoint(x: x, y: y)) + 6 }
            y += header
            let bw = w - pad * 2
            ctx.fill(Path(CGRect(x: pad - frame, y: y - frame, width: bw + frame * 2, height: bar + frame + ruler)), with: .color(color(v.barBackground)))
            var at = pad
            for s in b.segments {
                let sw = max(0, bw * CGFloat(s.fraction))
                ctx.fill(Path(CGRect(x: at, y: y, width: sw, height: bar)), with: .color(color(s.rgb)))
                at += sw
            }
            for t in 1..<max(Int(v.ticks), 1) {
                // On whole device pixels, so every mark is the same width.
                let tx = ((pad + bw * CGFloat(t) / CGFloat(v.ticks)) * scale).rounded() / scale
                ctx.fill(Path(CGRect(x: tx, y: y + bar - 5, width: 1, height: 5 + ruler)), with: .color(.black))
            }
            y += bar + ruler + gap
        }
        _ = text(v.caption, Color(white: 0.2), bold: false, at: CGPoint(x: pad, y: y))
    }
}
