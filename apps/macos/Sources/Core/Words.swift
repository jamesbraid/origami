// What the views take from the core's words: the platform the calls are made for, tone colours and the
// core's alerts as a native alert.

import AppKit
import SwiftUI

let thisPlatform = OrigamiPlatform.macos

extension OrigamiTone {
    /// The state's colour. It never stands without its word.
    var colour: Color {
        switch self {
        case .none: return Color(nsColor: .systemGray)
        case .success: return Color(nsColor: .systemGreen)
        case .caution: return Color(nsColor: .systemOrange)
        case .critical: return Color(nsColor: .systemRed)
        }
    }
}

@MainActor
enum Alerts {
    /// Show one of the core's alerts and return the index of the button pressed. Return takes the first button
    /// that is not destructive, so a stray key never does the harm; the destructive one is drawn as such.
    static func run(_ a: OrigamiAlert) -> Int { make(a).runModal().rawValue - NSApplication.ModalResponse.alertFirstButtonReturn.rawValue }

    /// The alert as a native one, not yet shown: the docs pictures show it without running it modally.
    static func make(_ a: OrigamiAlert) -> NSAlert {
        let alert = NSAlert()
        alert.messageText = a.title
        alert.informativeText = a.body
        for b in a.buttons { alert.addButton(withTitle: b.label) }
        let first = a.buttons.firstIndex { $0.role != .destructive } ?? 0
        for (i, b) in a.buttons.enumerated() {
            alert.buttons[i].hasDestructiveAction = b.role == .destructive
            alert.buttons[i].keyEquivalent = i == first ? "\r" : b.role == .cancel ? "\u{1b}" : ""
        }
        return alert
    }
}
