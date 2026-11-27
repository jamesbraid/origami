// `OrigamiApp --selftest-scene OUTDIR --machines DIR`: the app's own window scene, started through `open` (see ci.sh).
// The other self-tests put MainView in a bare NSWindow, which the system gives no sidebar button, so only this one
// can see a second one.

import AppKit
import Foundation

@MainActor
func scheduleSceneSelfTest(out: String) {
    let start = Date()
    func finish(_ ok: Bool, _ extra: [String: Any]) -> Never {
        emit(["step": "scene-has-one-sidebar-button", "ok": ok].merging(extra) { a, _ in a })
        emit(["step": "done", "ok": ok])
        exit(ok ? 0 : 1)
    }
    func poll() {
        let age = Date().timeIntervalSince(start)
        if let window = NSApp.windows.first(where: { $0.toolbar?.items.contains { $0.label == ui(.showOrHideTheLibrary) } == true }), age > 3 {
            let items = window.toolbar?.items ?? []
            if let frame = window.contentView?.superview, let rep = frame.bitmapImageRepForCachingDisplay(in: frame.bounds) {
                frame.cacheDisplay(in: frame.bounds, to: rep)
                try? rep.representation(using: .png, properties: [:])?.write(to: URL(fileURLWithPath: "\(out)/scene-main-window.png"))
            }
            let toggles = items.filter { $0.itemIdentifier.rawValue.localizedCaseInsensitiveContains("togglesidebar") }
            let ours = items.filter { $0.label == ui(.showOrHideTheLibrary) }
            finish(toggles.isEmpty && ours.count == 1, ["items": items.map { "\($0.itemIdentifier.rawValue)|\($0.label)" }])
        }
        if age > 60 { finish(false, ["error": "the main window never showed its toolbar"]) }
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.5) { MainActor.assumeIsolated { poll() } }
    }
    DispatchQueue.main.async { MainActor.assumeIsolated { poll() } }
}
