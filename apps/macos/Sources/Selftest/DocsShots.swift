// `OrigamiApp --docs-shots LIBRARY OUTDIR`: the pictures docs/app.md shows, as 2x bitmaps of the views, none on screen.
// A view bitmap leaves out the machine's Metal picture, so the library shot is the stopped machine.

import AppKit
import Foundation
import OrigamiFrame
import SwiftUI

@MainActor
func runDocsShots(library: String, out: String) {
    func fail(_ why: String) -> Never {
        FileHandle.standardError.write(Data("docs-shots: \(why)\n".utf8))
        exit(1)
    }
    do { try FileManager.default.createDirectory(atPath: out, withIntermediateDirectories: true) } catch { fail("cannot create \(out): \(error)") }
    let app = AppModel(root: URL(fileURLWithPath: library))
    guard let host = app.host else { fail(app.launchError ?? "the core did not start") }

    #if FAKE
    if let e = makeFakeMachine(host, library: library, name: "Octane2") { fail("creating the machine failed: \(e)") }
    app.rescanNow()
    #endif
    guard let model = app.models.first else { fail("no machine in \(library)") }
    app.select(model.id)

    func shot<V: View>(_ name: String, _ view: V, _ w: CGFloat, _ h: CGFloat) {
        guard snapshotRetina(view.environmentObject(app), size: CGSize(width: w, height: h), dark: false, to: "\(out)/\(name).png") else {
            fail("could not write \(name).png")
        }
    }

    // The library with the machine stopped, as it opens.
    shot("macos-library", MainView(), 1000, 640)

    // The New Machine sheet, once the catalogue has loaded.
    let nm = NewMachineModel(app: app)
    nm.load()
    guard waitFor(60, { !nm.loading }) else { fail("the New Machine catalogue did not load") }
    shot("macos-new-machine", NewMachineSheet(model: nm), 460, 420)

    // Start it, so Diagnostics and the CPU panel have something to report.
    var started = false
    model.start { _ in started = true }
    guard waitFor(180, { started }) else { fail("the machine did not start") }
    #if FAKE
    try? host.core.fakeAdvanceFrame(key: model.record.key)
    #endif
    guard waitFor(120, { model.state == .running }) else { fail("the machine did not reach Running (\(model.state.pill))") }
    spin(2)
    shot("macos-diagnostics", DiagnosticsView(), 640, 340)

    let load = CpuLoadModel()
    load.start(model)
    guard waitFor(20, { !(load.report?.cpus.isEmpty ?? true) }) else { fail("no CPU load report") }
    shot("macos-cpu-panel", CpuPanel(model: load), 640, 90)
    load.stop()

    shot("macos-about", AboutView(), 360, 420)

    var stopped = false
    app.stopAll { _ in stopped = true }
    _ = waitFor(120) { stopped }
    emit(["step": "docs-shots", "ok": true, "out": out])
    exit(0)
}
