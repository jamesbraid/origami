// The New Machine sheet: a model, a name, Create and Cancel; errors stay on the sheet.

import SwiftUI

struct NewMachineSheet: View {
    @StateObject private var model: NewMachineModel
    @Environment(\.dismiss) private var dismiss

    init(app: AppModel) { self.init(model: NewMachineModel(app: app)) }
    init(model: NewMachineModel) { _model = StateObject(wrappedValue: model) }

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            Text("New Machine").font(.headline)
            if model.loading {
                ProgressView().controlSize(.small).frame(maxWidth: .infinity)
            } else {
                Picker("Engine", selection: $model.engine) {
                    ForEach(model.engines, id: \.self) { Text($0).tag(String?.some($0)) }
                }
                .disabled(!model.canPickEngine)
                Picker("Model", selection: $model.selected) {
                    ForEach(model.visible, id: \.self) { Text(model.label(model.offerings[$0])).tag($0) }
                }
                if model.offerings.indices.contains(model.selected) {
                    let o = model.offerings[model.selected]
                    let product = Product.of(model: o.product, label: o.product)
                    HStack { Spacer(); NewMachineCard(product: Product.withArt(of: o)) ; Spacer() }
                        .accessibilityHidden(true).id(product.name + o.topology)
                }
                TextField("Name", text: $model.name).onSubmit { create() }
                if let e = model.error {
                    Text(e).foregroundStyle(.red).font(.callout).fixedSize(horizontal: false, vertical: true)
                }
            }
            HStack {
                Spacer()
                Button("Cancel") { dismiss() }.keyboardShortcut(.cancelAction)
                Button("Create") { create() }.keyboardShortcut(.defaultAction).disabled(!model.canCreate)
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
