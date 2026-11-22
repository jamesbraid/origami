// The Install IRIX window: a step list, the step's content and a footer with the media server's line.

import AppKit
import SwiftUI

/// One window per machine, opened by the machine's key.
struct InstallWindow: View {
    @EnvironmentObject var app: AppModel
    let key: String
    /// Kept once found, so a window that is closing does not ask the app for a new assistant.
    @State private var held: InstallModel?

    var body: some View {
        if let model = held ?? app.installModel(for: key) {
            InstallAssistant(model: model).navigationTitle("Install IRIX: \(model.machine.name)")
                .onAppear { held = model }
        } else {
            Text("This machine is no longer in the library.").foregroundStyle(.secondary).frame(width: 360, height: 120)
        }
    }
}

struct InstallAssistant: View {
    @ObservedObject var model: InstallModel
    @Environment(\.dismiss) private var dismiss
    @State private var adding = false

    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: 0) {
                stepList
                Divider()
                VStack(alignment: .leading, spacing: 12) {
                    Text(origamiInstallStepTitle(step: model.step)).font(.title2.weight(.semibold))
                    ScrollView { content.frame(maxWidth: .infinity, alignment: .leading) }
                    if let e = model.error {
                        Text(e).font(.callout).foregroundStyle(.red).fixedSize(horizontal: false, vertical: true)
                    }
                }
                .padding(20).frame(maxWidth: .infinity, maxHeight: .infinity)
            }
            Divider()
            footer
        }
        .frame(width: 780, height: 540)
        .background(CloseGuard(model: model).frame(width: 0, height: 0))
        .onAppear { model.onClose = { dismiss() } }
        .sheet(isPresented: $adding) { AddPackageSheet(model: model) }
    }

    private var stepList: some View {
        VStack(alignment: .leading, spacing: 10) {
            let steps = model.flow.steps(), here = steps.firstIndex(of: model.step) ?? 0
            ForEach(steps.indices, id: \.self) { i in
                let current = i == here
                HStack(spacing: 8) {
                    Image(systemName: i < here ? "checkmark.circle.fill" : current ? "circle.inset.filled" : "circle")
                        .foregroundStyle(current ? Color.accentColor : Color.secondary)
                    Text("\(i + 1). \(origamiInstallStepTitle(step: steps[i]))").fontWeight(current ? .semibold : .regular)
                }
                .accessibilityElement(children: .combine)
                .accessibilityAddTraits(current ? .isSelected : [])
            }
            Spacer()
        }
        .padding(16).frame(width: 210, alignment: .topLeading)
    }

    // MARK: footer

    private var footer: some View {
        HStack {
            if let failed = model.serveFailed {
                Text(failed).font(.caption).foregroundStyle(.red).lineLimit(2)
                Button("Restart") { model.restartServe() }
            } else if !model.footer.isEmpty {
                Text(model.footer).font(.caption).foregroundStyle(.secondary).lineLimit(1)
            } else if let plan = model.planText {
                Text(plan).font(.caption).foregroundStyle(.secondary).lineLimit(1)
            }
            Spacer()
            if model.flow.resumed() { Button("Start Over") { model.startOver() }.disabled(model.working) }
            Button("Cancel") { model.requestClose() }.keyboardShortcut(.cancelAction)
            Button("Back") { model.back() }.disabled(!model.canBack)
            Button(model.flow.continueLabel()) { model.next() }.keyboardShortcut(.defaultAction).disabled(!model.canContinue)
        }
        .padding(10)
    }

    // MARK: steps

    @ViewBuilder private var content: some View {
        switch model.step {
        case .release: releaseStep
        case .media: mediaStep
        case .software: softwareStep
        case .disk: diskStep
        case .check: checkStep
        case .prepare: guideStep(model.guide?.prepare, section: "prepare")
        case .installer: guideStep(model.guide?.installer, section: "installer")
        case .method: methodStep
        case .packages:
            if model.flow.automatic() { automaticStep }
            else if model.flow.byHand() { guideStep(model.guide?.restart, section: "restart") }
            else { guideStep(model.guide.map { $0.packages + $0.restart }, section: "packages", note: model.guide?.packageNote) }
        case .finish:
            if model.flow.automatic() { Text("IRIX is installed and the machine is stopped.") }
            else { guideStep(model.guide?.finish, section: "finish") }
        }
    }

    private var releaseStep: some View {
        VStack(alignment: .leading, spacing: 10) {
            ForEach(model.flow.releases(), id: \.release) { r in
                RadioRow(title: r.label, detail: nil, selected: model.flow.release() == r.release) { model.selectRelease(r.release) }
                if model.flow.release() == r.release {
                    VStack(alignment: .leading, spacing: 8) {
                        ForEach(r.profiles, id: \.name) { p in
                            RadioRow(title: p.label, detail: p.detail, selected: model.flow.profile() == p.name) { model.selectProfile(p.name) }
                        }
                    }.padding(.leading, 24)
                }
            }
        }
    }

    private var methodStep: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text("How should the install run?")
            RadioRow(title: "Automatic", detail: "Origami types the package commands for you, logs in when they finish and shuts IRIX down.",
                     selected: model.flow.method() == .automatic) { model.edit { $0.setMethod(method: .automatic) } }
            RadioRow(title: "Guided", detail: "You type each command in the console. Origami shows each step.",
                     selected: model.flow.method() == .guided) { model.edit { $0.setMethod(method: .guided) } }
        }
    }

    private var automaticStep: some View {
        let running = model.flow.apply() == .running
        return VStack(alignment: .leading, spacing: 10) {
            Text(model.flow.applyHint()).fixedSize(horizontal: false, vertical: true)
            if running {
                if let f = model.applyFraction { ProgressView(value: f) } else { ProgressView().progressViewStyle(.linear) }
                if !model.applyLine.isEmpty { Text(model.applyLine).font(.caption).foregroundStyle(.secondary).lineLimit(1) }
            }
            if let m = model.applyMessage { Text(m).foregroundStyle(.red).fixedSize(horizontal: false, vertical: true) }
            HStack {
                Button("Install") { model.install() }.disabled(!model.flow.canInstall(machineUp: model.machine.isRunning))
                if running { Button("Cancel Install") { model.cancelInstall() } }
                if model.flow.canFixed() { Button("I fixed it at Inst>") { model.fixed() } }
                Button("Open Console") { model.openConsole() }
            }
        }
    }

    private var mediaStep: some View {
        VStack(alignment: .leading, spacing: 10) {
            RadioRow(title: "Download them from the Origami server", detail: model.texts.mediaDownload,
                     selected: !model.flow.useFolder()) { model.edit { $0.setUseFolder(on: false) } }
            RadioRow(title: model.texts.mediaFolder, detail: nil, selected: model.flow.useFolder()) { model.edit { $0.setUseFolder(on: true) } }
            if model.flow.useFolder() {
                VStack(alignment: .leading, spacing: 6) {
                    HStack {
                        Button("Choose Folder…") { model.chooseFolder() }
                        if let f = model.flow.folder() { Text(f).lineLimit(1).truncationMode(.middle).foregroundStyle(.secondary) }
                    }
                    if let v = model.folderVerdict {
                        Text(v).font(.callout).foregroundStyle(tone(model.folderTone)).fixedSize(horizontal: false, vertical: true)
                    }
                }.padding(.leading, 24)
            }
        }
    }

    private func tone(_ t: InstallModel.FolderTone) -> Color {
        switch t { case .green: return .green; case .amber: return .orange; case .red: return .red }
    }

    private var softwareStep: some View {
        VStack(alignment: .leading, spacing: 10) {
            ForEach(model.flow.addons(), id: \.choice.name) { a in
                let c = a.choice
                VStack(alignment: .leading, spacing: 2) {
                    Toggle(c.label, isOn: Binding(get: { a.on }, set: { on in model.edit { $0.tick(name: c.name, on: on) } }))
                        .toggleStyle(.checkbox).disabled(c.locked)
                    if let n = c.note { Text(n).font(.caption).foregroundStyle(.secondary).padding(.leading, 20) }
                }
            }
            ForEach(Array(model.flow.extras().enumerated()), id: \.offset) { i, x in
                HStack {
                    VStack(alignment: .leading) {
                        Text(x.source ?? "").lineLimit(1).truncationMode(.middle)
                        if !x.install.isEmpty { Text(x.install.joined(separator: ", ")).font(.caption).foregroundStyle(.secondary) }
                    }
                    Spacer()
                    Button("Remove") { model.edit { $0.removeExtra(index: UInt32(i)) } }
                }
            }
            Button("Add a Package…") { adding = true }
        }
    }

    @ViewBuilder private var diskStep: some View {
        VStack(alignment: .leading, spacing: 10) {
            if !model.diskKnown {
                ProgressView().controlSize(.small)
            } else if let d = model.disk {
                Text(d.text).fixedSize(horizontal: false, vertical: true)
                Toggle("Erase this disk and install onto it", isOn: Binding(get: { model.flow.erase() }, set: { on in model.edit { $0.setErase(on: on) } }))
                    .toggleStyle(.checkbox)
            } else {
                Text("This machine has no disk.")
                Stepper("Create a disk: \(model.flow.newDiskGb()) GB",
                        value: Binding(get: { Int(model.flow.newDiskGb()) }, set: { n in model.edit { $0.setNewDiskGb(gb: UInt32(n)) } }), in: 1...2048)
            }
        }
    }

    private var checkStep: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text(model.texts.checkIntro)
                .fixedSize(horizontal: false, vertical: true)
            Toggle("Check thoroughly (also reads the downloaded discs)", isOn: $model.full).toggleStyle(.checkbox)
                .disabled(model.flow.check() == .running)
            switch model.flow.check() {
            case .ready:
                Button("Check Now") { model.runCheck() }
            case .running:
                ProgressView().progressViewStyle(.linear)
                HStack {
                    Text(model.texts.checking).font(.caption).foregroundStyle(.secondary)
                    Spacer()
                    Button("Cancel") { model.cancelCheck() }
                }
            case .passed:
                Label(model.texts.passed, systemImage: "checkmark.circle.fill").foregroundStyle(.green)
            case .failed:
                Text(model.checkMessage ?? "").foregroundStyle(.red).fixedSize(horizontal: false, vertical: true)
                Button("Check Again") { model.runCheck() }
            }
            ForEach(model.notes, id: \.self) { Text($0).font(.caption).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true) }
        }
    }

    /// A guide section as a checklist; ticks never block Continue.
    @ViewBuilder private func guideStep(_ steps: [OrigamiGuideStep]?, section: String, note: String? = nil) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            if model.stoppedBySelf {
                HStack {
                    Label("The machine stopped.", systemImage: "exclamationmark.triangle.fill").foregroundStyle(.orange)
                    Button("Start Again") { model.startMachine() }
                }
            }
            if let steps {
                ForEach(steps.indices, id: \.self) { i in
                    let s = steps[i]
                    HStack(alignment: .top) {
                        Toggle(isOn: model.ticked(section, i)) {
                            Text(s.text).fixedSize(horizontal: false, vertical: true)
                        }.toggleStyle(.checkbox)
                        Spacer()
                        if let c = s.command {
                            Button("Copy") {
                                NSPasteboard.general.clearContents()
                                NSPasteboard.general.setString(c, forType: .string)
                            }
                        }
                        if let send = s.send, model.canTypeIt {
                            Button("Type It") { model.typeIt(send) }
                        }
                    }
                }
            } else if model.error == nil {
                ProgressView().controlSize(.small)
            }
            if let note { Text(note).font(.callout).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true) }
            if model.step != .finish {
                HStack {
                    Button("Show Machine") { model.showMachine() }
                    Button("Open Console") { model.openConsole() }
                }
            }
        }
    }
}

/// A round radio button with a title and, optionally, a line under it.
struct RadioRow: View {
    let title: String
    let detail: String?
    let selected: Bool
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            HStack(alignment: .top, spacing: 8) {
                Image(systemName: selected ? "largecircle.fill.circle" : "circle").foregroundStyle(selected ? Color.accentColor : Color.secondary)
                VStack(alignment: .leading, spacing: 2) {
                    Text(title)
                    if let detail { Text(detail).font(.caption).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true) }
                }
                Spacer(minLength: 0)
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityAddTraits(selected ? .isSelected : [])
    }
}

/// Add a package from a file or an HTTPS address, and say which of its selections to install.
struct AddPackageSheet: View {
    @ObservedObject var model: InstallModel
    @Environment(\.dismiss) private var dismiss
    @State private var source = ""
    @State private var packages = ""

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Add a Package").font(.headline)
            HStack {
                TextField("Source: a file or an HTTPS address", text: $source)
                Button("Choose File…") { if let u = model.choosePath("Choose a Package", false) { source = u.path } }
            }
            Text("Packages to install, one per line").font(.caption).foregroundStyle(.secondary)
            TextEditor(text: $packages).font(.system(.body, design: .monospaced)).frame(height: 90)
                .overlay(RoundedRectangle(cornerRadius: 4).stroke(Color.secondary.opacity(0.4)))
            HStack {
                Spacer()
                Button("Cancel") { dismiss() }.keyboardShortcut(.cancelAction)
                Button("Add") { model.addExtra(source: source, packages: packages); dismiss() }
                    .keyboardShortcut(.defaultAction).disabled(source.trimmingCharacters(in: .whitespaces).isEmpty)
            }
        }
        .padding(20).frame(width: 460)
    }
}

/// Lets the model veto the window's close button and Cmd-W, and tells it when the window is gone.
struct CloseGuard: NSViewRepresentable {
    let model: InstallModel

    func makeNSView(context: Context) -> GuardView { GuardView() }
    func updateNSView(_ view: GuardView, context: Context) { view.delegate.model = model }
}

final class GuardView: NSView {
    let delegate = CloseDelegate()

    override func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
        guard let window, window.delegate !== delegate else { return }
        delegate.previous = window.delegate
        window.delegate = delegate
    }
}

/// Answers the close questions and passes everything else to the delegate SwiftUI installed.
final class CloseDelegate: NSObject, NSWindowDelegate {
    weak var previous: NSWindowDelegate?
    weak var model: InstallModel?

    func windowShouldClose(_ sender: NSWindow) -> Bool {
        MainActor.assumeIsolated { model?.windowMayClose() ?? true }
    }

    func windowWillClose(_ notification: Notification) {
        MainActor.assumeIsolated {
            model?.close(dismissing: false)
            previous?.windowWillClose?(notification)
        }
    }

    override func responds(to selector: Selector!) -> Bool { super.responds(to: selector) || (previous?.responds(to: selector) ?? false) }
    override func forwardingTarget(for selector: Selector!) -> Any? { previous }
}
