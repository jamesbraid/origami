// Helpers for the headless self-test: JSON lines out and run-loop waits.

import AppKit
import Foundation
import OrigamiFrame

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
            name: name, offering: "fake.1", memoryPerNodeMib: nil, graphics: nil, bootProm: prom, ioProm: nil, engine: nil, systemDisk: nil, identity: nil))
        return op.wait(timeoutMs: 10_000) == .succeeded ? nil : NSError(domain: "fake", code: 1)
    } catch { return error }
}
#endif

/// Writes tight BGRA rows as a PNG, as evidence of what the core drew (a window capture cannot see Metal).
func writePNG(_ frame: (width: Int, height: Int, bgra: [UInt8]), to path: String) {
    var rgba = frame.bgra
    for i in stride(from: 0, to: rgba.count, by: 4) { rgba.swapAt(i, i + 2) }
    guard let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: frame.width, pixelsHigh: frame.height, bitsPerSample: 8,
                                     samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB,
                                     bytesPerRow: frame.width * 4, bitsPerPixel: 32),
          let dst = rep.bitmapData else { return }
    rgba.withUnsafeBufferPointer { dst.update(from: $0.baseAddress!, count: rgba.count) }
    try? rep.representation(using: .png, properties: [:])?.write(to: URL(fileURLWithPath: path))
}

/// A cube with no window, drawn once at rest in a scene, over nothing: its pixels and its size. For measuring the renderer.
func offscreenCubeFrame(_ w: UInt32, _ h: UInt32, scale: Double, scene: UInt32) -> (width: Int, height: Int, bgra: [UInt8])? {
    var made: OpaquePointer?
    guard origami_cube_attach_offscreen(w, h, scale, &made) == 0, let cube = made else { return nil }
    defer { origami_cube_detach(cube) }
    var s = origami_cube_style()
    s.size = UInt32(MemoryLayout<origami_cube_style>.size)
    s.env = UInt32(ORIGAMI_CUBE_ENV_SCENE); s.scene = scene
    s.horizon_rgb = UInt32.max
    s.granite_light_rgb = 0x282a30; s.granite_dark_rgb = 0x1f2126
    s.backdrop = UInt32(ORIGAMI_CUBE_BACKDROP_NONE)
    guard origami_cube_set_style(cube, &s) == 0 else { return nil }
    var drawn: UInt32 = 0, moving: UInt32 = 0
    guard origami_cube_present(cube, &drawn, &moving) == 0 else { return nil }
    var rw: UInt32 = 0, rh: UInt32 = 0, stride: UInt32 = 0
    _ = origami_cube_readback(cube, nil, 0, &rw, &rh, &stride)
    guard rw > 0, rh > 0 else { return nil }
    var buf = [UInt8](repeating: 0, count: Int(stride) * Int(rh))
    guard origami_cube_readback(cube, &buf, buf.count, &rw, &rh, &stride) == 0 else { return nil }
    return (Int(rw), Int(rh), buf)
}

/// How rough the cube's edges are: the distinct partial-coverage alpha levels on its silhouette (more is smoother), and the mean
/// error in 0 to 255 of a `side` px render against one at 4x box-filtered down (less is smoother).
func edgeQuality(side: Int, scene: UInt32) -> (levels: Int, error: Double)? {
    guard let one = offscreenCubeFrame(UInt32(side), UInt32(side), scale: 1, scene: scene),
          let big = offscreenCubeFrame(UInt32(side * 4), UInt32(side * 4), scale: 4, scene: scene) else { return nil }
    var levels = Set<UInt8>()
    for i in stride(from: 3, to: one.bgra.count, by: 4) where one.bgra[i] > 0 && one.bgra[i] < 255 { levels.insert(one.bgra[i]) }
    var sum = 0
    for y in 0..<side { for x in 0..<side { for c in 0..<4 {
        var acc = 0
        for k in 0..<16 { acc += Int(big.bgra[((y * 4 + k / 4) * side * 4 + x * 4 + k % 4) * 4 + c]) }
        sum += abs(Int(one.bgra[(y * side + x) * 4 + c]) - Int((Double(acc) / 16).rounded()))
    } } }
    return (levels.count, Double(sum) / Double(side * side * 4))
}
