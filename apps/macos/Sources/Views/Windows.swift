// The secondary windows: Diagnostics, a machine's Settings sheet and the
// app's Preferences.

import SwiftUI

// MARK: Diagnostics

@MainActor
func diagnosticsReport(_ model: MachineModel) -> String {
    var out = ["Machine: \(model.name) (\(model.record.product.name))", "State: \(model.state.pill)"]
    if let p = model.picture { out.append("Picture: \(p.width) x \(p.height)") }
    if let s = model.session {
        guard let v = model.coreView else { out.append("Waiting for the core."); return out.joined(separator: "\n") }
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
    @StateObject private var edit: SettingsModel
    @State private var forwardName = ""
    @State private var forwardHost = ""
    @State private var forwardGuest = ""
    @State private var forwardUDP = false

    init(model: MachineModel) {
        _model = ObservedObject(wrappedValue: model)
        _edit = StateObject(wrappedValue: SettingsModel(machine: model))   // an autoclosure: built once, not per render
    }

    enum Pane: String, CaseIterable, Identifiable {
        case general = "General", system = "System Information", processor = "Processor & Memory", graphics = "Graphics",
             drives = "Drives", network = "Network", input = "Input"
        var id: String { rawValue }
        var symbol: String {
            switch self {
            case .general: return "slider.horizontal.3"
            case .system: return "info.circle"
            case .processor: return "cpu"
            case .graphics: return "display"
            case .drives: return "internaldrive"
            case .network: return "network"
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
            HStack {
                if let e = edit.error { Text(e).font(.caption).foregroundStyle(.red).lineLimit(2) }
                else if locked { Text("This machine is running. Hardware settings are locked until it is powered off.").font(.caption).foregroundStyle(.secondary) }
                Spacer()
                Button("Done") { dismiss() }.keyboardShortcut(.defaultAction)
            }.padding(10)
        }
        .frame(width: 700, height: 460)
        .onAppear { edit.load() }
    }

    private var locked: Bool { model.isRunning }

    @ViewBuilder private var loadingRow: some View {
        if edit.error == nil { HStack { ProgressView().controlSize(.small); Text("Reading the machine file…").foregroundStyle(.secondary) } }
    }

    @ViewBuilder private func network(_ s: OrigamiMachineSettings) -> some View {
        let modes: [(OrigamiNetworkMode, String, String)] = [(.user, "user", "Shared (NAT)"), (.private, "private", "Private"), (.none, "none", "Off")]
        // tap and vmnet need an endpoint this sheet does not ask for: shown, never changed.
        let known = modes.contains { $0.1 == s.networkMode }
        Picker("Mode", selection: Binding(get: { s.networkMode }, set: { m in
            if let pick = modes.first(where: { $0.1 == m }) { edit.apply(.setNetwork(mode: pick.0)) }
        })) {
            ForEach(modes, id: \.1) { Text($0.2).tag($0.1) }
            if !known { Text(s.networkMode).tag(s.networkMode) }
        }.disabled(locked || edit.working || !known)
        if !known { Text("Currently \(s.networkMode)").font(.caption).foregroundStyle(.secondary) }
        Section("Port forwards") {
            ForEach(s.forwards, id: \.name) { f in
                HStack {
                    Text("\(f.name): \(f.protocol) host \(f.hostPort) to guest \(f.guestPort)")
                    Spacer()
                    Button { edit.apply(.removeForward(name: f.name)) } label: { Image(systemName: "minus.circle") }
                        .buttonStyle(.borderless).accessibilityLabel("Remove forward \(f.name)").disabled(locked || edit.working)
                }
            }
            HStack {
                TextField("Name", text: $forwardName)
                TextField("Host port", text: $forwardHost).frame(width: 80)
                TextField("Guest port", text: $forwardGuest).frame(width: 80)
                Picker("", selection: $forwardUDP) { Text("TCP").tag(false); Text("UDP").tag(true) }.labelsHidden().frame(width: 70)
                Button("Add") {
                    if let h = UInt16(forwardHost), let g = UInt16(forwardGuest), !forwardName.trimmingCharacters(in: .whitespaces).isEmpty {
                        edit.apply(.addForward(forward: OrigamiForward(name: forwardName.trimmingCharacters(in: .whitespaces),
                                                                       protocol: forwardUDP ? "udp" : "tcp", hostPort: h, guestPort: g)))
                        forwardName = ""; forwardHost = ""; forwardGuest = ""
                    }
                }
                .disabled(locked || edit.working || UInt16(forwardHost) == nil || UInt16(forwardGuest) == nil
                          || forwardName.trimmingCharacters(in: .whitespaces).isEmpty)
            }
        }
        Section("Shared folders") {
            ForEach(s.shares, id: \.name) { sh in
                HStack {
                    VStack(alignment: .leading) { Text(sh.name); Text(sh.hostDir).font(.caption).foregroundStyle(.secondary).lineLimit(1).truncationMode(.middle) }
                    Spacer()
                    Button { edit.apply(.removeShare(name: sh.name)) } label: { Image(systemName: "minus.circle") }
                        .buttonStyle(.borderless).accessibilityLabel("Stop sharing \(sh.name)").disabled(locked || edit.working)
                }
            }
            Button("Share a Folder…") {
                let p = NSOpenPanel()
                p.canChooseDirectories = true; p.canChooseFiles = false; p.prompt = "Share"
                if p.runModal() == .OK, let url = p.url { edit.apply(.addShare(name: url.lastPathComponent, hostDir: url.path)) }
            }.disabled(locked || edit.working)
        }
    }

    @ViewBuilder private var detail: some View {
        let r = model.record
        switch pane {
        case .general:
            HStack { MachineArt(product: r.product, pt: Int(Tokens.Size.artSummary)); Text(r.name).font(.title2.weight(.semibold)) }
            LabeledContent("Name", value: r.name)
            LabeledContent("Product", value: r.product.name)
        case .system:
            LabeledContent("Model", value: r.product.name)
            if let t = r.topology { LabeledContent("Topology", value: t) }
            LabeledContent("Nodes", value: "\(r.nodes)")
            LabeledContent("Memory", value: r.memory)
        case .processor:
            if let s = edit.settings {
                LabeledContent("Processor", value: "\(s.processor) \(s.processorPart)")
                LabeledContent("Nodes", value: "\(s.nodes)")
                Picker("Processors per node", selection: Binding(get: { Int(s.cpusPerNode) }, set: { edit.apply(.setProcessor(cpusPerNode: UInt32($0))) })) {
                    ForEach(s.cpusPerNodeAllowed.map { Int($0) }, id: \.self) { Text("\($0)").tag($0) }
                }.disabled(locked || edit.working)
                Picker("Memory per node", selection: Binding(get: { Int(s.memoryPerNodeMib) }, set: { edit.apply(.setMemory(mib: UInt32($0))) })) {
                    ForEach(s.memoryAllowedMib.map { Int($0) }, id: \.self) { Text("\($0) MiB").tag($0) }
                }.disabled(locked || edit.working)
            } else { loadingRow }
        case .graphics:
            if let s = edit.settings {
                if s.pipes.isEmpty { LabeledContent("Graphics", value: "None") }
                ForEach(s.pipes, id: \.slot) { LabeledContent("Pipe in slot \($0.slot)", value: "\($0.product) (\($0.family))") }
            } else { loadingRow }
        case .drives:
            if let s = edit.settings {
                if s.drives.isEmpty { LabeledContent("Drives", value: "None") }
                ForEach(s.drives, id: \.name) { d in
                    HStack {
                        VStack(alignment: .leading) {
                            Text("\(d.name) (\(d.kind), bus \(d.bus) target \(d.target))")
                            Text(d.image + (d.readOnly ? " (read-only)" : "")).font(.caption).foregroundStyle(.secondary).lineLimit(1).truncationMode(.middle)
                        }
                        Spacer()
                        Button { edit.apply(.detachDrive(name: d.name)) } label: { Image(systemName: "minus.circle") }
                            .buttonStyle(.borderless).help("Detach").accessibilityLabel("Detach \(d.name)")
                            .disabled(locked || edit.working)
                    }
                }
            } else { loadingRow }
        case .network:
            if let s = edit.settings { network(s) } else { loadingRow }
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
    @AppStorage(ConsoleKeys.optionAsMetaKey) private var optionAsMeta = false

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
                Picker("Pointer", selection: $pointerMode) {
                    Text("Follow my mouse").tag(PointerMode.follow.rawValue)
                    Text("Capture on click").tag(PointerMode.capture.rawValue)
                }
                .pickerStyle(.radioGroup)
                LabeledContent("Release the mouse", value: "⌃⌥")
                Toggle("Use Option as Meta in the console", isOn: $optionAsMeta)
            }
            .formStyle(.grouped).tabItem { Label("Input", systemImage: "cursorarrow") }
            .onChange(of: pointerMode) { _ in app.defaultPointerChanged() }
        }
        .frame(width: 480, height: 260)
    }
}
