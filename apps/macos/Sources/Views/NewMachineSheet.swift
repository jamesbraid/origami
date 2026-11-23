// The New Machine sheet: a model, a name, Create and Cancel; errors stay on the sheet.

import SwiftUI

struct NewMachineSheet: View {
    @StateObject private var model: NewMachineModel
    @Environment(\.dismiss) private var dismiss

    init(app: AppModel) { self.init(model: NewMachineModel(app: app)) }
    init(model: NewMachineModel) { _model = StateObject(wrappedValue: model) }

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            Text(ui(.newMachine)).font(.headline)
            if model.loading {
                ProgressView().controlSize(.small).frame(maxWidth: .infinity)
            } else {
                Picker(Labels.NewMachine.engine, selection: $model.engine) {
                    ForEach(model.engines, id: \.self) { Text(model.engineName($0)).tag(String?.some($0)) }
                }
                .disabled(!model.canPickEngine)
                Picker(Labels.NewMachine.model, selection: $model.selected) {
                    ForEach(model.visible, id: \.self) { Text(model.label(model.offerings[$0])).tag($0) }
                }
                if model.offerings.indices.contains(model.selected) {
                    let o = model.offerings[model.selected]
                    let product = Product.of(model: o.product, label: o.product)
                    HStack { Spacer(); NewMachineCard(product: Product.withArt(of: o)) ; Spacer() }
                        .accessibilityHidden(true).id(product.name + o.topology)
                }
                TextField(Labels.NewMachine.name, text: $model.name).onSubmit { create() }
                Picker(Labels.NewMachine.disk, selection: $model.disk) { ForEach(model.choices) { Text($0.title).tag($0) } }
                if model.disk == .ready {
                    Picker(Labels.NewMachine.release, selection: $model.readyPick) {
                        ForEach(model.readyDisks.indices, id: \.self) { Text(model.readyDisks[$0].label).tag($0) }
                    }
                }
                if model.disk == .empty {
                    Stepper("Disk size: \(model.sizeGB) GB", value: $model.sizeGB, in: 1...2048)
                    Toggle(ui(.installIrixAfterCreating), isOn: $model.installAfter)
                }
                if model.disk == .image { ImageChoiceView(choice: model.image) }
                DisclosureGroup(Labels.NewMachine.advanced) {
                    TextField("Machine ID", text: $model.idText, prompt: Text("1 to 2999"))
                    LabeledContent(ui(.ethernetAddress), value: model.addressShown)
                    Toggle(ui(.useThisAddressInstead), isOn: $model.useAddress).disabled(model.identity?.fromId == false)
                    if model.useAddress { TextField(ui(.address), text: $model.addressText, prompt: Text("08:00:69:__:__:__")) }
                }
                if let p = model.identityProblem { Text(p).foregroundStyle(.red).font(.callout).fixedSize(horizontal: false, vertical: true) }
                if let p = model.progress { OperationProgress(progress: p) { model.cancelCopy() } }
                if let e = model.error {
                    Text(e).foregroundStyle(.red).font(.callout).fixedSize(horizontal: false, vertical: true)
                }
            }
            HStack {
                Spacer()
                Button(ui(.cancel)) { dismiss() }.keyboardShortcut(.cancelAction)
                Button(ui(.create)) { create() }.keyboardShortcut(.defaultAction).disabled(!model.canCreate)
            }
        }
        .padding(20).frame(width: 460)
        .onAppear { model.load() }
    }

    private func create() { model.create { ok in if ok { dismiss() } } }
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
