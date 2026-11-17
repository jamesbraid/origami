// The CPU panel: one bar per emulated CPU, user in blue then system in red on a dark background, with the
// user+system percentage at the right.

import SwiftUI

struct CpuPanel: View {
    @ObservedObject var model: CpuLoadModel
    static let userColour = Color(red: 0.25, green: 0.50, blue: 1.0)
    static let sysColour = Color(red: 0.95, green: 0.25, blue: 0.25)

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text("Emulated CPU load (measured on the host)").font(.caption.weight(.medium)).foregroundStyle(.secondary)
            if let cpus = model.report?.cpus, !cpus.isEmpty {
                ForEach(cpus, id: \.cpu) { c in
                    HStack(spacing: 8) {
                        Text("cpu \(c.cpu)").font(.system(.caption, design: .monospaced)).frame(width: 44, alignment: .leading)
                        Bar(user: Double(c.userPct), sys: Double(c.sysPct))
                            .frame(height: 10)
                        Text(String(format: "%.0f%%", c.userPct + c.sysPct)).font(.system(.caption, design: .monospaced))
                            .frame(width: 40, alignment: .trailing)
                    }
                    .accessibilityElement(children: .ignore)
                    .accessibilityLabel("cpu \(c.cpu): \(Int(c.userPct)) percent user, \(Int(c.sysPct)) percent system")
                }
            } else {
                Text(cpuLoadPlaceholder(model.report)).font(.caption).foregroundStyle(.secondary)
            }
        }
        .padding(.horizontal, 12).padding(.vertical, 8)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(.bar)
    }

    private struct Bar: View {
        let user: Double, sys: Double
        var body: some View {
            GeometryReader { g in
                let w = g.size.width
                ZStack(alignment: .leading) {
                    Rectangle().fill(Color(white: 0.12))
                    HStack(spacing: 0) {
                        Rectangle().fill(CpuPanel.userColour).frame(width: w * min(1, max(0, user / 100)))
                        Rectangle().fill(CpuPanel.sysColour).frame(width: w * min(1 - min(1, max(0, user / 100)), max(0, sys / 100)))
                    }
                }
            }
        }
    }
}
