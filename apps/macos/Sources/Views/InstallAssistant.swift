// The Install IRIX set-up sheet, and the install guide that takes the sidebar once the install starts.

import AppKit
import SwiftUI

/// Set-up: release, discs, software, method, disk and check, as a sheet over the main window with the numbered
/// step list beside the page. Continue on the check starts the install and the guide takes over the sidebar.
struct InstallSetupSheet: View {
    @ObservedObject var model: InstallModel
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
        .frame(width: 740, height: 480)
        .interactiveDismissDisabled()
        .sheet(isPresented: $adding) { AddPackageSheet(model: model) }
    }

    /// Numbered text from the core's list: a check for what is done, the current one bold. No radio circles.
    private var stepList: some View {
        VStack(alignment: .leading, spacing: 10) {
            ForEach(model.flow.stepTitles(), id: \.number) { t in
                let current = t.state == .current
                HStack(alignment: .firstTextBaseline, spacing: 8) {
                    Group {
                        if t.state == .done { Image(systemName: "checkmark").foregroundStyle(Color.secondary) } else { Text("\(t.number)") }
                    }
                    .frame(width: 16)
                    Text(t.title).fontWeight(current ? .semibold : .regular)
                        .foregroundStyle(t.state == .later ? Color.secondary : Color.primary)
                }
                .accessibilityElement(children: .combine)
                .accessibilityAddTraits(current ? .isSelected : [])
            }
            Spacer()
        }
        .padding(16).frame(width: 200, alignment: .topLeading)
    }

    private var footer: some View {
        HStack {
            if model.flow.resumed() { Button(ui(.startOver)) { model.startOver() }.disabled(model.working) }
            // Continue says why it is off.
            if let reason = model.continueReason {
                Text(reason).font(.caption).foregroundStyle(.secondary).lineLimit(2).fixedSize(horizontal: false, vertical: true)
            }
            Spacer()
            Button(ui(.cancel)) { model.requestClose() }.keyboardShortcut(.cancelAction)
            Button(ui(.back)) { model.back() }.disabled(!model.canBack)
            Button(model.flow.continueLabel()) { model.next() }.keyboardShortcut(.defaultAction).disabled(!model.canContinue)
        }
        .padding(10)
    }

    @ViewBuilder private var content: some View {
        switch model.step {
        case .release: releaseStep
        case .media: mediaStep
        case .software: softwareStep
        case .disk: diskStep
        case .check: checkStep
        case .method: methodStep
        case .prepare, .installer, .packages, .finish: ProgressView().controlSize(.small)
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

    private var mediaStep: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text(ui(.whereShouldTheDiscsComeFrom)).font(.headline)
            RadioRow(title: ui(.downloadThem), detail: ui(.needsAnInternetConnection),
                     selected: !model.flow.useFolder()) { model.edit { $0.setUseFolder(on: false) } }
            RadioRow(title: ui(.useDiscsIAlreadyHave), detail: nil, selected: model.flow.useFolder()) { model.edit { $0.setUseFolder(on: true) } }
            if model.flow.useFolder() {
                VStack(alignment: .leading, spacing: 6) {
                    HStack {
                        Button(ui(.chooseFolder)) { model.chooseFolder() }
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
                    Button(ui(.remove)) { model.edit { $0.removeExtra(index: UInt32(i)) } }
                }
            }
            Button(ui(.addAPackage2)) { adding = true }
        }
    }

    @ViewBuilder private var diskStep: some View {
        VStack(alignment: .leading, spacing: 10) {
            if !model.diskKnown {
                ProgressView().controlSize(.small)
            } else if let d = model.disk {
                Text(d.text).fixedSize(horizontal: false, vertical: true)
                Toggle(ui(.eraseThisDiskAndInstall), isOn: Binding(get: { model.flow.erase() }, set: { on in model.edit { $0.setErase(on: on) } }))
                    .toggleStyle(.checkbox)
            } else {
                Text("This machine has no disk.")
                LabeledContent(ui(.diskSize)) {
                    HStack(spacing: 6) {
                        TextField(ui(.diskSize), value: Binding(get: { Int(model.flow.newDiskGb()) },
                                                                set: { n in model.edit { $0.setNewDiskGb(gb: UInt32(max(1, min(2048, n)))) } }),
                                  format: .number)
                            .labelsHidden().multilineTextAlignment(.trailing).frame(width: 70)
                        Text(ui(.gb))
                    }
                }
            }
        }
    }

    private var checkStep: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text(model.texts.checkIntro)
                .fixedSize(horizontal: false, vertical: true)
            Toggle(ui(.checkThoroughlyAlsoReadsThe), isOn: $model.full).toggleStyle(.checkbox)
                .disabled(model.flow.check() == .running)
            switch model.flow.check() {
            case .ready:
                Button(ui(.checkNow)) { model.runCheck() }
            case .running:
                ProgressView().progressViewStyle(.linear)
                HStack {
                    Text(model.flow.checkStatus() ?? "").font(.caption).foregroundStyle(.secondary)
                    Spacer()
                    Button(ui(.cancel)) { model.cancelCheck() }
                }
            case .passed:
                Label(model.flow.checkStatus() ?? "", systemImage: "checkmark.circle.fill").foregroundStyle(.green)
            case .failed:
                Text(model.checkMessage ?? "").foregroundStyle(.red).fixedSize(horizontal: false, vertical: true)
                Button(ui(.checkAgain)) { model.runCheck() }
            }
            ForEach(model.notes, id: \.self) { Text($0).font(.caption).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true) }
        }
    }

}


// MARK: the guide in the sidebar

/// The install guide: the steps from the PROM menu to the halt, one marked as the next to do, beside the
/// machine's screen or console. The core words it all.
struct InstallGuideSidebar: View {
    @ObservedObject var model: InstallModel

    var body: some View {
        VStack(spacing: 0) {
            ScrollView {
                VStack(alignment: .leading, spacing: 10) {
                    let steps = model.listedSteps
                    Text(origamiGuideHeader(step: UInt32(min(model.progress + 1, max(1, steps.count))), total: UInt32(steps.count)))
                        .font(.caption).foregroundStyle(.secondary)
                    if model.stoppedBySelf {
                        HStack {
                            Label(ui(.theMachineStopped), systemImage: "exclamationmark.triangle.fill").foregroundStyle(.orange)
                            Button(ui(.startAgain)) { model.startMachine() }
                        }
                    }
                    if steps.isEmpty && model.error == nil { ProgressView().controlSize(.small) }
                    ForEach(steps.indices, id: \.self) { i in
                        GuideStepRow(model: model, index: i, step: steps[i])
                    }
                    if model.step == .packages && model.flow.automatic() { automatic }
                    if let e = model.error { Text(e).font(.caption).foregroundStyle(.red).fixedSize(horizontal: false, vertical: true) }
                }
                .padding(12).frame(maxWidth: .infinity, alignment: .leading)
            }
            Divider()
            footer
        }
    }

    /// The automatic method: Origami types the package commands once the installer is at Inst>.
    private var automatic: some View {
        let running = model.flow.apply() == .running
        return VStack(alignment: .leading, spacing: 8) {
            Text(origamiInstallStepTitle(step: .packages)).fontWeight(.semibold)
            Text(model.flow.applyHint()).font(.callout).fixedSize(horizontal: false, vertical: true)
            if running {
                if let f = model.applyFraction { ProgressView(value: f) } else { ProgressView().progressViewStyle(.linear) }
                if !model.applyLine.isEmpty { Text(model.applyLine).font(.caption).foregroundStyle(.secondary).lineLimit(1) }
            }
            if let m = model.applyMessage { Text(m).font(.callout).foregroundStyle(.red).fixedSize(horizontal: false, vertical: true) }
            HStack {
                Button(ui(.install)) { model.install() }.disabled(!model.flow.canInstall(machineUp: model.machine.isRunning))
                if running { Button(ui(.cancelInstall)) { model.cancelInstall() } }
                if model.flow.canFixed() { Button(ui(.iFixedItAtInst)) { model.fixed() } }
            }
        }
    }

    private var footer: some View {
        VStack(alignment: .leading, spacing: 6) {
            if let failed = model.serveFailed {
                HStack { Text(failed).font(.caption).foregroundStyle(.red).lineLimit(2); Button(ui(.restart)) { model.restartServe() } }
            } else if let text = model.installFooter {
                Text(text).font(.caption).foregroundStyle(.secondary).lineLimit(2)
            }
            if let reason = model.continueReason, model.step == .finish || (model.step == .packages && model.flow.automatic()) {
                Text(reason).font(.caption).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
            }
            HStack {
                Button(ui(.cancel)) { model.requestClose() }
                Spacer()
                // Continue only where the guide cannot tell the step is over: the automatic install, and Finish.
                if model.step == .finish || (model.step == .packages && model.flow.automatic()) {
                    Button(model.flow.continueLabel()) { model.next() }.keyboardShortcut(.defaultAction).disabled(!model.canContinue)
                }
            }
        }
        .padding(10)
    }
}

/// One guide step: done, the next to do (with what to watch for and type) or still to come.
struct GuideStepRow: View {
    @ObservedObject var model: InstallModel
    let index: Int
    let step: OrigamiGuideStep

    var body: some View {
        let done = model.isDone(index), current = index == model.progress
        HStack(alignment: .top, spacing: 8) {
            Group {
                if done { Image(systemName: "checkmark.circle.fill").foregroundStyle(Color.green) }
                else { Text("\(index + 1)").foregroundStyle(current ? Color.accentColor : Color.secondary) }
            }
            .frame(width: 18)
            VStack(alignment: .leading, spacing: 6) {
                Text(step.title).fontWeight(current ? .semibold : .regular)
                    .foregroundStyle(done || !current ? Color.secondary : Color.primary)
                    .fixedSize(horizontal: false, vertical: true)
                if done {
                    Text(model.serial && index < model.seen ? ui(.seen) : ui(.done)).font(.caption).foregroundStyle(.secondary)
                }
                if current { detail }
            }
        }
        .accessibilityElement(children: .combine)
        .accessibilityAddTraits(current ? .isSelected : [])
    }

    /// What to watch for and type, a note, and how the step is finished.
    @ViewBuilder private var detail: some View {
        if let watch = step.watch {
            line(ui(.watchFor)) { code(watch) }
        }
        ForEach(Array(step.typed.enumerated()), id: \.offset) { _, t in
            line(ui(.typeLabel)) {
                HStack(spacing: 6) {
                    if let p = t.prompt, p != step.watch { Text(p).font(.caption).foregroundStyle(.secondary) }
                    code(t.text)
                    if t.text != "Return" {
                        Button(ui(.copy)) { NSPasteboard.general.clearContents(); NSPasteboard.general.setString(t.text, forType: .string) }
                            .controlSize(.small)
                    }
                }
            }
        }
        if let note = step.note {
            Label { Text(note.text).font(.caption) } icon: {
                Image(systemName: note.warning ? "exclamationmark.triangle.fill" : "info.circle")
                    .foregroundStyle(note.warning ? Color.orange : Color.secondary)
            }
            .fixedSize(horizontal: false, vertical: true)
        }
        HStack {
            if let send = step.send, model.canTypeIt { Button(ui(.typeIt)) { model.typeIt(send) }.controlSize(.small) }
            if model.serial, step.confirm != nil {
                if let watch = step.watch { Text(origamiWaitingFor(prompt: watch)).font(.caption).foregroundStyle(.secondary) }
            } else {
                // A graphics console cannot confirm a step, and a step with nothing to see ends by hand.
                Button(ui(.done)) { model.markDone(index) }.controlSize(.small)
            }
        }
    }

    private func line<C: View>(_ label: String, @ViewBuilder _ value: () -> C) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            Text(label).font(.caption).foregroundStyle(.secondary).frame(width: 56, alignment: .leading)
            value()
        }
    }

    private func code(_ text: String) -> some View {
        Text(text).font(.system(.caption, design: .monospaced)).textSelection(.enabled)
            .padding(.horizontal, 6).padding(.vertical, 2)
            .background(RoundedRectangle(cornerRadius: 4).fill(Color.secondary.opacity(0.15)))
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
            Text(ui(.addAPackage)).font(.headline)
            HStack {
                TextField(ui(.sourceAFileOrAn), text: $source)
                Button(ui(.chooseFile)) { if let u = model.choosePath(ui(.chooseFile), false) { source = u.path } }
            }
            Text(ui(.packagesToInstallOnePerLine)).font(.caption).foregroundStyle(.secondary)
            TextEditor(text: $packages).font(.system(.body, design: .monospaced)).frame(height: 90)
                .overlay(RoundedRectangle(cornerRadius: 4).stroke(Color.secondary.opacity(0.4)))
            HStack {
                Spacer()
                Button(ui(.cancel)) { dismiss() }.keyboardShortcut(.cancelAction)
                Button(ui(.add)) { model.addExtra(source: source, packages: packages); dismiss() }
                    .keyboardShortcut(.defaultAction).disabled(source.trimmingCharacters(in: .whitespaces).isEmpty)
            }
        }
        .padding(20).frame(width: 460)
    }
}
