// Helpers for the headless self-test: JSON lines out and run-loop waits.

import AppKit
import Foundation

func emit(_ d: [String: Any]) {
    let data = try! JSONSerialization.data(withJSONObject: d, options: [.sortedKeys])
    print(String(data: data, encoding: .utf8)!)
    fflush(stdout)
}

func spin(_ seconds: Double) { RunLoop.main.run(until: Date().addingTimeInterval(seconds)) }

func waitFor(_ timeout: Double, _ cond: () -> Bool) -> Bool {
    let end = Date().addingTimeInterval(timeout)
    while Date() < end { if cond() { return true }; spin(0.02) }
    return cond()
}

func processCPU() -> (user: Double, sys: Double) {
    var r = rusage(); getrusage(RUSAGE_SELF, &r)
    func s(_ t: timeval) -> Double { Double(t.tv_sec) + Double(t.tv_usec) / 1e6 }
    return (s(r.ru_utime), s(r.ru_stime))
}

struct Picture { var w: Int; var h: Int; var rgb: [UInt8] }

/// A binary PPM (P6, 255), as QEMU's screendump writes.
func readPPM(_ path: String) -> Picture? {
    guard let d = FileManager.default.contents(atPath: path) else { return nil }
    var i = 0
    func token() -> String {
        while i < d.count, d[i] == 0x20 || d[i] == 0x0a { i += 1 }
        var s = ""
        while i < d.count, d[i] != 0x20, d[i] != 0x0a { s.append(Character(UnicodeScalar(d[i]))); i += 1 }
        return s
    }
    guard token() == "P6", let w = Int(token()), let h = Int(token()), token() == "255" else { return nil }
    i += 1
    guard d.count >= i + w * h * 3 else { return nil }
    return Picture(w: w, h: h, rgb: Array(d[i..<(i + w * h * 3)]))
}

#if FAKE
/// A machine on the fake engine, with its stand-in firmware file in `library`; the error if the core refuses it.
func makeFakeMachine(_ host: CoreHost, library: String, name: String) -> Error? {
    let prom = library + "/prom.bin"
    FileManager.default.createFile(atPath: prom, contents: Data(count: 16))
    do {
        let op = try host.core.createMachine(request: OrigamiCreateRequest(
            name: name, offering: "fake.1", memoryPerNodeMib: nil, graphics: nil, bootProm: prom, ioProm: nil, engine: nil))
        return op.wait(timeoutMs: 10_000) == .succeeded ? nil : NSError(domain: "fake", code: 1)
    } catch { return error }
}
#endif
