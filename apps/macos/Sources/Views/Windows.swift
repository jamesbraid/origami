// The secondary windows: Diagnostics, a machine's Settings sheet and the
// app's Preferences.

import SwiftUI

// MARK: Diagnostics

@MainActor
func diagnosticsReport(_ model: MachineModel) -> String {
    var out = ["Machine: \(model.name) (\(model.record.product.name))", "State: \(model.state.pill)"]
    if let p = model.picture { out.append("Picture: \(p.width) x \(p.height)") }
    if let s = model.session {
        let v = s.machine.view()
        out.append("Engine: \(v.engine), core state: \(v.state), reason: \(v.reason)")
        if let e = v.lastError { out.append("Last error: \(e)") }
        for h in v.heads { out.append("Head \(h.index): \(h.width) x \(h.height), picture \(h.active ? "yes" : "no"), sequence \(h.seq)") }
        out.append("Console: \(v.console.connected ? "connected" : "down\(v.console.downReason.map { " (\($0))" } ?? "")"), \(v.console.total) bytes")
        out.append("Frame wake-ups received: \(s.wakes), presented by the app: \(model.view?.presented ?? 0)")
    } else {
        out.append("The machine has not been opened by this app.")
    }
    return out.joined(separator: "\n")
}

struct DiagnosticsView: View {
    @EnvironmentObject var app: AppModel

    var body: some View {
        if let model = app.current {
            // One refresh a second while the window is open; a closed window costs nothing.
            TimelineView(.periodic(from: .now, by: 1)) { _ in
                let report = diagnosticsReport(model)
                VStack(alignment: .leading, spacing: 8) {
                    HStack {
                        Text(model.name).font(.headline)
                        StatusPill(state: model.state)
                        Spacer()
                        Button("Copy Report") {
                            NSPasteboard.general.clearContents()
                            NSPasteboard.general.setString(report, forType: .string)
                        }
                    }
                    ScrollView {
                        Text(report).font(.system(.caption, design: .monospaced)).textSelection(.enabled)
                            .frame(maxWidth: .infinity, alignment: .leading)
                    }
                    .padding(8).background(RoundedRectangle(cornerRadius: 6).fill(Color(nsColor: .textBackgroundColor)))
                }
                .padding(12)
            }
            .frame(minWidth: 560, minHeight: 320)
        } else {
            Text("Choose a machine in the library.").foregroundStyle(.secondary).frame(minWidth: 560, minHeight: 320)
        }
    }
}

// MARK: a machine's settings

struct MachineSettings: View {
    @ObservedObject var model: MachineModel
    @Environment(\.dismiss) private var dismiss
    @State private var pane = Pane.general

    enum Pane: String, CaseIterable, Identifiable {
        case general = "General", system = "System Information", processor = "Processor & Memory", input = "Input"
        var id: String { rawValue }
        var symbol: String {
            switch self {
            case .general: return "slider.horizontal.3"
            case .system: return "info.circle"
            case .processor: return "cpu"
            case .input: return "cursorarrow"
            }
        }
    }

    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: 0) {
                List(Pane.allCases, selection: $pane) { p in Label(p.rawValue, systemImage: p.symbol).tag(p) }
                    .listStyle(.sidebar).frame(width: 190)
                Divider()
                Form { detail }.formStyle(.grouped).frame(maxWidth: .infinity, maxHeight: .infinity)
            }
            Divider()
            HStack { Spacer(); Button("Done") { dismiss() }.keyboardShortcut(.defaultAction) }.padding(10)
        }
        .frame(width: 640, height: 420)
    }

    private var locked: Bool { model.isRunning }

    @ViewBuilder private var detail: some View {
        let r = model.record
        switch pane {
        case .general:
            LabeledContent("Name", value: r.name)
            LabeledContent("Product", value: r.product.name)
        case .system:
            LabeledContent("Model", value: r.product.name)
            if let t = r.topology { LabeledContent("Topology", value: t) }
            LabeledContent("Nodes", value: "\(r.nodes)")
            LabeledContent("Memory", value: r.memory)
        case .processor:
            LabeledContent("Memory", value: r.memory)
            Text(locked ? "Power Off to change these." : "Editing arrives with the core's settings API.").foregroundStyle(.secondary)
        case .input:
            Picker("Pointer", selection: $model.pointerMode) {
                Text("Follow my mouse").tag(PointerMode.follow)
                Text("Capture on click").tag(PointerMode.capture)
            }
            .pickerStyle(.radioGroup)
            Text("Press ⌃⌥ to give the mouse back after a capture.").foregroundStyle(.secondary)
        }
    }
}

// MARK: preferences

struct PreferencesView: View {
    @EnvironmentObject var app: AppModel
    @AppStorage("pointerMode") private var pointerMode = PointerMode.follow.rawValue

    var body: some View {
        TabView {
            Form {
                Section("Library folder") {
                    Text((app.root.path as NSString).abbreviatingWithTildeInPath)
                    Button("Choose Library Folder…") { app.invoke(.chooseLibrary) }
                }
            }
            .formStyle(.grouped).tabItem { Label("General", systemImage: "gearshape") }

            Form {
                Picker("New machines", selection: $pointerMode) {
                    Text("Follow my mouse").tag(PointerMode.follow.rawValue)
                    Text("Capture on click").tag(PointerMode.capture.rawValue)
                }
                .pickerStyle(.radioGroup)
                LabeledContent("Release the mouse", value: "⌃⌥")
            }
            .formStyle(.grouped).tabItem { Label("Input", systemImage: "cursorarrow") }
        }
        .frame(width: 480, height: 260)
    }
}
