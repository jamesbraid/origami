// Lists machines, creates and starts one on the fake engine, receives an
// event, and reads a frame through the C frame plane. Exits non-zero on the
// first failure.
import Foundation
import OrigamiFrame

final class Recorder: OrigamiObserver, @unchecked Sendable {
    private let condition = NSCondition()
    private var events: [OrigamiEvent] = []

    func onEvent(event: OrigamiEvent) {
        condition.lock()
        events.append(event)
        condition.broadcast()
        condition.unlock()
    }

    func wait(timeout: TimeInterval = 10, _ match: (OrigamiEvent) -> Bool) -> Bool {
        let deadline = Date(timeIntervalSinceNow: timeout)
        condition.lock()
        defer { condition.unlock() }
        while !events.contains(where: match) {
            if !condition.wait(until: deadline) { return false }
        }
        return true
    }
}

func expect(_ condition: Bool, _ what: String, line: Int = #line) {
    if !condition {
        FileHandle.standardError.write("main.swift:\(line): expected \(what)\n".data(using: .utf8)!)
        exit(1)
    }
}

let root = NSTemporaryDirectory() + "origami-swift-test-\(getpid())"
try FileManager.default.createDirectory(atPath: root, withIntermediateDirectories: true)
let prom = root + "/prom.bin"
FileManager.default.createFile(atPath: prom, contents: Data(count: 16))

let recorder = Recorder()
let core = try OrigamiCore(libraryRoot: root, observer: recorder)
expect(try core.listMachines().isEmpty, "an empty library")
expect(core.engines().first?.id == "fake", "the fake engine")
expect(core.commands().contains { $0.id == "machine.start" }, "a Start command")

let create = try core.createMachine(request: OrigamiCreateRequest(
    name: "alpha", offering: "fake.1", memoryPerNodeMib: nil, graphics: nil, bootProm: prom, ioProm: nil, engine: nil, systemDisk: .empty(gb: 1), identity: nil))
expect(create.wait(timeoutMs: 10_000) == .succeeded, "the machine is created")
let machines = try core.listMachines()
print("machines:", machines.map { $0.name })
expect(machines.count == 1 && machines[0].name == "alpha", "alpha in the list")

let key = machines[0].key
let stopped = try core.openMachine(key: key)
try stopped.applyChange(change: .setConsole(port: "ttyS1"))
_ = try stopped.setNetwork(mode: .tap, device: "tap0", shareAddress: nil, subnet: nil)
expect(try stopped.settings().network.device == "tap0", "the tap device kept")
expect(origamiHostNetworkModes().count == 4, "four network modes")
expect(origamiSuggestShareName(dir: "/x/My Code") == "My-Code", "a share name from a folder")
expect(try stopped.settings().consolePort == "ttyS1", "the console line chosen")
expect(try stopped.settings().diskTarget == 2, "an empty system disk at target 1")
expect(try stopped.driveInfo(drive: "system") == "Unknown", "a blank disk is Unknown")
let machine = try core.startMachine(key: key)
expect(machine.view().state == .running, "a running machine")
expect(recorder.wait { $0.machine == key && $0.kinds.contains(.state) }, "a state event")

try core.fakeAdvanceFrame(key: key)
expect(recorder.wait { $0.machine == key && $0.kinds.contains(.display) }, "a display event")

// The frame plane: the control plane hands over an owned handle.
let handle = OpaquePointer(bitPattern: UInt(machine.framePlaneHandle()))!
var head = origami_head_info()
var heads: Int = 0
expect(origami_frame_heads(handle, &head, 1, &heads) == 0, "heads")
expect(heads == 1 && head.width == 64 && head.height == 48, "one 64x48 head")
origami_machine_release(handle)

do {
    _ = try core.openMachine(key: "no-such-machine")
    expect(false, "NotFound for an unknown machine")
} catch CoreError.NotFound {
    // expected
}
// An older machine file is listed as needing an upgrade, and upgrading says so.
try FileManager.default.createDirectory(atPath: root + "/old", withIntermediateDirectories: true)
try "format = 4\n[machine]\nmodel = \"origin200\"\ntopology = \"origin200\"\npopulation = [1]\nnodes = 1\ncpus_per_node = 1\nmemory_per_node = \"256MiB\"\n"
    .write(toFile: root + "/old/machine.toml", atomically: true, encoding: .utf8)
let old = try core.listMachines().first { $0.name == "old" }!
expect(old.needsUpgrade, "the older machine needs an upgrade")
expect(try core.upgradeMachine(key: old.key).contains { $0.kind == .result }, "the upgrade reports")
core.shutdown()
print("ok")
