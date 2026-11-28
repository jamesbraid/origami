// `OrigamiApp --selftest-scene OUTDIR --machines DIR`: the app's own window scene, started through `open` (see ci.sh).
// The other self-tests put MainView in a bare NSWindow, which the system gives no sidebar button, so only this one
// can see a second one. The docs pictures of the main window start the same way (DocsShots.swift).

import AppKit
import Foundation

/// The app's main scene window: the one whose toolbar has the library button. Hosted test windows have no toolbar.
@MainActor
func sceneMainWindow() -> NSWindow? {
    NSApp.windows.first { $0.toolbar?.items.contains { $0.label == ui(.showOrHideTheLibrary) } == true }
}

@MainActor
func scheduleSceneSelfTest(out: String) {
    let start = Date()
    func finish(_ ok: Bool, _ extra: [String: Any] = [:]) -> Never {
        if !extra.isEmpty { emit(["step": "scene-has-one-sidebar-button", "ok": ok].merging(extra) { a, _ in a }) }
        emit(["step": "done", "ok": ok])
        exit(ok ? 0 : 1)
    }
    func poll() {
        let age = Date().timeIntervalSince(start)
        if let window = sceneMainWindow(), age > 3 {
            let items = window.toolbar?.items ?? []
            if let frame = window.contentView?.superview, let rep = frame.bitmapImageRepForCachingDisplay(in: frame.bounds) {
                frame.cacheDisplay(in: frame.bounds, to: rep)
                try? rep.representation(using: .png, properties: [:])?.write(to: URL(fileURLWithPath: "\(out)/scene-main-window.png"))
            }
            // The sidebar column asks for 200 to 340 points; a modifier out of order brings it up at 140.
            func splits(_ v: NSView) -> [NSSplitView] { ((v as? NSSplitView).map { [$0] } ?? []) + v.subviews.flatMap(splits) }
            let width = window.contentView.flatMap(splits)?.first?.arrangedSubviews.first?.frame.width ?? 0
            emit(["step": "scene-sidebar-keeps-its-width", "ok": width >= 200, "width": Double(width)])
            let toggles = items.filter { $0.itemIdentifier.rawValue.localizedCaseInsensitiveContains("togglesidebar") }
            let ours = items.filter { $0.label == ui(.showOrHideTheLibrary) }
            let single = toggles.isEmpty && ours.count == 1
            emit(["step": "scene-has-one-sidebar-button", "ok": single, "items": items.map { "\($0.itemIdentifier.rawValue)|\($0.label)" }])
            finish(single && width >= 200)
        }
        if age > 60 { finish(false, ["error": "the main window never showed its toolbar"]) }
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.5) { MainActor.assumeIsolated { poll() } }
    }
    DispatchQueue.main.async { MainActor.assumeIsolated { poll() } }
}
