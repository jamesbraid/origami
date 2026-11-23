// Names all three apps show in the same places. The docs-shots run writes them to JSON so a check can compare the apps.

import Foundation

enum Labels {
    static let sidebarHeading = "Machines"

    /// The New Machine fields in the order they appear; the IRIX release row only follows a ready-made disk.
    enum NewMachine {
        static let engine = "Engine", model = "Model", name = "Name", disk = "Disk", advanced = "Advanced"
        static var release: String { origamiDiskLabels().release }
        static let fields = [engine, model, name, disk, advanced]
    }
}
