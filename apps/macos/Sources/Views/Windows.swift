// The secondary windows: Diagnostics, a machine's Settings sheet and the
// app's Preferences.

import SwiftUI

// MARK: Diagnostics

@MainActor
func diagnosticsReport(_ model: MachineModel) -> String {
    guard let s = model.session else {
        return "Machine: \(model.name)\nStatus: \(model.word)\nThe machine has not been opened by this app."
    }
    return s.machine.diagnosticsReport(status: model.word, updates: UInt64(s.wakes), shown: UInt64(model.view?.presented ?? 0))
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
                        Circle().fill(model.tone.colour).frame(width: 8, height: 8).accessibilityHidden(true)
                        Text(model.word).foregroundStyle(.secondary)
                        Spacer()
                        Button(ui(.copyReport)) {
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
            Text(ui(.selectAMachineFirst)).foregroundStyle(.secondary).frame(minWidth: 560, minHeight: 320)
        }
    }
}

// MARK: a machine's settings

struct MachineSettings: View {
    @ObservedObject var model: MachineModel
    @EnvironmentObject var app: AppModel
    @Environment(\.dismiss) private var dismiss
    @State private var pane = Pane.general
    @StateObject private var edit: SettingsModel
    @State private var forwardName = ""
    @State private var forwardHost = ""
    @State private var forwardGuest = ""
    @State private var forwardUDP = false
    @State private var adding: ImageChoice?

    /// `edit` and `pane` let the documentation pictures open on a prepared pane.
    init(model: MachineModel, pane: Pane = .general, edit: SettingsModel? = nil) {
        _model = ObservedObject(wrappedValue: model)
        _pane = State(initialValue: pane)
        _edit = StateObject(wrappedValue: edit ?? SettingsModel(machine: model))   // an autoclosure: built once, not per render
    }

    static let sidebarWidth: CGFloat = 190

    enum Pane: String, CaseIterable, Identifiable {
        case general = "General", processor = "Processor & memory", graphics = "Graphics",
             drives = "Drives", network = "Network", shares = "Shared folders", startup = "Startup & firmware", input = "Input"
        var id: String { rawValue }
        var symbol: String {
            switch self {
            case .general: return "slider.horizontal.3"
            case .processor: return "cpu"
            case .graphics: return "display"
            case .drives: return "internaldrive"
            case .network: return "network"
            case .shares: return "folder"
            case .startup: return "power"
            case .input: return "cursorarrow"
            }
        }
    }

    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: 0) {
                List(Pane.allCases, selection: $pane) { p in Label(p.rawValue, systemImage: p.symbol).tag(p) }
                    .listStyle(.sidebar).frame(width: Self.sidebarWidth)
                Divider()
                Form { detail }.formStyle(.grouped).frame(maxWidth: .infinity, maxHeight: .infinity)
            }
            Divider()
            HStack {
                if let e = edit.error { Text(e).font(.caption).foregroundStyle(.red).lineLimit(2) }
                else if locked {
                    Text(ui(.powerOffToChangeHardwareSettings)).font(.caption).foregroundStyle(.secondary)
                    Button(ui(.powerOff)) { dismiss(); app.invoke(.powerOff) }.controlSize(.small)
                }
                Spacer()
                Button(ui(.done)) { dismiss() }.keyboardShortcut(.defaultAction)
            }.padding(10)
        }
        .frame(width: 700, height: 460)
        .onAppear { edit.load() }
        .sheet(item: $edit.draft) { _ in ShareNameSheet(edit: edit) }
    }

    private var locked: Bool { model.isRunning }

    @ViewBuilder private var loadingRow: some View {
        if edit.error == nil { HStack { ProgressView().controlSize(.small); Text("Reading the machine file…").foregroundStyle(.secondary) } }
    }

    @ViewBuilder private func network(_ s: OrigamiMachineSettings) -> some View {
        let setup = origamiBridgedSetup()
        ForEach(Array(edit.notes.enumerated()), id: \.offset) { _, note in
            HStack {
                Text(note.shown ?? "").font(.callout).fixedSize(horizontal: false, vertical: true)
                Spacer()
                if let command = note.copy {
                    Button(ui(.copy)) { NSPasteboard.general.clearContents(); NSPasteboard.general.setString(command, forType: .string) }
                }
            }
            .padding(8).background(RoundedRectangle(cornerRadius: 6).fill(Color.accentColor.opacity(0.12)))
        }
        Picker(ui(.mode), selection: Binding(get: { edit.networkMode }, set: { if let m = $0 { edit.chooseNetwork(m) } })) {
            ForEach(edit.networkChoices, id: \.label) { c in
                VStack(alignment: .leading) { Text(c.label); Text(c.detail).font(.caption).foregroundStyle(.secondary) }.tag(Optional(c.mode))
            }
        }
        .pickerStyle(.radioGroup).disabled(locked || edit.working)
        if edit.networkMode == nil { Text("Currently \(s.network.mode)").font(.caption).foregroundStyle(.secondary) }
        if edit.bridging {
            Section(ui(.bridged)) {
                // Return in either field applies Bridged with both; there is no Apply button.
                VStack(alignment: .leading, spacing: 2) {
                    TextField(setup.field, text: $edit.bridgeDevice, prompt: Text(setup.example)).disabled(locked || edit.working)
                        .onSubmit { edit.applyBridged() }
                    Text(setup.fieldNote).font(.caption).foregroundStyle(.secondary)
                }
                VStack(alignment: .leading, spacing: 2) {
                    TextField(ui(.addressForSharedFolders), text: $edit.bridgeShare, prompt: Text(setup.shareExample)).disabled(locked || edit.working)
                        .onSubmit { edit.applyBridged() }
                    Text(setup.shareNote).font(.caption).foregroundStyle(.secondary)
                }
                DisclosureGroup("How to set this up") {
                    Text(setup.steps).font(.callout).textSelection(.enabled).fixedSize(horizontal: false, vertical: true)
                    if let commands = setup.commands {
                        Button(ui(.copy)) { NSPasteboard.general.clearContents(); NSPasteboard.general.setString(commands, forType: .string) }
                    }
                }
                ForEach(s.network.warnings, id: \.self) { Text($0).font(.caption).foregroundStyle(.orange) }
            }
        }
        LabeledContent(ui(.ethernetAddress)) {
            VStack(alignment: .trailing, spacing: 2) {
                HStack {
                    Text(s.network.mac ?? "").textSelection(.enabled)
                    if let mac = s.network.mac {
                        Button(ui(.copy)) { NSPasteboard.general.clearContents(); NSPasteboard.general.setString(mac, forType: .string) }
                    }
                }
                Text(s.network.macNote).font(.caption).foregroundStyle(.secondary)
            }
        }
        if s.network.mode == "user" || s.network.mode == "private" {
            DisclosureGroup("Advanced") {
                HStack {
                    TextField("Private subnet", text: $edit.subnetText).onSubmit { edit.applySubnet() }
                    Button("Set") { edit.applySubnet() }
                }.disabled(locked || edit.working)
            }
        }
        Section(ui(.portForwards)) {
            ForEach(s.forwards, id: \.name) { f in
                HStack {
                    Text("\(f.name): \(f.protocol) host \(f.hostPort) to guest \(f.guestPort)")
                    Spacer()
                    Button { edit.apply(.removeForward(name: f.name)) } label: { Image(systemName: "minus.circle") }
                        .buttonStyle(.borderless).accessibilityLabel("Remove forward \(f.name)").disabled(locked || edit.working)
                }
            }
            HStack {
                TextField(ui(.name), text: $forwardName)
                TextField(ui(.hostPort), text: $forwardHost).frame(width: 80)
                TextField(ui(.guestPort), text: $forwardGuest).frame(width: 80)
                Picker("", selection: $forwardUDP) { Text("TCP").tag(false); Text("UDP").tag(true) }.labelsHidden().frame(width: 70)
                Button(ui(.add)) {
                    if let h = UInt16(forwardHost), let g = UInt16(forwardGuest), !forwardName.trimmingCharacters(in: .whitespaces).isEmpty {
                        edit.apply(.addForward(forward: OrigamiForward(name: forwardName.trimmingCharacters(in: .whitespaces),
                                                                       protocol: forwardUDP ? "udp" : "tcp", hostPort: h, guestPort: g)))
                        forwardName = ""; forwardHost = ""; forwardGuest = ""
                    }
                }
                .disabled(locked || edit.working || UInt16(forwardHost) == nil || UInt16(forwardGuest) == nil
                          || forwardName.trimmingCharacters(in: .whitespaces).isEmpty)
                .accessibilityLabel(ui(.addAPortForward)).help(ui(.addAPortForward))
            }
        }
    }

    @ViewBuilder private func sharesPane(_ s: OrigamiMachineSettings) -> some View {
        if s.shares.isEmpty {
            let e = origamiEmptyState(kind: .noSharedFolders)
            ContentUnavailableView { Text(e.title).font(.headline) } description: { Text(e.sentence) }
        }
        ForEach(s.shares, id: \.name) { sh in
            VStack(alignment: .leading, spacing: 4) {
                HStack {
                    VStack(alignment: .leading) { Text(sh.name); Text(sh.hostDir).font(.caption).foregroundStyle(.secondary).lineLimit(1).truncationMode(.middle) }
                    Spacer()
                    Button { edit.apply(.removeShare(name: sh.name)) } label: { Image(systemName: "minus.circle") }
                        .buttonStyle(.borderless).accessibilityLabel("\(ui(.stopSharing)) \(sh.name)").disabled(locked || edit.working)
                }
                if let problem = sh.mountProblem {
                    Text(problem).font(.caption).foregroundStyle(.orange)
                    if sh.needsAddress { Button(ui(.setTheAddressInNetwork)) { pane = .network }.buttonStyle(.link) }
                } else {
                    HStack(alignment: .top) {
                        Text(sh.mountLines.joined(separator: "\n")).font(.system(.caption, design: .monospaced)).textSelection(.enabled)
                        Spacer()
                        Button(ui(.copy)) { NSPasteboard.general.clearContents(); NSPasteboard.general.setString(sh.mountLines.joined(separator: "\n"), forType: .string) }
                            .accessibilityLabel("Copy mount commands for \(sh.name)")
                    }
                }
            }
        }
        Button(ui(.shareAFolder)) { if let dir = PathPicker.chooseFolder(ui(.choose)) { edit.chooseShareFolder(dir) } }.disabled(locked || edit.working)
        if !s.shares.isEmpty {
            Text(ui(.runTheseAsRootIn)).font(.caption).foregroundStyle(.secondary)
        }
    }

    @ViewBuilder private func drivesPane(_ s: OrigamiMachineSettings) -> some View {
        if s.drives.isEmpty {
            ContentUnavailableView { Text(ui(.drives)).font(.headline) }
        } else if edit.driveRows.isEmpty {
            HStack { ProgressView().controlSize(.small); Text(ui(.reading)).foregroundStyle(.secondary) }
        }
        ForEach(edit.driveRows, id: \.name) { row in
            let kind = s.drives.first { $0.name == row.name }?.kind
            HStack {
                VStack(alignment: .leading) {
                    Text(row.title)
                    Text(row.caption).font(.caption).foregroundStyle(.secondary).lineLimit(1).truncationMode(.middle)
                }
                Spacer()
                if kind == "cdrom" {
                    // A CD changes while the machine runs, unlike the rest of the pane.
                    Button(ui(.changeCd)) { if let u = PathPicker.choose(ui(.choose)) { edit.changeMedia(row.name, to: u) } }
                        .disabled(edit.working).accessibilityLabel("\(ui(.changeCd)) \(row.title)")
                    Button(ui(.eject)) { edit.changeMedia(row.name, to: nil) }.disabled(edit.working).accessibilityLabel("\(ui(.eject)) \(row.title)")
                }
                Button { if app.confirmed(.detachDrive(name: row.title)) { edit.apply(.detachDrive(name: row.name)) } } label: { Image(systemName: "minus.circle") }
                    .buttonStyle(.borderless).help(ui(.detach)).accessibilityLabel("\(ui(.detach)) \(row.title)")
                    .disabled(locked || edit.working)
            }
        }
        let diskTarget = locked ? nil : s.diskTarget
        let cdTarget = locked ? nil : s.cdTarget
        HStack {
            Button(ui(.addADiskImage)) {
                let choice = ImageChoice(core: { [model] in model.coreHost.core }, machine: model.record.key)
                choice.choose()
                if choice.path != nil { adding = choice }
            }.disabled(diskTarget == nil || edit.working)
            if !s.drives.contains(where: { $0.kind == "cdrom" }) {
                Button(ui(.insertACdImage)) {
                    if let u = PathPicker.choose(ui(.choose)), let t = cdTarget { edit.attachCD(u, target: t) }
                }.disabled(cdTarget == nil || edit.working)
            }
        }
        .sheet(isPresented: Binding(get: { adding != nil }, set: { if !$0 { adding = nil } })) {
            if let choice = adding, let target = diskTarget { AddDiskSheet(choice: choice, edit: edit, target: target) }
        }
    }

    @ViewBuilder private var detail: some View {
        let r = model.record
        switch pane {
        case .general:
            HStack { MachineArt(product: r.product, pt: Int(Tokens.Size.artSummary)); Text(r.name).font(.title2.weight(.semibold)) }
            LabeledContent(ui(.name), value: r.name)
            LabeledContent(ui(.model), value: r.product.name)
            if r.nodes > 1 { LabeledContent(ui(.nodes), value: "\(r.nodes)") }
            LabeledContent("Memory", value: r.memory)
        case .processor:
            if let s = edit.settings {
                LabeledContent(ui(.processor), value: "\(s.processor) \(s.processorPart)")
                if s.nodes > 1 { LabeledContent(ui(.nodes), value: "\(s.nodes)") }
                Picker(ui(.processorsPerNode), selection: Binding(get: { Int(s.cpusPerNode) }, set: { edit.apply(.setProcessor(cpusPerNode: UInt32($0))) })) {
                    ForEach(s.cpusPerNodeAllowed.map { Int($0) }, id: \.self) { Text("\($0)").tag($0) }
                }.disabled(locked || edit.working)
                Picker(ui(.memoryPerNode), selection: Binding(get: { Int(s.memoryPerNodeMib) }, set: { edit.apply(.setMemory(mib: UInt32($0))) })) {
                    ForEach(s.memoryAllowedMib.map { Int($0) }, id: \.self) { Text("\($0) MiB").tag($0) }
                }.disabled(locked || edit.working)
            } else { loadingRow }
        case .graphics:
            if let s = edit.settings {
                if s.pipes.isEmpty { LabeledContent(ui(.graphics), value: "None") }
                ForEach(s.pipes, id: \.slot) { LabeledContent("Pipe in slot \($0.slot)", value: "\($0.product) (\($0.family))") }
            } else { loadingRow }
        case .drives:
            if let s = edit.settings { drivesPane(s) } else { loadingRow }
        case .network:
            if let s = edit.settings { network(s) } else { loadingRow }
        case .shares:
            if let s = edit.settings { sharesPane(s) } else { loadingRow }
        case .startup:
            if let s = edit.settings {
                Section(ui(.startupFirmware)) {
                    LabeledContent(ui(.startupMessagesOn), value: edit.startup ?? "")
                    Picker(ui(.consoleSerialLine), selection: Binding(get: { s.consolePort }, set: { edit.setConsole($0) })) {
                        ForEach(s.consolePorts, id: \.name) { Text($0.label).tag($0.name) }
                    }.disabled(locked || edit.working)
                    Text(ui(.theConsoleWindowAndA))
                        .font(.caption).foregroundStyle(.secondary)
                }
            } else { loadingRow }
        case .input:
            Picker(ui(.pointer), selection: $model.pointerMode) {
                Text(ui(.followMyMouse)).tag(PointerMode.follow)
                Text(ui(.captureOnClick)).tag(PointerMode.capture)
            }
            .pickerStyle(.radioGroup)
            Text("Press ⌃⌥ to give the mouse back after a capture.").foregroundStyle(.secondary)
        }
    }
}

/// Name a folder before it is shared: what IRIX will call it.
struct ShareNameSheet: View {
    @ObservedObject var edit: SettingsModel
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            Text("Share a folder").font(.headline)
            TextField(ui(.nameInIrix), text: Binding(get: { edit.draft?.name ?? "" }, set: { edit.draft?.name = $0 }))
            LabeledContent(ui(.folder), value: edit.draft?.dir.path ?? "")
            if let p = edit.draftProblem { Text(p).foregroundStyle(.red).font(.callout).fixedSize(horizontal: false, vertical: true) }
            HStack {
                Spacer()
                Button(ui(.cancel)) { edit.draft = nil }.keyboardShortcut(.cancelAction)
                Button(ui(.add)) { edit.addDraft() }.keyboardShortcut(.defaultAction).disabled(edit.draftProblem != nil || edit.working)
            }
        }
        .padding(20).frame(width: 440)
    }
}

/// Add a chosen disk image to the machine, copied into its folder or used in place.
struct AddDiskSheet: View {
    @ObservedObject var choice: ImageChoice
    @ObservedObject var edit: SettingsModel
    let target: UInt32
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            Text(ui(.addADiskImage).trimmingCharacters(in: ["…"])).font(.headline)
            ImageChoiceView(choice: choice)
            if let p = edit.progress { OperationProgress(progress: p) { edit.cancelCopy() } }
            if let e = edit.error { Text(e).foregroundStyle(.red).font(.callout).fixedSize(horizontal: false, vertical: true) }
            HStack {
                Spacer()
                Button(ui(.cancel)) { if edit.working { edit.cancelCopy() } else { dismiss() } }.keyboardShortcut(.cancelAction)
                Button(ui(.add)) { edit.addDisk(choice, target: target) { ok in if ok { dismiss() } } }
                    .keyboardShortcut(.defaultAction).disabled(!choice.ready || edit.working)
            }
        }
        .padding(20).frame(width: 460)
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
                    Button("Choose library folder…") { app.invoke(.chooseLibrary) }
                }
            }
            .formStyle(.grouped).tabItem { Label(ui(.general), systemImage: "gearshape") }

            Form {
                Picker(ui(.pointer), selection: $pointerMode) {
                    Text(ui(.followMyMouse)).tag(PointerMode.follow.rawValue)
                    Text(ui(.captureOnClick)).tag(PointerMode.capture.rawValue)
                }
                .pickerStyle(.radioGroup)
                LabeledContent("Release the mouse", value: "⌃⌥")
                Toggle("Use Option as Meta in the console", isOn: $optionAsMeta)
            }
            .formStyle(.grouped).tabItem { Label(ui(.input), systemImage: "cursorarrow") }
            .onChange(of: pointerMode) { _ in app.defaultPointerChanged() }

            DownloadedDisksView().tabItem { Label(ui(.storage), systemImage: "internaldrive") }
        }
        .frame(width: 480, height: 260)
    }
}

/// The ready-made disks downloaded so far, each with Remove.
struct DownloadedDisksView: View {
    @EnvironmentObject var app: AppModel
    @State private var disks: [OrigamiCachedDisk] = []

    var body: some View {
        Form {
            Section(ui(.downloadedDisks)) {
                if disks.isEmpty {
                    let e = origamiEmptyState(kind: .noDownloadedDisks)
                    ContentUnavailableView { Text(e.title).font(.headline) } description: { Text(e.sentence) }
                }
                ForEach(disks, id: \.key) { d in
                    HStack {
                        VStack(alignment: .leading) {
                            Text(d.label)
                            Text(origamiSizeLabel(bytes: d.bytes)).font(.caption).foregroundStyle(.secondary)
                        }
                        Spacer()
                        Button(ui(.remove)) {
                            guard app.confirmed(.removeDownload(name: d.label)) else { return }
                            try? app.host?.core.removeCachedDisk(key: d.key)
                            reload()
                        }
                    }
                }
            }
        }
        .formStyle(.grouped).onAppear(perform: reload)
    }

    private func reload() { disks = (try? app.host?.core.cachedDisks()) ?? [] }
}
