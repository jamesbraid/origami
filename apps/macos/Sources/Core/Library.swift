// The library as the views show it. The core lists the machines (a folder of
// machine directories) and says what each one is; this only adds the
// product's name and chassis colour.

import Foundation

/// Which SGI product a machine is, for its name and chassis colour.
struct Product {
    let name: String
    let kind: Kind
    /// Chassis colour as 0xRRGGBB. Identity, never state.
    let colour: UInt32
    enum Kind { case workstation, server }

    static func of(model: String) -> Product {
        let m = model.lowercased()
        if m.hasPrefix("octane2") { return Product(name: "Octane2", kind: .workstation, colour: 0x2f6fb7) }
        if m.hasPrefix("octane") { return Product(name: "Octane", kind: .workstation, colour: 0x1f9d9a) }
        if m.hasPrefix("o2") { return Product(name: "O2", kind: .workstation, colour: 0x6c5bc4) }
        if m.hasPrefix("fuel") { return Product(name: "Fuel", kind: .workstation, colour: 0xc2412d) }
        if m.hasPrefix("tezro") { return Product(name: "Tezro", kind: .workstation, colour: 0x7b3fa6) }
        if m.hasPrefix("onyx") { return Product(name: "Onyx", kind: .server, colour: 0x5b6672) }
        if m.hasPrefix("origin") { return Product(name: "Origin", kind: .server, colour: 0x4a4f55) }
        return Product(name: model.isEmpty ? "Machine" : model, kind: .workstation, colour: 0x8a8f96)
    }
}

struct MachineRecord: Identifiable, Hashable {
    /// The core's key for the machine; stable while it exists.
    let key: String
    let name: String
    let productName: String
    let topology: String?
    let nodes: Int
    let memoryPerNodeMiB: Int

    var id: String { key }
    var product: Product { Product.of(model: productName) }

    var memory: String {
        let total = memoryPerNodeMiB * max(1, nodes)
        return total >= 1024 && total % 1024 == 0 ? "\(total / 1024) GB" : "\(total) MB"
    }

    init(_ s: OrigamiMachineSummary) {
        key = s.key
        name = s.name
        productName = s.product
        topology = s.topology
        nodes = Int(s.nodes)
        memoryPerNodeMiB = Int(s.memoryPerNodeMib)
    }
}

enum Library {
    /// Where machines live unless told otherwise.
    static func defaultRoot() -> URL {
        FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
            .appendingPathComponent("Origami/Machines", isDirectory: true)
    }
}
