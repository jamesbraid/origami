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

struct MachineRecord: Identifiable, Hashable {
    /// The core's key for the machine; stable while it exists.
    let key: String
    let name: String
    let productName: String
    let productLabel: String
    let topology: String?
    let nodes: Int
    let memoryPerNodeMiB: Int

    var id: String { key }

    /// The summary cards: always Memory, Nodes and Topology. Other hardware belongs in Settings.
    var cards: [(symbol: String, title: String, value: String)] {
        [("memorychip", "Memory", memory), ("cpu", "Nodes", "\(nodes)"), ("square.stack.3d.up", "Topology", topology ?? "Unknown")]
    }
    var product: Product {
        var p = Product.of(model: productName, label: productLabel)
        if let id = Art.id(topology: topology, product: productName) {
            p.art = id
            if let c = Art.colour(id) { p.colour = c }
        }
        return p
    }

    var memory: String {
        let total = memoryPerNodeMiB * max(1, nodes)
        return total >= 1024 && total % 1024 == 0 ? "\(total / 1024) GB" : "\(total) MB"
    }

    init(_ s: OrigamiMachineSummary) {
        key = s.key
        name = s.name
        productName = s.product
        productLabel = s.productLabel
        topology = s.topology
        nodes = Int(s.nodes)
        memoryPerNodeMiB = Int(s.memoryPerNodeMib)
    }
}

enum Library {
    // Stopgap: the core's own left-running state and exit reasons replace this lock check once they land.
    /// Whether a process holds the machine's lock. A runner holds it for the machine's whole life, so
    /// this answers "is it up" without the core, and sees machines started by another core or session.
    static func runnerHolds(_ dir: String) -> Bool { probe(dir) == true }

    /// True when the runner ended: the lock exists and nothing holds it, or the run record names a
    /// runner or QEMU process that no longer exists. No lock file (an engine without a runner, or a
    /// start still under way) is not "gone".
    static func runnerGone(_ dir: String) -> Bool {
        if probe(dir) == false { return true }
        guard let data = FileManager.default.contents(atPath: dir + "/state/run.json"),
              let record = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else { return false }
        return ["pid", "qemu_pid"].contains { key in
            guard let pid = (record[key] as? NSNumber)?.int32Value, pid > 0 else { return false }
            return kill(pid, 0) != 0 && errno == ESRCH
        }
    }

    private static func probe(_ dir: String) -> Bool? {
        let fd = open(dir + "/state/machine.lock", O_RDONLY)
        if fd < 0 { return nil }
        defer { close(fd) }
        if flock(fd, LOCK_EX | LOCK_NB) == 0 { flock(fd, LOCK_UN); return false }
        return true
    }

    /// Where machines live unless told otherwise.
    static func defaultRoot() -> URL {
        FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
            .appendingPathComponent("Origami/Machines", isDirectory: true)
    }
}
