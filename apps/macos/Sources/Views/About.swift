// The About window: the cube on its granite band, the credits and an
// acknowledgements sheet.

import OrigamiFrame
import SwiftUI

struct AboutView: View {
    @EnvironmentObject var app: AppModel
    @State private var acknowledgements = false

    /// Reflecting a running machine's screen is off for now; the code stays for when it returns.
    static let reflectsRunningMachine = false

    /// The machine whose screen the chrome reflects: the one on show if it runs, else any that does.
    private var running: MachineModel? {
        if let c = app.current, c.state == .running, c.view?.isPresenting == true { return c }
        return app.models.first { $0.state == .running && $0.view?.isPresenting == true }
    }

    @State private var qemuVersion: String?

    private var version: String {
        let v = Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? ""
        return "Version \(v)" + (qemuVersion.map { " · QEMU \($0)" } ?? "")
    }

    var body: some View {
        VStack(spacing: 0) {
            // The view is the band: the cube fills 90% of its height on granite across the width.
            CubeHost(look: .about, machine: Self.reflectsRunningMachine ? running : nil)
                .frame(width: 360, height: 180)
                .accessibilityLabel("The SGI cube")
            VStack(spacing: 6) {
                Text("Origami").font(.title.weight(.semibold))
                Text(version).font(.caption).foregroundStyle(.secondary)
                Text("Released under the BSD 3-Clause licence.").font(.caption).foregroundStyle(.secondary)
                Text("All trademarks are the property of their respective owners.")
                    .font(.caption).foregroundStyle(.secondary).multilineTextAlignment(.center)
                Button("Acknowledgements…") { acknowledgements = true }.padding(.top, 6)
            }
            .padding(16)
        }
        .frame(width: 360)
        .sheet(isPresented: $acknowledgements) { Acknowledgements() }
        .task {
            // The core is asked off the main actor; the line fills in when it answers.
            guard let core = app.host?.core else { return }
            qemuVersion = await Task.detached { core.engines().first(where: { $0.id == "qemu" })?.version }.value
        }
    }
}

struct Acknowledgements: View {
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text("Acknowledgements").font(.headline)
            ScrollView {
                VStack(alignment: .leading, spacing: 8) {
                    ForEach(0..<origami_cube_scene_count(), id: \.self) { i in
                        VStack(alignment: .leading, spacing: 2) {
                            Text(sceneText(i, Int32(ORIGAMI_CUBE_SCENE_TITLE))).font(.caption.weight(.medium))
                            Text(sceneText(i, Int32(ORIGAMI_CUBE_SCENE_CREDIT))).font(.caption).foregroundStyle(.secondary)
                        }
                    }
                    Text("SGI cube model from martin-steen.de. The granite tile is from the IRIX desktop.")
                        .font(.caption).foregroundStyle(.secondary)
                    Text("Machines run on QEMU.").font(.caption).foregroundStyle(.secondary)
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            }
            HStack { Spacer(); Button("Done") { dismiss() }.keyboardShortcut(.defaultAction) }
        }
        .padding(16).frame(width: 420, height: 300)
    }
}
