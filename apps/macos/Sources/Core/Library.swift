// The library as the views show it. The core lists the machines (a folder of
// machine directories) and says what each one is; this only adds the
// product's name and chassis colour.

import Foundation

/// Which SGI product a machine is, for its name and chassis colour.
struct Product {
    let name: String
    let kind: Kind
    /// Chassis colour as 0xRRGGBB. Identity, never state.
    var colour: UInt32
    /// The product still's id in the art manifest, when there is one.
    var art: String?
    enum Kind { case workstation, server }

    /// `label` is the core's product name; the model only picks the fallback colour.
    static func of(model: String, label: String) -> Product {
        let m = model.lowercased()
        // Fallback colours, used when the bundle has no stills manifest.
        if m.hasPrefix("octane2") { return Product(name: label, kind: .workstation, colour: 0x2e354e) }
        if m.hasPrefix("octane") { return Product(name: label, kind: .workstation, colour: 0x2a6574) }
        if m.hasPrefix("o2") { return Product(name: label, kind: .workstation, colour: 0x3d5a6d) }
        if m.hasPrefix("fuel") { return Product(name: label, kind: .workstation, colour: 0x8e3738) }
        if m.hasPrefix("tezro") { return Product(name: label, kind: .workstation, colour: 0x4c629c) }
        if m.hasPrefix("onyx") { return Product(name: label, kind: .server, colour: 0x5b6672) }
        if m.hasPrefix("origin350") { return Product(name: label, kind: .server, colour: 0xa2a4a6) }
        if m.hasPrefix("origin") { return Product(name: label, kind: .server, colour: 0x4a4f55) }
        return Product(name: label.isEmpty ? "Machine" : label, kind: .workstation, colour: 0x8a8f96)
    }
}

extension Product {
    /// The product for a catalogue offering, with its art by topology first.
    static func withArt(of o: OrigamiOffering) -> Product {
        var p = Product.of(model: o.product, label: o.product)
        if let id = Art.id(topology: o.topology, product: o.product) {
            p.art = id
            if let c = Art.colour(id) { p.colour = c }
        }
        return p
    }
}

struct MachineRecord: Identifiable, Hashable {
    /// The core's key for the machine; stable while it exists.
    let key: String
    let name: String
    let productName: String
    let productLabel: String
    let topology: String?
    let nodes: Int
    let memoryPerNodeMiB: Int
    let needsUpgrade: Bool
    /// What the system disk holds, such as "IRIX 6.5.30", once the core has read it.
    let systemDisk: String?
    /// Install IRIX is offered only to machines that run IRIX.
    let runsIrix: Bool

    var id: String { key }

    /// The summary cards, worded by the core. Other hardware belongs in Settings.
    var cards: [(symbol: String, title: String, value: String)] {
        coreCards.map { ($0.title == "Memory" ? "memorychip" : $0.title == "Nodes" ? "cpu" : "square.stack.3d.up", $0.title, $0.value) }
    }
    let coreCards: [OrigamiSummaryCard]
    var product: Product {
        var p = Product.of(model: productName, label: productLabel)
        if let id = Art.id(topology: topology, product: productName) {
            p.art = id
            if let c = Art.colour(id) { p.colour = c }
        }
        return p
    }

    var memory: String { coreCards.first { $0.title == "Memory" }?.value ?? "" }

    init(_ s: OrigamiMachineSummary) {
        key = s.key
        name = s.name
        productName = s.product
        productLabel = s.productLabel
        topology = s.topology
        coreCards = s.cards
        nodes = Int(s.nodes)
        memoryPerNodeMiB = Int(s.memoryPerNodeMib)
        needsUpgrade = s.needsUpgrade
        systemDisk = s.systemDisk
        runsIrix = s.runsIrix
    }
}

enum Library {
    /// Where machines live unless told otherwise.
    static func defaultRoot() -> URL {
        FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
            .appendingPathComponent("Origami/Machines", isDirectory: true)
    }
}
