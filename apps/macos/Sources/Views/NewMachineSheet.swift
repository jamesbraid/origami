// The New Machine sheet: a grouped form with Machine, Disk and Advanced; errors stay under their field.

import SwiftUI

struct NewMachineSheet: View {
    @StateObject private var model: NewMachineModel
    @Environment(\.dismiss) private var dismiss

    init(app: AppModel) { self.init(model: NewMachineModel(app: app)) }
    init(model: NewMachineModel) { _model = StateObject(wrappedValue: model) }

    var body: some View {
        VStack(spacing: 0) {
            if model.loading {
                ProgressView().controlSize(.small).frame(maxWidth: .infinity, maxHeight: .infinity)
            } else {
                Form {
                    machine
                    disk
                    advanced
                    if let p = model.progress { OperationProgress(progress: p) { model.cancelCopy() } }
                }
                .formStyle(.grouped)
            }
            HStack {
                Spacer()
                Button(ui(.cancel)) { dismiss() }.keyboardShortcut(.cancelAction)
                Button(ui(.create)) { create() }.keyboardShortcut(.defaultAction).disabled(!model.canCreate)
            }
            .padding(.horizontal, 20).padding(.vertical, 12)
        }
        .frame(width: 460, height: min(model.sheetHeight, Self.screenRoom))
        .onAppear { model.load() }
    }

    private func create() { model.create { ok in if ok { dismiss() } } }

    /// The most height the sheet may take: the screen less its menu bar and a margin.
    static var screenRoom: CGFloat { roomLimit ?? max(520, (NSScreen.main?.visibleFrame.height ?? 900) - 80) }
    /// The docs pictures ask for the whole sheet on a runner whose screen is small.
    static var roomLimit: CGFloat?

    /// A line under a field, red when it is the problem.
    private func under(_ text: String, bad: Bool = false) -> some View {
        Text(text).font(.caption).foregroundStyle(bad ? Color.red : Color.secondary).fixedSize(horizontal: false, vertical: true)
    }

    @ViewBuilder private var machine: some View {
        Section(ui(.machine)) {
            if model.canPickEngine {
                Picker(Labels.NewMachine.engine, selection: $model.engine) {
                    ForEach(model.engines, id: \.self) { Text(model.engineName($0)).tag(String?.some($0)) }
                }
            }
            Picker(Labels.NewMachine.model, selection: $model.selected) {
                ForEach(model.visible, id: \.self) { Text(model.label(model.offerings[$0])).tag($0) }
            }
            if model.offerings.indices.contains(model.selected) {
                let o = model.offerings[model.selected]
                let product = Product.of(model: o.product, label: o.product)
                HStack { Spacer(); NewMachineCard(product: Product.withArt(of: o)); Spacer() }
                    .accessibilityHidden(true).id(product.name + o.topology)
            }
            VStack(alignment: .leading, spacing: 4) {
                TextField(Labels.NewMachine.name, text: $model.name).onSubmit { create() }
                if let e = model.error { under(e, bad: true) }
            }
        }
    }

    @ViewBuilder private var disk: some View {
        Section(ui(.disk)) {
            Picker(Labels.NewMachine.disk, selection: $model.disk) {
                ForEach(model.options, id: \.title) { o in
                    VStack(alignment: .leading, spacing: 2) {
                        HStack(spacing: 6) {
                            Text(o.title)
                            if o.recommended { Text(ui(.recommended)).font(.caption).foregroundStyle(Color.accentColor) }
                        }
                        under(o.caption)
                    }
                    .tag(DiskChoice(o.choice))
                }
            }
            .pickerStyle(.radioGroup).labelsHidden()
            if model.disk == .ready {
                Picker(Labels.NewMachine.release, selection: $model.readyPick) {
                    ForEach(model.readyDisks.indices, id: \.self) { Text(model.readyDisks[$0].label).tag($0) }
                }
            }
            if model.disk == .empty {
                LabeledContent(ui(.diskSize)) {
                    HStack(spacing: 6) {
                        TextField(ui(.diskSize), value: $model.sizeGB, format: .number).labelsHidden()
                            .multilineTextAlignment(.trailing).frame(width: 70)
                        Text(ui(.gb))
                    }
                }
            }
            if model.disk == .image { ImageChoiceView(choice: model.image) }
        }
    }

    @ViewBuilder private var advanced: some View {
        Section {
            DisclosureGroup(isExpanded: $model.advancedOpen) {
                VStack(alignment: .leading, spacing: 4) {
                    TextField(ui(.machineId), text: $model.idText, prompt: Text("1 to 2999"))
                    if let next = model.nextId { under(origamiMachineIdCaption(next: next)) }
                    if let p = model.identityProblem(for: .id) { under(p, bad: true) }
                }
                Picker(ui(.ethernetAddress), selection: $model.useAddress) {
                    VStack(alignment: .leading, spacing: 2) {
                        Text(ui(.fromTheMachineId))
                        if let made = model.madeAddress { under(made) }
                    }.tag(false)
                    Text(ui(.custom)).tag(true)
                }
                .pickerStyle(.radioGroup)
                VStack(alignment: .leading, spacing: 4) {
                    TextField(ui(.address), text: $model.addressText, prompt: Text("08:00:69:__:__:__"))
                        .labelsHidden().disabled(!model.useAddress)
                    if let p = model.identityProblem(for: .address) { under(p, bad: true) }
                }
            } label: {
                // The whole header toggles, not only the arrow.
                Button { model.advancedOpen.toggle() } label: {
                    Text(Labels.NewMachine.advanced).frame(maxWidth: .infinity, alignment: .leading).contentShape(Rectangle())
                }
                .buttonStyle(.plain)
            }
        }
    }
}

/// The 120 x 90 point card for a product, or its glyph.
struct NewMachineCard: View {
    let product: Product
    @Environment(\.colorScheme) private var scheme
    var body: some View {
        if let id = product.art, let img = Art.image(id, "card", points: NSSize(width: 120, height: 90)) {
            Image(nsImage: img).resizable().interpolation(.high).frame(width: 120, height: 90)
        } else {
            ChassisGlyph(product: product).scaleEffect(2.5).frame(width: 120, height: 90)
        }
    }
}
